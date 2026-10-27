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
use tracing::Instrument;

use crate::{
    AppState,
    auth::Client,
    bootstrap::{self, Payload, Store},
    ec2::{self, Action, Instance, Launch},
    error::AppError,
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
}

impl Vm {
    pub fn new(instance: &Instance, store: &Store) -> Self {
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
    let mut vms: Vec<Vm> = (instances.iter())
        .filter(|instance| visible(instance, &client.name))
        .map(|instance| Vm::new(instance, &state.bootstrap))
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
              id: Result<Path<String>, PathRejection>| apply(action, state, client, id),
    )
}

async fn apply(
    action: Action,
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
) -> Result<(StatusCode, Json<Vm>), AppError> {
    let id = id_of(id)?;
    let mut vm = find(&state, &client.name, &id).await?;
    if action.allowed_from().is_some_and(|from| from != vm.state) {
        return Err(AppError::Conflict(format!("vm is {}", vm.state)));
    }
    state.ec2.act(&vm.id, action).await?;
    tracing::info!(tenant = %client.name, vm = %vm.id, ?action, "vm action");
    // EC2 still reports the old state for a moment, so show the state that the action causes.
    vm.state = action.next_state().to_owned();
    Ok((StatusCode::ACCEPTED, Json(Vm::new(&vm, &state.bootstrap))))
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
    tracing::info!(tenant = %client.name, vm = %vm.id, "vm deleted");
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct CreateVm {
    name: String,
    instance_type: Option<String>,
    bootstrap: Payload,
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

    // ponytail: one create at a time (README, "Create a VM"); a lock per tenant and name scales.
    let guard = state.create_lock.clone().lock_owned().await;
    let tenant = client.name.clone();
    let instance_type = instance_type.to_owned();
    // A task of its own: the 10 s request timeout must not split the launch from the payload.
    let task = tokio::spawn(
        async move {
            let _guard = guard;
            launch_vm(&state, &tenant, request, &instance_type).await
        }
        .in_current_span(),
    );
    let vm = task
        .await
        .map_err(|e| AppError::Aws(format!("create task failed: {e}")))??;
    Ok((StatusCode::CREATED, Json(vm)))
}

/// The part of a create that must not be cut in two: the name check, the launch, and the
/// insert of the payload. The caller holds the create lock.
async fn launch_vm(
    state: &AppState,
    tenant: &str,
    request: CreateVm,
    instance_type: &str,
) -> Result<Vm, AppError> {
    // EC2 lists a new VM late, so a name that the payload store holds counts as in use too.
    let listed = (state.ec2.list(tenant).await?)
        .iter()
        .any(|i| visible(i, tenant) && i.name == request.name);
    if listed || state.bootstrap.name_in_use(tenant, &request.name) {
        return Err(AppError::Conflict("name in use".to_owned()));
    }

    let token = bootstrap::new_token();
    let launch = Launch {
        name: request.name,
        tenant: tenant.to_owned(),
        instance_type: instance_type.to_owned(),
        user_data: STANDARD.encode(bootstrap::stub(&state.vm.public_url, &token)),
    };
    let instance = state.ec2.launch(&launch).await?;
    // No await between the launch and the insert: the VM cannot redeem before the entry exists.
    let name = &instance.name;
    (state.bootstrap).insert(&token, request.bootstrap, &instance.id, tenant, name);
    tracing::info!(%tenant, vm = %instance.id, %instance_type, "vm launched");
    Ok(Vm::new(&instance, &state.bootstrap))
}
