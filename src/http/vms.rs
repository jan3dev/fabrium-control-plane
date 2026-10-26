use aws_sdk_ec2::primitives::DateTimeFormat;
use axum::{Extension, Json, extract::State};
use serde::Serialize;

use crate::{
    AppState,
    auth::Client,
    bootstrap::Store,
    ec2::{self, Instance},
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
