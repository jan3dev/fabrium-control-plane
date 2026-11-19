//! The projects of the tenant. The control plane checks the fields. zooid checks who may do
//! what: it holds the roles.

use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
};
use serde::Deserialize;

use super::agents::{hex_id, name_in_use, parse};
use crate::{
    AppState,
    auth::Client,
    error::AppError,
    projects::{self, Project},
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
