//! The VM registry: the creator and the auto-stop hours of each VM that the API launched, and
//! the SHA-256 of the VM token that the VM reads its hours with. A VM launched before the
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
}

pub struct NewVm<'a> {
    pub instance_id: &'a str,
    pub tenant: &'a str,
    pub name: &'a str,
    pub creator: Option<&'a str>,
    pub auto_stop_hours: u32,
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
    })
}

pub async fn insert(db: &DatabaseConnection, vm: &NewVm<'_>) -> Result<(), DbErr> {
    let sql = "INSERT INTO vms (instance_id, tenant, name, creator, auto_stop_hours, token_hash)
               VALUES (?, ?, ?, ?, ?, ?)";
    let values = vec![
        vm.instance_id.into(),
        vm.tenant.into(),
        vm.name.into(),
        vm.creator.map(str::to_owned).into(),
        i64::from(vm.auto_stop_hours).into(),
        token_hash(vm.token).into(),
    ];
    db.execute_raw(stmt(sql, values)).await.map(|_| ())
}

/// The rows of a tenant, by instance id.
pub async fn of_tenant(
    db: &DatabaseConnection,
    tenant: &str,
) -> Result<HashMap<String, Record>, DbErr> {
    let sql = "SELECT instance_id, creator, auto_stop_hours FROM vms WHERE tenant = ?";
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
    let sql = "SELECT creator, auto_stop_hours FROM vms WHERE tenant = ? AND instance_id = ?";
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

/// The hours of the VM that holds `token`.
pub async fn hours_by_token(db: &DatabaseConnection, token: &str) -> Result<Option<u32>, DbErr> {
    let sql = "SELECT auto_stop_hours FROM vms WHERE token_hash = ?";
    let row = db
        .query_one_raw(stmt(sql, vec![token_hash(token).into()]))
        .await?;
    row.as_ref().map(hours).transpose()
}

/// Drops the row of a VM, so its token stops working.
pub async fn remove(db: &DatabaseConnection, instance_id: &str) -> Result<(), DbErr> {
    let sql = "DELETE FROM vms WHERE instance_id = ?";
    db.execute_raw(stmt(sql, vec![instance_id.into()]))
        .await
        .map(|_| ())
}
