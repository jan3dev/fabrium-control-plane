use sea_orm_migration::prelude::*;

/// The agent records: one row per agent of a tenant. A profile has no VM: `vm` is NULL, and
/// SQLite treats each NULL as distinct, so two profiles can share a name.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261118_000001_agents"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            "CREATE TABLE agents (
                id TEXT PRIMARY KEY NOT NULL,
                tenant TEXT NOT NULL,
                name TEXT NOT NULL,
                owner TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                model TEXT NOT NULL,
                tag TEXT,
                project TEXT,
                vm TEXT,
                rooms TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            )",
        )
        .await?;
        db.execute_unprepared(
            "CREATE UNIQUE INDEX agents_tenant_vm_name ON agents (tenant, vm, name)",
        )
        .await?;
        db.execute_unprepared("CREATE INDEX agents_tenant ON agents (tenant)")
            .await
            .map(|_| ())
    }
}
