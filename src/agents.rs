//! The agent records of each tenant. An agent without a VM is a profile: its `vm` is NULL.
//! The unique index of `(tenant, vm, name)` keeps a name once per VM of a tenant.

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, QueryResult, Statement, Value,
};
use serde::Serialize;

/// The agent object of the API.
#[derive(Clone, Debug, Serialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub description: String,
    pub model: String,
    pub tag: Option<String>,
    pub project: Option<String>,
    pub vm: Option<String>,
    pub rooms: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct NewAgent<'a> {
    pub name: &'a str,
    pub owner: &'a str,
    pub description: &'a str,
    pub model: &'a str,
    pub tag: Option<&'a str>,
    pub project: Option<&'a str>,
    /// The VM name and the rooms, together.
    pub placement: Option<(&'a str, &'a [String])>,
}

const COLUMNS: &str =
    "id, name, owner, description, model, tag, project, vm, rooms, created_at, updated_at";

fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Sqlite, sql, values)
}

/// 16 random bytes in lowercase hex.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS has a random source");
    hex::encode(bytes)
}

fn rooms_text(rooms: &[String]) -> String {
    serde_json::to_string(rooms).expect("a list of strings serializes")
}

fn agent(row: &QueryResult) -> Result<Agent, DbErr> {
    let rooms: String = row.try_get("", "rooms")?;
    Ok(Agent {
        id: row.try_get("", "id")?,
        name: row.try_get("", "name")?,
        owner: row.try_get("", "owner")?,
        description: row.try_get("", "description")?,
        model: row.try_get("", "model")?,
        tag: row.try_get("", "tag")?,
        project: row.try_get("", "project")?,
        vm: row.try_get("", "vm")?,
        rooms: serde_json::from_str(&rooms).map_err(|e| DbErr::Custom(format!("rooms: {e}")))?,
        created_at: row.try_get("", "created_at")?,
        updated_at: row.try_get("", "updated_at")?,
    })
}

pub async fn insert(
    db: &DatabaseConnection,
    tenant: &str,
    agent: &NewAgent<'_>,
) -> Result<Agent, DbErr> {
    let id = new_id();
    let (vm, rooms) = agent.placement.unzip();
    let sql =
        "INSERT INTO agents (id, tenant, name, owner, description, model, tag, project, vm, rooms)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
    let values = vec![
        id.as_str().into(),
        tenant.into(),
        agent.name.into(),
        agent.owner.into(),
        agent.description.into(),
        agent.model.into(),
        agent.tag.map(str::to_owned).into(),
        agent.project.map(str::to_owned).into(),
        vm.map(str::to_owned).into(),
        rooms_text(rooms.unwrap_or_default()).into(),
    ];
    db.execute_raw(stmt(sql, values)).await?;
    (get(db, tenant, &id).await?).ok_or_else(|| DbErr::Custom(format!("agent {id} not stored")))
}

/// The agents of a tenant, by `created_at`, then `id`.
pub async fn of_tenant(db: &DatabaseConnection, tenant: &str) -> Result<Vec<Agent>, DbErr> {
    let sql = format!("SELECT {COLUMNS} FROM agents WHERE tenant = ? ORDER BY created_at, id");
    let rows = db.query_all_raw(stmt(&sql, vec![tenant.into()])).await?;
    rows.iter().map(agent).collect()
}

pub async fn get(db: &DatabaseConnection, tenant: &str, id: &str) -> Result<Option<Agent>, DbErr> {
    let sql = format!("SELECT {COLUMNS} FROM agents WHERE tenant = ? AND id = ?");
    let row = db
        .query_one_raw(stmt(&sql, vec![tenant.into(), id.into()]))
        .await?;
    row.as_ref().map(agent).transpose()
}

/// `None` keeps a value. For `tag` and `project`, `Some("")` stores NULL.
#[derive(Default)]
pub struct Changes {
    pub owner: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub tag: Option<String>,
    pub project: Option<String>,
    pub placement: Option<(String, Vec<String>)>,
}

/// One UPDATE. With `placement`, it applies only to a row whose `vm` is NULL. `false`: no row
/// changed. With no change, it reports whether the row exists.
pub async fn update(
    db: &DatabaseConnection,
    tenant: &str,
    id: &str,
    changes: &Changes,
) -> Result<bool, DbErr> {
    let label = |value: &String| (!value.is_empty()).then(|| value.clone());
    let mut sets: Vec<(&str, Value)> = Vec::new();
    if let Some(owner) = &changes.owner {
        sets.push(("owner", owner.as_str().into()));
    }
    if let Some(description) = &changes.description {
        sets.push(("description", description.as_str().into()));
    }
    if let Some(model) = &changes.model {
        sets.push(("model", model.as_str().into()));
    }
    if let Some(tag) = &changes.tag {
        sets.push(("tag", label(tag).into()));
    }
    if let Some(project) = &changes.project {
        sets.push(("project", label(project).into()));
    }
    if let Some((vm, rooms)) = &changes.placement {
        sets.push(("vm", vm.as_str().into()));
        sets.push(("rooms", rooms_text(rooms).into()));
    }
    if sets.is_empty() {
        let sql = "SELECT id FROM agents WHERE tenant = ? AND id = ?";
        let row = (db.query_one_raw(stmt(sql, vec![tenant.into(), id.into()]))).await?;
        return Ok(row.is_some());
    }
    let columns: Vec<_> = sets
        .iter()
        .map(|(column, _)| format!("{column} = ?"))
        .collect();
    let only_profile = if changes.placement.is_some() {
        " AND vm IS NULL"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE agents SET {}, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
         WHERE tenant = ? AND id = ?{only_profile}",
        columns.join(", ")
    );
    let mut values: Vec<Value> = sets.into_iter().map(|(_, value)| value).collect();
    values.extend([tenant.into(), id.into()]);
    let done = db.execute_raw(stmt(&sql, values)).await?;
    Ok(done.rows_affected() > 0)
}

/// `false`: no such row.
pub async fn remove(db: &DatabaseConnection, tenant: &str, id: &str) -> Result<bool, DbErr> {
    let sql = "DELETE FROM agents WHERE tenant = ? AND id = ?";
    let done = db
        .execute_raw(stmt(sql, vec![tenant.into(), id.into()]))
        .await?;
    Ok(done.rows_affected() > 0)
}
