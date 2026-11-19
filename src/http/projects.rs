//! The projects of the tenant. The control plane checks the fields. zooid checks who may do
//! what: it holds the roles.

use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
};
use serde::Deserialize;

use super::{
    agents::{hex_id, name_in_use, parse},
    vms::visible,
};
use crate::{
    AppState,
    auth::Client,
    error::AppError,
    projects::{self, Project},
    registry,
};

/// The body of both writes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectBody {
    name: String,
}

/// 1 to 64 characters, none of them a control character, and no white space at either end.
fn valid_project_name(name: &str) -> bool {
    (1..=64).contains(&name.chars().count())
        && !name.chars().any(char::is_control)
        && name.trim() == name
}

/// The name of a write body.
fn name_of(body: &[u8]) -> Result<String, AppError> {
    let request: ProjectBody = parse(body)?;
    valid_project_name(&request.name)
        .then_some(request.name)
        .ok_or_else(|| AppError::BadRequest("invalid project name".to_owned()))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
) -> Result<Json<Vec<Project>>, AppError> {
    Ok(Json(projects::of_tenant(&state.db, &client.name).await?))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    body: Bytes,
) -> Result<(StatusCode, Json<Project>), AppError> {
    let name = name_of(&body)?;
    let project = (projects::insert(&state.db, &client.name, &name).await).map_err(name_in_use)?;
    tracing::info!(tenant = %client.name, project = %project.id, "project created");
    Ok((StatusCode::CREATED, Json(project)))
}

/// `PUT /v1/projects/{id}`: a rename. A rename that changes only the case is allowed: the
/// unique index ignores the row that the UPDATE changes.
pub async fn change(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<Json<Project>, AppError> {
    let id = hex_id(id)?;
    let name = name_of(&body)?;
    let tenant = &client.name;
    if !(projects::rename(&state.db, tenant, &id, &name).await).map_err(name_in_use)? {
        return Err(AppError::NotFound);
    }
    let project = (projects::get(&state.db, tenant, &id).await?).ok_or(AppError::NotFound)?;
    tracing::info!(%tenant, project = %id, "project renamed");
    Ok(Json(project))
}

/// `DELETE /v1/projects/{id}`: deletes a project that no agent and no registry row of the
/// tenant has, in one statement. A row of a VM that is gone does not count.
pub async fn delete(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, AppError> {
    let id = hex_id(id)?;
    let tenant = &client.name;
    // No create or recreate is between its check of the project and its registry insert.
    let _guard = state.create_lock.lock().await;
    if !projects::exists(&state.db, tenant, &id).await? {
        return Err(AppError::NotFound);
    }
    drop_rows_of_gone_vms(&state, tenant, &id).await?;
    if !projects::remove_if_free(&state.db, tenant, &id).await? {
        return Err(AppError::Conflict("project in use".to_owned()));
    }
    tracing::info!(%tenant, project = %id, "project deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Drops the rows of the project that no VM backs: a VM terminated outside the control plane
/// keeps its row for ever. Only rows older than 10 minutes, because EC2 lists a new VM late.
/// A failed EC2 list drops nothing, and the delete goes on.
async fn drop_rows_of_gone_vms(
    state: &AppState,
    tenant: &str,
    project: &str,
) -> Result<(), AppError> {
    let old = registry::old_rows_of_project(&state.db, tenant, project).await?;
    if old.is_empty() {
        return Ok(());
    }
    let instances = match state.ec2.list(tenant).await {
        Ok(instances) => instances,
        Err(e) => {
            let error = format!("{e:#}");
            tracing::warn!(%tenant, %project, %error, "could not list the vms, so no row is dropped");
            return Ok(());
        }
    };
    let listed = |vm: &String| (instances.iter()).any(|i| i.id == *vm && visible(i, tenant));
    for vm in old.iter().filter(|vm| !listed(vm)) {
        registry::remove(&state.db, vm).await?;
        tracing::info!(%tenant, %vm, "dropped the registry row of a vm that is gone");
    }
    Ok(())
}
