//! The agent records of the tenant. The control plane checks the fields. zooid checks who may
//! do what: it holds the roles.

use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, State, rejection::PathRejection},
    http::StatusCode,
};
use sea_orm::{DbErr, SqlErr};
use serde::{Deserialize, de::DeserializeOwned};

use super::vms;
use crate::{
    AppState,
    agents::{self, Agent, Changes, NewAgent},
    auth::Client,
    error::AppError,
    projects,
};

/// The largest body of an agent write. A larger one gives 413.
pub const MAX_BODY: usize = 128 * 1024;

/// The largest description, in bytes of UTF-8.
const MAX_DESCRIPTION: usize = 16384;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateAgent {
    name: String,
    owner: String,
    model: String,
    #[serde(default)]
    description: String,
    tag: Option<String>,
    project: Option<String>,
    project_id: Option<String>,
    vm: Option<String>,
    rooms: Option<Vec<String>>,
}

/// Every field is optional. An absent field keeps its value.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangeAgent {
    owner: Option<String>,
    description: Option<String>,
    model: Option<String>,
    tag: Option<String>,
    project: Option<String>,
    project_id: Option<String>,
    vm: Option<String>,
    rooms: Option<Vec<String>>,
}

pub(crate) fn unknown_project() -> AppError {
    AppError::BadRequest("unknown project".to_owned())
}

fn check(ok: bool, message: &str) -> Result<(), AppError> {
    ok.then_some(())
        .ok_or_else(|| AppError::BadRequest(message.to_owned()))
}

/// The body, which must be a JSON object: serde also reads a struct from a positional array.
/// A fixed message: a serde error can echo a value of the body.
pub(crate) fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, AppError> {
    let invalid = || AppError::BadRequest("invalid request body".to_owned());
    if body.trim_ascii_start().first() != Some(&b'{') {
        return Err(invalid());
    }
    serde_json::from_slice(body).map_err(|_| invalid())
}

/// `^[a-z][a-z0-9-]{0,31}$`
fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// `^[\x21-\x7e]{1,100}$`
fn valid_model(model: &str) -> bool {
    (1..=100).contains(&model.len()) && model.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// A tag or a project: 1 to 200 characters, none of them a control character.
fn valid_label(label: &str) -> bool {
    (1..=200).contains(&label.chars().count()) && !label.chars().any(char::is_control)
}

/// At most 20 rooms, each `^[#!][^\s'",\\]+$` and at most 255 bytes.
fn valid_rooms(rooms: &[String]) -> bool {
    let bad = |c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ',' | '\\');
    rooms.len() <= 20
        && rooms.iter().all(|room| {
            room.len() <= 255
                && (room.strip_prefix(['#', '!']))
                    .is_some_and(|rest| !rest.is_empty() && !rest.chars().any(bad))
        })
}

/// The rules of the fields that both writes take. `None` is a field that the body leaves out.
fn check_fields(
    owner: Option<&str>,
    description: Option<&str>,
    model: Option<&str>,
    labels: [Option<&str>; 2],
) -> Result<(), AppError> {
    let owner_ok = owner.is_none_or(vms::valid_creator);
    check(owner_ok, "owner must be a Matrix user id")?;
    let description_ok = description.is_none_or(|d| d.len() <= MAX_DESCRIPTION);
    check(description_ok, "description must be at most 16384 bytes")?;
    let model_ok = model.is_none_or(valid_model);
    check(model_ok, "model must match ^[\\x21-\\x7e]{1,100}$")?;
    let labels_ok = labels.into_iter().flatten().all(valid_label);
    check(
        labels_ok,
        "tag and project must be 1 to 200 characters without control characters",
    )
}

/// `vm` and `rooms` come together.
fn placement(
    vm: Option<String>,
    rooms: Option<Vec<String>>,
) -> Result<Option<(String, Vec<String>)>, AppError> {
    match (vm, rooms) {
        (None, None) => Ok(None),
        (Some(vm), Some(rooms)) => {
            check(
                vms::valid_name(&vm),
                "vm must match ^[a-z0-9][a-z0-9-]{0,62}$",
            )?;
            check(
                valid_rooms(&rooms),
                "rooms must be at most 20 room aliases or ids",
            )?;
            Ok(Some((vm, rooms)))
        }
        _ => Err(AppError::BadRequest(
            "vm and rooms come together".to_owned(),
        )),
    }
}

/// A unique index refused the row: `(tenant, vm, name)` of an agent, or the name of a project.
pub(crate) fn name_in_use(e: DbErr) -> AppError {
    match e.sql_err() {
        Some(SqlErr::UniqueConstraintViolation(_)) => AppError::Conflict("name in use".to_owned()),
        _ => e.into(),
    }
}

pub async fn list(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
) -> Result<Json<Vec<Agent>>, AppError> {
    Ok(Json(agents::of_tenant(&state.db, &client.name).await?))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    body: Bytes,
) -> Result<(StatusCode, Json<Agent>), AppError> {
    let request: CreateAgent = parse(&body)?;
    check(
        valid_name(&request.name),
        "name must match ^[a-z][a-z0-9-]{0,31}$",
    )?;
    let labels = [request.tag.as_deref(), request.project.as_deref()];
    let description = Some(request.description.as_str());
    check_fields(
        Some(&request.owner),
        description,
        Some(&request.model),
        labels,
    )?;
    let placement = placement(request.vm, request.rooms)?;
    let new = NewAgent {
        name: &request.name,
        owner: &request.owner,
        description: &request.description,
        model: &request.model,
        tag: request.tag.as_deref(),
        project: request.project.as_deref(),
        project_id: request.project_id.as_deref(),
        placement: (placement.as_ref()).map(|(vm, rooms)| (vm.as_str(), rooms.as_slice())),
    };
    let agent = (agents::insert(&state.db, &client.name, &new).await).map_err(name_in_use)?;
    let agent = agent.ok_or_else(unknown_project)?;
    tracing::info!(tenant = %client.name, agent = %agent.id, "agent created");
    Ok((StatusCode::CREATED, Json(agent)))
}

/// `^[0-9a-f]{32}$`, the id of an agent or a project. Any other id, one that does not decode
/// included, is not found.
pub(crate) fn hex_id(id: Result<Path<String>, PathRejection>) -> Result<String, AppError> {
    let id = vms::id_of(id)?;
    let valid = id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    valid.then_some(id).ok_or(AppError::NotFound)
}

pub async fn change(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
    body: Bytes,
) -> Result<Json<Agent>, AppError> {
    let id = hex_id(id)?;
    let request: ChangeAgent = parse(&body)?;
    // `""` clears a tag or a project, so only a value that is not empty has the label rules.
    let labels = [request.tag.as_deref(), request.project.as_deref()];
    let labels = labels.map(|label| label.filter(|l| !l.is_empty()));
    let (owner, model) = (request.owner.as_deref(), request.model.as_deref());
    check_fields(owner, request.description.as_deref(), model, labels)?;
    let changes = Changes {
        placement: placement(request.vm, request.rooms)?,
        owner: request.owner,
        description: request.description,
        model: request.model,
        tag: request.tag,
        project: request.project,
        project_id: request.project_id,
    };
    let tenant = &client.name;
    if !(agents::update(&state.db, tenant, &id, &changes).await).map_err(name_in_use)? {
        // No row changed: the agent is not there, the project is not one of the tenant, or the
        // change sets a `vm` that the agent has.
        if agents::get(&state.db, tenant, &id).await?.is_none() {
            return Err(AppError::NotFound);
        }
        let project = (changes.project_id.as_deref()).filter(|p| !p.is_empty());
        if let Some(project) = project
            && !projects::exists(&state.db, tenant, project).await?
        {
            return Err(unknown_project());
        }
        return Err(AppError::Conflict("vm already set".to_owned()));
    }
    let agent = (agents::get(&state.db, tenant, &id).await?).ok_or(AppError::NotFound)?;
    tracing::info!(%tenant, agent = %id, "agent changed");
    Ok(Json(agent))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(client): Extension<Client>,
    id: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, AppError> {
    let id = hex_id(id)?;
    if !agents::remove(&state.db, &client.name, &id).await? {
        return Err(AppError::NotFound);
    }
    tracing::info!(tenant = %client.name, agent = %id, "agent deleted");
    Ok(StatusCode::NO_CONTENT)
}
