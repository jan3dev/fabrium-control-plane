//! The VM registry: the creator, the project, and the auto-stop hours of each VM that the API
//! launched, and the SHA-256 of the VM token that the VM reads its hours with. A VM launched before the
//! registry has no row: its hours come from its tag, and the API cannot change them.

use std::collections::HashMap;

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, QueryResult, Statement, Value,
};
use sha2::{Digest, Sha256};

/// One row, reduced to what the API shows.
pub struct Record {
    pub creator: Option<String>,
    pub auto_stop_hours: u32,
    pub project_id: Option<String>,
}

pub struct NewVm<'a> {
    pub instance_id: &'a str,
    pub tenant: &'a str,
    pub name: &'a str,
    pub creator: Option<&'a str>,
    pub auto_stop_hours: u32,
    pub project_id: Option<&'a str>,
    /// The VM token. Only its SHA-256 is stored.
    pub token: &'a str,
}

fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Sqlite, sql, values)
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn hours(row: &QueryResult) -> Result<u32, DbErr> {
    let hours: i64 = row.try_get("", "auto_stop_hours")?;
    u32::try_from(hours).map_err(|e| DbErr::Custom(format!("auto_stop_hours: {e}")))
}

fn record(row: &QueryResult) -> Result<Record, DbErr> {
    Ok(Record {
        creator: row.try_get("", "creator")?,
        auto_stop_hours: hours(row)?,
        project_id: row.try_get("", "project_id")?,
    })
}

pub async fn insert(db: &DatabaseConnection, vm: &NewVm<'_>) -> Result<(), DbErr> {
    let sql = "INSERT INTO vms
                 (instance_id, tenant, name, creator, auto_stop_hours, token_hash, project_id)
               VALUES (?, ?, ?, ?, ?, ?, ?)";
    let values = vec![
        vm.instance_id.into(),
        vm.tenant.into(),
        vm.name.into(),
        vm.creator.map(str::to_owned).into(),
        i64::from(vm.auto_stop_hours).into(),
        token_hash(vm.token).into(),
        vm.project_id.map(str::to_owned).into(),
    ];
    db.execute_raw(stmt(sql, values)).await.map(|_| ())
}

/// The rows of a tenant, by instance id.
pub async fn of_tenant(
    db: &DatabaseConnection,
    tenant: &str,
) -> Result<HashMap<String, Record>, DbErr> {
    let sql = "SELECT instance_id, creator, auto_stop_hours, project_id FROM vms WHERE tenant = ?";
    let rows = db.query_all_raw(stmt(sql, vec![tenant.into()])).await?;
    (rows.iter())
        .map(|row| Ok((row.try_get("", "instance_id")?, record(row)?)))
        .collect()
}

pub async fn get(
    db: &DatabaseConnection,
    tenant: &str,
    instance_id: &str,
) -> Result<Option<Record>, DbErr> {
    let sql = "SELECT creator, auto_stop_hours, project_id FROM vms
               WHERE tenant = ? AND instance_id = ?";
    let row = db
        .query_one_raw(stmt(sql, vec![tenant.into(), instance_id.into()]))
        .await?;
    row.as_ref().map(record).transpose()
}

/// Sets the hours of a VM. `false` if the VM has no row.
pub async fn set_hours(
    db: &DatabaseConnection,
    tenant: &str,
    instance_id: &str,
    auto_stop_hours: u32,
) -> Result<bool, DbErr> {
    let sql = "UPDATE vms SET auto_stop_hours = ? WHERE tenant = ? AND instance_id = ?";
    let values = vec![
        i64::from(auto_stop_hours).into(),
        tenant.into(),
        instance_id.into(),
    ];
    let done = db.execute_raw(stmt(sql, values)).await?;
    Ok(done.rows_affected() > 0)
}

/// `None` keeps a value. `project_id` `Some("")` stores NULL.
pub struct OwnerChange {
    pub project_id: Option<String>,
    pub creator: Option<String>,
}

/// One UPDATE of the fields that `change` sets, at least one. With a `project_id` that is not
/// empty, it applies only if that is a project of the tenant. `false`: no row changed (no row,
/// or an unknown project).
pub async fn set_owner(
    db: &DatabaseConnection,
    tenant: &str,
    instance_id: &str,
    change: &OwnerChange,
) -> Result<bool, DbErr> {
    let mut sets: Vec<(&str, Value)> = Vec::new();
    if let Some(project_id) = &change.project_id {
        let stored = (!project_id.is_empty()).then(|| project_id.clone());
        sets.push(("project_id", stored.into()));
    }
    if let Some(creator) = &change.creator {
        sets.push(("creator", creator.as_str().into()));
    }
    let columns: Vec<_> = sets.iter().map(|(c, _)| format!("{c} = ?")).collect();
    let project = (change.project_id.as_deref()).filter(|p| !p.is_empty());
    let only_known = if project.is_some() {
        " AND EXISTS (SELECT 1 FROM projects WHERE id = ? AND tenant = ?)"
    } else {
        ""
    };
    let sql = format!(
        "UPDATE vms SET {} WHERE tenant = ? AND instance_id = ?{only_known}",
        columns.join(", ")
    );
    let mut values: Vec<Value> = sets.into_iter().map(|(_, value)| value).collect();
    values.extend([tenant.into(), instance_id.into()]);
    if let Some(project) = project {
        values.extend([project.into(), tenant.into()]);
    }
    let done = db.execute_raw(stmt(&sql, values)).await?;
    Ok(done.rows_affected() > 0)
}

/// The hours of the VM that holds `token`.
pub async fn hours_by_token(db: &DatabaseConnection, token: &str) -> Result<Option<u32>, DbErr> {
    let sql = "SELECT auto_stop_hours FROM vms WHERE token_hash = ?";
    let row = db
        .query_one_raw(stmt(sql, vec![token_hash(token).into()]))
        .await?;
    row.as_ref().map(hours).transpose()
}

/// The tenant and the name of the VM that holds `token`.
pub async fn vm_by_token(
    db: &DatabaseConnection,
    token: &str,
) -> Result<Option<(String, String)>, DbErr> {
    let sql = "SELECT tenant, name FROM vms WHERE token_hash = ?";
    let row = db
        .query_one_raw(stmt(sql, vec![token_hash(token).into()]))
        .await?;
    (row.as_ref())
        .map(|row| Ok((row.try_get("", "tenant")?, row.try_get("", "name")?)))
        .transpose()
}

/// The `instance_id` of the rows of the project that are older than 10 minutes.
pub async fn old_rows_of_project(
    db: &DatabaseConnection,
    tenant: &str,
    project_id: &str,
) -> Result<Vec<String>, DbErr> {
    let sql = "SELECT instance_id FROM vms WHERE tenant = ? AND project_id = ?
               AND created_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-10 minutes')";
    let rows = db
        .query_all_raw(stmt(sql, vec![tenant.into(), project_id.into()]))
        .await?;
    rows.iter()
        .map(|row| row.try_get("", "instance_id"))
        .collect()
}

/// Drops the row of a VM, so its token stops working.
pub async fn remove(db: &DatabaseConnection, instance_id: &str) -> Result<(), DbErr> {
    let sql = "DELETE FROM vms WHERE instance_id = ?";
    db.execute_raw(stmt(sql, vec![instance_id.into()]))
        .await
        .map(|_| ())
}
