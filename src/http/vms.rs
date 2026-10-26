use aws_sdk_ec2::primitives::DateTimeFormat;
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    routing::{MethodRouter, post},
};
use serde::Serialize;

use crate::{
    AppState,
    auth::Client,
    bootstrap::Store,
    ec2::{self, Action, Instance},
    error::AppError,
};

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

/// The isolation rule. The tenant tag is checked here, never left to a filter of the port,
/// so a VM of another tenant, a VM that Fabrium did not launch, and a terminated VM are
/// all invisible.
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

/// `POST` route that applies `action` to the VM of the path. EC2 would accept more states
/// than the API does, so the state table is checked here.
pub fn action(action: Action) -> MethodRouter<AppState> {
    post(
        move |state: State<AppState>, client: Extension<Client>, id: Path<String>| {
            apply(action, state, client, id)
        },
    )
}

async fn apply(
    action: Action,
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Vm>), AppError> {
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
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let vm = find(&state, &client.name, &id).await?;
    state.ec2.act(&vm.id, Action::Terminate).await?;
    // The VM is gone, so nobody can redeem its payload. A failed terminate keeps the entry.
    state.bootstrap.remove_instance(&vm.id);
    tracing::info!(tenant = %client.name, vm = %vm.id, "vm deleted");
    Ok(StatusCode::ACCEPTED)
}
