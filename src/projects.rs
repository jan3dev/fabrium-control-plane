//! The projects of each tenant. The unique index of `(tenant, name COLLATE NOCASE)` keeps a
//! name once per tenant, without regard to the case of ASCII letters.

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, QueryResult, Statement, Value,
};
use serde::Serialize;

/// The project object of the API.
#[derive(Clone, Debug, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

const COLUMNS: &str = "id, name, created_at, updated_at";

fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Sqlite, sql, values)
}

/// 16 random bytes in lowercase hex.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS has a random source");
    hex::encode(bytes)
}

fn project(row: &QueryResult) -> Result<Project, DbErr> {
    Ok(Project {
        id: row.try_get("", "id")?,
        name: row.try_get("", "name")?,
        created_at: row.try_get("", "created_at")?,
        updated_at: row.try_get("", "updated_at")?,
    })
}

pub async fn insert(db: &DatabaseConnection, tenant: &str, name: &str) -> Result<Project, DbErr> {
    let id = new_id();
    let sql = "INSERT INTO projects (id, tenant, name) VALUES (?, ?, ?)";
    let values = vec![id.as_str().into(), tenant.into(), name.into()];
    db.execute_raw(stmt(sql, values)).await?;
    (get(db, tenant, &id).await?).ok_or_else(|| DbErr::Custom(format!("project {id} not stored")))
}

/// The projects of a tenant, by name without regard to ASCII case, then `id`.
pub async fn of_tenant(db: &DatabaseConnection, tenant: &str) -> Result<Vec<Project>, DbErr> {
    let sql =
        format!("SELECT {COLUMNS} FROM projects WHERE tenant = ? ORDER BY name COLLATE NOCASE, id");
    let rows = db.query_all_raw(stmt(&sql, vec![tenant.into()])).await?;
    rows.iter().map(project).collect()
}

pub async fn get(
    db: &DatabaseConnection,
    tenant: &str,
    id: &str,
) -> Result<Option<Project>, DbErr> {
    let sql = format!("SELECT {COLUMNS} FROM projects WHERE tenant = ? AND id = ?");
    let row = db
        .query_one_raw(stmt(&sql, vec![tenant.into(), id.into()]))
        .await?;
    row.as_ref().map(project).transpose()
}

pub async fn exists(db: &DatabaseConnection, tenant: &str, id: &str) -> Result<bool, DbErr> {
    let sql = "SELECT 1 FROM projects WHERE tenant = ? AND id = ?";
    let row = db
        .query_one_raw(stmt(sql, vec![tenant.into(), id.into()]))
        .await?;
    Ok(row.is_some())
}

/// One UPDATE. `false`: no such project of the tenant.
pub async fn rename(
    db: &DatabaseConnection,
    tenant: &str,
    id: &str,
    name: &str,
) -> Result<bool, DbErr> {
    let sql = "UPDATE projects SET name = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
               WHERE tenant = ? AND id = ?";
    let values = vec![name.into(), tenant.into(), id.into()];
    let done = db.execute_raw(stmt(sql, values)).await?;
    Ok(done.rows_affected() > 0)
}
