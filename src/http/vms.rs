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

use super::agents::{parse, unknown_project};
use crate::{
    AppState,
    auth::Client,
    bootstrap::{self, File, Payload, Store},
    ec2::{self, Action, Instance, Launch},
    error::AppError,
    projects,
    registry::{self, NewVm, OwnerChange, Record},
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
    /// The project of the registry row. `None` also for a VM without a row.
    project_id: Option<String>,
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
            project_id: record.and_then(|r| r.project_id.clone()),
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
pub(crate) fn id_of(id: Result<Path<String>, PathRejection>) -> Result<String, AppError> {
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
    // Under the create lock, a recreate of this VM sees the row before the terminate or not at all.
    let _guard = state.create_lock.lock().await;
    state.ec2.act(&vm.id, Action::Terminate).await?;
    // The VM is gone, so nobody can redeem its payload. A failed terminate keeps the entry.
    state.bootstrap.remove_instance(&vm.id);
    // The VM is gone: a failed drop only leaves a token that reads hours and agents.
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
    project_id: Option<String>,
}

/// The rule of `creator`, for `POST /v1/vms` and `PUT /v1/vms/{id}/owner`.
fn check_creator(creator: Option<&str>) -> Result<(), AppError> {
    if creator.is_some_and(|c| !valid_creator(c)) {
        let message = "creator must be a Matrix user id";
        return Err(AppError::BadRequest(message.to_owned()));
    }
    Ok(())
}

/// `@localpart:server` in printable ASCII, at most 255 bytes (the Matrix limit).
pub(crate) fn valid_creator(id: &str) -> bool {
    id.len() <= 255
        && id.bytes().all(|b| b.is_ascii_graphic())
        && (id.strip_prefix('@'))
            .and_then(|rest| rest.split_once(':'))
            .is_some_and(|(local, server)| !local.is_empty() && !server.is_empty())
}

/// `^[a-z0-9][a-z0-9-]{0,62}$`. The `vm` of an agent has the same rule.
pub(crate) fn valid_name(name: &str) -> bool {
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
    check_creator(request.creator.as_deref())?;

    // ponytail: one create at a time (README, "Create a VM"); a lock per tenant and name scales.
    let guard = state.create_lock.clone().lock_owned().await;
    let tenant = client.name.clone();
    let instance_type = instance_type.to_owned();
    let spec = LaunchSpec {
        name: request.name,
        creator: request.creator,
        project_id: request.project_id,
        bootstrap: request.bootstrap,
    };
    // A task of its own: the 10 s request timeout must not split the launch from the payload.
    let task = tokio::spawn(
        async move {
            name_free(&state, &tenant, &spec.name).await?;
            // Under the lock, which a project delete takes too: the project stays until the
            // registry row that holds it is in.
            if let Some(project) = &spec.project_id
                && !projects::exists(&state.db, &tenant, project).await?
            {
                return Err(unknown_project());
            }
            launch_vm(
                &state,
                &tenant,
                spec,
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

/// A `409` if a VM of the tenant has the name. Run it under the create lock.
async fn name_free(state: &AppState, tenant: &str, name: &str) -> Result<(), AppError> {
    // EC2 lists a new VM late, so a name that the payload store holds counts as in use too.
    let listed = (state.ec2.list(tenant).await?)
        .iter()
        .any(|i| visible(i, tenant) && i.name == name);
    if listed || state.bootstrap.name_in_use(tenant, name) {
        return Err(AppError::Conflict("name in use".to_owned()));
    }
    Ok(())
}

/// What `launch_vm` launches.
struct LaunchSpec {
    name: String,
    creator: Option<String>,
    project_id: Option<String>,
    bootstrap: Payload,
}

/// The part of a create that must not be cut in two: the launch and the insert of the
/// payload. It holds the create lock until the registry row is in, so a project delete sees
/// the row. It checks no name and no project.
async fn launch_vm(
    state: &AppState,
    tenant: &str,
    spec: LaunchSpec,
    instance_type: &str,
    auto_stop_hours: u32,
    guard: OwnedMutexGuard<()>,
) -> Result<Vm, AppError> {
    let token = bootstrap::new_token();
    // The VM reads its auto-stop hours with this token. Root-only: the agent runs as ubuntu.
    let vm_token = bootstrap::new_token();
    let mut payload = spec.bootstrap;
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
        name: spec.name,
        tenant: tenant.to_owned(),
        instance_type: instance_type.to_owned(),
        auto_stop_hours,
        user_data: STANDARD.encode(bootstrap::stub(&state.vm.public_url, &token)),
    };
    let instance = state.ec2.launch(&launch).await?;
    // No await between the launch and the insert: the VM cannot redeem before the entry exists.
    let name = &instance.name;
    (state.bootstrap).insert(&token, payload, &instance.id, tenant, name);
    tracing::info!(%tenant, vm = %instance.id, %instance_type, "vm launched");
    let new = NewVm {
        instance_id: &instance.id,
        tenant,
        name,
        creator: spec.creator.as_deref(),
        auto_stop_hours,
        project_id: spec.project_id.as_deref(),
        token: &vm_token,
    };
    // The VM runs already. Without a row it keeps the hours of its tag, so only log.
    let record = match registry::insert(&state.db, &new).await {
        Ok(()) => Some(Record {
            creator: spec.creator,
            auto_stop_hours,
            project_id: spec.project_id,
        }),
        Err(e) => {
            tracing::error!(vm = %instance.id, error = %e, "could not register the vm");
            None
        }
    };
    drop(guard);
    Ok(Vm::new(&instance, &state.bootstrap, record.as_ref()))
}

#[derive(Deserialize)]
struct RecreateVm {
    bootstrap: Payload,
}

/// `POST /v1/vms/{id}/recreate`: terminates the VM and launches one with the same name, type,
/// creator, project, and hours, with the new payload and a new VM token. It checks no name: the
/// old VM holds it until EC2 reports it as terminated.
pub async fn recreate(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<(StatusCode, Json<Vm>), AppError> {
    let id = id_of(id)?;
    let old = find(&state, &client.name, &id).await?;
    let request: RecreateVm = serde_json::from_slice(&body)
        .map_err(|_| AppError::BadRequest("invalid request body".to_owned()))?;
    request
        .bootstrap
        .validate()
        .map_err(|e| AppError::BadRequest(e.to_owned()))?;
    // Read under the lock: of two recreates of one VM, the second finds no row and gets 409.
    let guard = state.create_lock.clone().lock_owned().await;
    let record = (registry::get(&state.db, &client.name, &old.id).await?)
        .ok_or_else(|| AppError::Conflict("vm has no registry row".to_owned()))?;
    let tenant = client.name.clone();
    // A task of its own, as in `create`: the request timeout must not cut it after the terminate.
    let task = tokio::spawn(
        async move {
            state.ec2.act(&old.id, Action::Terminate).await?;
            state.bootstrap.remove_instance(&old.id);
            // The VM is gone: a failed drop only leaves a token that reads hours and agents.
            if let Err(e) = registry::remove(&state.db, &old.id).await {
                tracing::error!(vm = %old.id, error = %e, "could not drop the registry row");
            }
            let spec = LaunchSpec {
                name: old.name.clone(),
                creator: record.creator,
                project_id: record.project_id,
                bootstrap: request.bootstrap,
            };
            let hours = record.auto_stop_hours;
            let vm = launch_vm(&state, &tenant, spec, &old.instance_type, hours, guard).await?;
            tracing::info!(%tenant, old = %old.id, new = %vm.id, "vm recreated");
            Ok::<_, AppError>(vm)
        }
        .in_current_span(),
    );
    let vm = task
        .await
        .map_err(|e| AppError::Aws(format!("recreate task failed: {e}")))??;
    Ok((StatusCode::CREATED, Json(vm)))
}

/// The body of `PUT /v1/vms/{id}/owner`: at least one field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetOwner {
    project_id: Option<String>,
    creator: Option<String>,
}

/// `PUT /v1/vms/{id}/owner`: sets the project, the creator, or both, in the registry row of the
/// VM, in one statement. `project_id` `""` clears the project.
pub async fn set_owner_route(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<Json<Vm>, AppError> {
    let id = id_of(id)?;
    let vm = find(&state, &client.name, &id).await?;
    let request: SetOwner = parse(&body)?;
    if request.project_id.is_none() && request.creator.is_none() {
        return Err(AppError::BadRequest("invalid request body".to_owned()));
    }
    check_creator(request.creator.as_deref())?;
    let change = OwnerChange {
        project_id: request.project_id,
        creator: request.creator,
    };
    let tenant = &client.name;
    if !registry::set_owner(&state.db, tenant, &vm.id, &change).await? {
        // No row changed: the VM has no row, or the project is not one of the tenant.
        return Err(match registry::get(&state.db, tenant, &vm.id).await? {
            Some(_) => unknown_project(),
            None => AppError::Conflict("vm has no registry row".to_owned()),
        });
    }
    tracing::info!(%tenant, vm = %vm.id, "vm owner set");
    let record = registry::get(&state.db, tenant, &vm.id).await?;
    Ok(Json(Vm::new(&vm, &state.bootstrap, record.as_ref())))
}
