use aws_sdk_ec2::primitives::DateTimeFormat;
use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
    routing::{MethodRouter, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedMutexGuard;
use tracing::Instrument;

use crate::{
    AppState,
    auth::Client,
    bootstrap::{self, File, Payload, Store},
    ec2::{self, Action, Instance, Launch},
    error::AppError,
    registry::{self, NewVm, Record},
};

/// The largest body of `POST /v1/vms`. A larger one gives 413.
pub const MAX_BODY: usize = 256 * 1024;

#[derive(Serialize)]
pub struct InstanceType {
    name: String,
    vcpus: i32,
    memory_mib: i64,
    sev_snp: bool,
    default: bool,
}

pub async fn instance_types(
    State(state): State<AppState>,
) -> Result<Json<Vec<InstanceType>>, AppError> {
    let specs = (state.types)
        .get_or_try_init(|| state.ec2.instance_types(&state.vm.instance_types))
        .await?;
    // The list keeps the order of INSTANCE_TYPES.
    let types = (state.vm.instance_types.iter())
        .filter_map(|name| specs.iter().find(|spec| spec.name == *name))
        .map(|spec| InstanceType {
            name: spec.name.clone(),
            vcpus: spec.vcpus,
            memory_mib: spec.memory_mib,
            sev_snp: ec2::sev_snp(&spec.name),
            default: spec.name == state.vm.default_instance_type,
        })
        .collect();
    Ok(Json(types))
}

/// The VM object of the API.
#[derive(Serialize)]
pub struct Vm {
    id: String,
    name: String,
    state: String,
    instance_type: String,
    sev_snp: bool,
    public_ip: Option<String>,
    launched_at: String,
    setup: &'static str,
    auto_stop_hours: Option<u32>,
    creator: Option<String>,
    /// The VM has a registry row: the API can change its hours.
    registered: bool,
}

impl Vm {
    pub fn new(instance: &Instance, store: &Store, record: Option<&Record>) -> Self {
        let setup = if instance.setup_delivered {
            "delivered"
        } else if store.has_live(&instance.id) {
            "pending"
        } else {
            "expired"
        };
        Self {
            id: instance.id.clone(),
            name: instance.name.clone(),
            state: instance.state.clone(),
            instance_type: instance.instance_type.clone(),
            sev_snp: ec2::sev_snp(&instance.instance_type),
            public_ip: instance.public_ip.clone(),
            launched_at: (instance.launched_at.fmt(DateTimeFormat::DateTime)).unwrap_or_default(),
            setup,
            auto_stop_hours: record
                .map(|r| r.auto_stop_hours)
                .or(instance.auto_stop_hours),
            creator: record.and_then(|r| r.creator.clone()),
            registered: record.is_some(),
        }
    }
}

/// Enforces tenant isolation, hiding VMs from other tenants, unmanaged VMs, and terminated VMs.
pub fn visible(instance: &Instance, tenant: &str) -> bool {
    instance.tenant.as_deref() == Some(tenant) && instance.state != "terminated"
}

pub async fn list(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
) -> Result<Json<Vec<Vm>>, AppError> {
    let instances = state.ec2.list(&client.name).await?;
    let records = registry::of_tenant(&state.db, &client.name).await?;
    let mut vms: Vec<Vm> = (instances.iter())
        .filter(|instance| visible(instance, &client.name))
        .map(|instance| Vm::new(instance, &state.bootstrap, records.get(&instance.id)))
        .collect();
    vms.sort_by(|a, b| (&a.name, &a.id).cmp(&(&b.name, &b.id)));
    Ok(Json(vms))
}

/// `^i-[0-9a-f]{8,17}$`
fn valid_id(id: &str) -> bool {
    id.strip_prefix("i-").is_some_and(|hex| {
        (8..=17).contains(&hex.len()) && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// An id that does not decode, such as `%FF`, is a bad id like any other: a `404`, not the
/// plain-text `400` of the extractor.
fn id_of(id: Result<Path<String>, PathRejection>) -> Result<String, AppError> {
    id.map(|Path(id)| id).map_err(|_| AppError::NotFound)
}

/// The VM, if it exists and the tenant owns it. Every other case is "not found", so a
/// response never shows that a VM of another tenant exists.
async fn find(state: &AppState, tenant: &str, id: &str) -> Result<Instance, AppError> {
    if !valid_id(id) {
        return Err(AppError::NotFound);
    }
    (state.ec2.get(id).await?)
        .filter(|instance| visible(instance, tenant))
        .ok_or(AppError::NotFound)
}

/// `POST` route that applies `action` to the VM of the path.
pub fn action(action: Action) -> MethodRouter<AppState> {
    post(
        move |state: State<AppState>,
              client: Extension<Client>,
              id: Result<Path<String>, PathRejection>,
              body: Bytes| apply(action, state, client, id, body),
    )
}

async fn apply(
    action: Action,
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<(StatusCode, Json<Vm>), AppError> {
    let id = id_of(id)?;
    // Only `start` reads a body: `{"auto_stop_hours": N}`. An empty body keeps the hours.
    let hours = if action == Action::Start && !body.is_empty() {
        let request: StartVm = serde_json::from_slice(&body)
            .map_err(|_| AppError::BadRequest("invalid request body".to_owned()))?;
        valid_hours(request.auto_stop_hours)?
    } else {
        None
    };
    let mut vm = find(&state, &client.name, &id).await?;
    if action.allowed_from().is_some_and(|from| from != vm.state) {
        return Err(AppError::Conflict(format!("vm is {}", vm.state)));
    }
    if let Some(hours) = hours {
        // A VM without a row reads only its tag, which the API cannot change: the hours do not
        // apply to it, and the start goes on.
        registry::set_hours(&state.db, &client.name, &vm.id, hours).await?;
    }
    state.ec2.act(&vm.id, action).await?;
    tracing::info!(tenant = %client.name, vm = %vm.id, ?action, "vm action");
    // EC2 still reports the old state for a moment, so show the state that the action causes.
    vm.state = action.next_state().to_owned();
    let record = registry::get(&state.db, &client.name, &vm.id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Vm::new(&vm, &state.bootstrap, record.as_ref())),
    ))
}

#[derive(Deserialize)]
struct StartVm {
    auto_stop_hours: Option<i64>,
}

/// The default of `auto_stop_hours` at create.
const DEFAULT_AUTO_STOP_HOURS: u32 = 12;

/// `auto_stop_hours` must be 1 to 168. `None` stays `None`.
fn valid_hours(hours: Option<i64>) -> Result<Option<u32>, AppError> {
    hours.map(checked_hours).transpose()
}

fn checked_hours(hours: i64) -> Result<u32, AppError> {
    (u32::try_from(hours).ok())
        .filter(|h| (1..=168).contains(h))
        .ok_or_else(|| AppError::BadRequest("auto_stop_hours must be 1 to 168".to_owned()))
}

#[derive(Deserialize)]
struct SetAutoStop {
    auto_stop_hours: i64,
}

/// `PUT /v1/vms/{id}/auto-stop`: the VM reads the new hours on its next check.
pub async fn set_auto_stop(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<Json<Vm>, AppError> {
    let id = id_of(id)?;
    let request: SetAutoStop = serde_json::from_slice(&body)
        .map_err(|_| AppError::BadRequest("invalid request body".to_owned()))?;
    let hours = checked_hours(request.auto_stop_hours)?;
    let vm = find(&state, &client.name, &id).await?;
    if !registry::set_hours(&state.db, &client.name, &vm.id, hours).await? {
        let message = "recreate the vm to change its auto-stop";
        return Err(AppError::Conflict(message.to_owned()));
    }
    tracing::info!(tenant = %client.name, vm = %vm.id, hours, "auto-stop set");
    let record = registry::get(&state.db, &client.name, &vm.id).await?;
    Ok(Json(Vm::new(&vm, &state.bootstrap, record.as_ref())))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, AppError> {
    let id = id_of(id)?;
    let vm = find(&state, &client.name, &id).await?;
    state.ec2.act(&vm.id, Action::Terminate).await?;
    // The VM is gone, so nobody can redeem its payload. A failed terminate keeps the entry.
    state.bootstrap.remove_instance(&vm.id);
    // The VM is terminated already: a failed delete only leaves a token that reads hours.
    if let Err(e) = registry::remove(&state.db, &vm.id).await {
        tracing::error!(vm = %vm.id, error = %e, "could not drop the registry row");
    }
    tracing::info!(tenant = %client.name, vm = %vm.id, "vm deleted");
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct CreateVm {
    name: String,
    instance_type: Option<String>,
    bootstrap: Payload,
    auto_stop_hours: Option<i64>,
    /// The Matrix user who asked for the VM.
    creator: Option<String>,
}

/// `@localpart:server` in printable ASCII, at most 255 bytes (the Matrix limit).
fn valid_creator(id: &str) -> bool {
    id.len() <= 255
        && id.bytes().all(|b| b.is_ascii_graphic())
        && (id.strip_prefix('@'))
            .and_then(|rest| rest.split_once(':'))
            .is_some_and(|(local, server)| !local.is_empty() && !server.is_empty())
}

/// `^[a-z0-9][a-z0-9-]{0,62}$`
fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=63).contains(&b.len())
        && b[0] != b'-'
        && b.iter()
            .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    body: Bytes,
) -> Result<(StatusCode, Json<Vm>), AppError> {
    // A fixed message: a serde error can echo a value of the payload.
    let request: CreateVm = serde_json::from_slice(&body)
        .map_err(|_| AppError::BadRequest("invalid request body".to_owned()))?;
    if !valid_name(&request.name) {
        let message = "name must match ^[a-z0-9][a-z0-9-]{0,62}$";
        return Err(AppError::BadRequest(message.to_owned()));
    }
    let instance_type =
        (request.instance_type.as_deref()).unwrap_or(&state.vm.default_instance_type);
    if !state.vm.instance_types.iter().any(|t| t == instance_type) {
        return Err(AppError::BadRequest(
            "instance_type is not allowed".to_owned(),
        ));
    }
    request
        .bootstrap
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_owned()))?;
    let auto_stop_hours = valid_hours(request.auto_stop_hours)?.unwrap_or(DEFAULT_AUTO_STOP_HOURS);
    if (request.creator.as_deref()).is_some_and(|c| !valid_creator(c)) {
        let message = "creator must be a Matrix user id";
        return Err(AppError::BadRequest(message.to_owned()));
    }

    // ponytail: one create at a time (README, "Create a VM"); a lock per tenant and name scales.
    let guard = state.create_lock.clone().lock_owned().await;
    let tenant = client.name.clone();
    let instance_type = instance_type.to_owned();
    // A task of its own: the 10 s request timeout must not split the launch from the payload.
    let task = tokio::spawn(
        async move {
            launch_vm(
                &state,
                &tenant,
                request,
                &instance_type,
                auto_stop_hours,
                guard,
            )
            .await
        }
        .in_current_span(),
    );
    let vm = task
        .await
        .map_err(|e| AppError::Aws(format!("create task failed: {e}")))??;
    Ok((StatusCode::CREATED, Json(vm)))
}

/// The part of a create that must not be cut in two: the name check, the launch, and the
/// insert of the payload. It holds the create lock until the payload is in the store.
async fn launch_vm(
    state: &AppState,
    tenant: &str,
    request: CreateVm,
    instance_type: &str,
    auto_stop_hours: u32,
    guard: OwnedMutexGuard<()>,
) -> Result<Vm, AppError> {
    // EC2 lists a new VM late, so a name that the payload store holds counts as in use too.
    let listed = (state.ec2.list(tenant).await?)
        .iter()
        .any(|i| visible(i, tenant) && i.name == request.name);
    if listed || state.bootstrap.name_in_use(tenant, &request.name) {
        return Err(AppError::Conflict("name in use".to_owned()));
    }

    let token = bootstrap::new_token();
    // The VM reads its auto-stop hours with this token. Root-only: the agent runs as ubuntu.
    let vm_token = bootstrap::new_token();
    let mut payload = request.bootstrap;
    let file = |path: &str, mode: &str, content: &str| File {
        path: path.to_owned(),
        mode: mode.to_owned(),
        owner: "root".to_owned(),
        content: content.to_owned(),
    };
    payload
        .files
        .push(file("/etc/fabrium/vm-token", "600", &vm_token));
    let url = &state.vm.public_url;
    payload
        .files
        .push(file("/etc/fabrium/control-plane-url", "644", url));
    let launch = Launch {
        name: request.name,
        tenant: tenant.to_owned(),
        instance_type: instance_type.to_owned(),
        auto_stop_hours,
        user_data: STANDARD.encode(bootstrap::stub(&state.vm.public_url, &token)),
    };
    let instance = state.ec2.launch(&launch).await?;
    // No await between the launch and the insert: the VM cannot redeem before the entry exists.
    let name = &instance.name;
    (state.bootstrap).insert(&token, payload, &instance.id, tenant, name);
    drop(guard);
    tracing::info!(%tenant, vm = %instance.id, %instance_type, "vm launched");
    let new = NewVm {
        instance_id: &instance.id,
        tenant,
        name,
        creator: request.creator.as_deref(),
        auto_stop_hours,
        token: &vm_token,
    };
    // The VM runs already. Without a row it keeps the hours of its tag, so only log.
    let record = match registry::insert(&state.db, &new).await {
        Ok(()) => Some(Record {
            creator: request.creator,
            auto_stop_hours,
        }),
        Err(e) => {
            tracing::error!(vm = %instance.id, error = %e, "could not register the vm");
            None
        }
    };
    Ok(Vm::new(&instance, &state.bootstrap, record.as_ref()))
}
