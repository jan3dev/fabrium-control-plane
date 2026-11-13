use sea_orm_migration::prelude::*;

/// The VM registry: one row per VM that the API launched.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261111_000001_vms"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE TABLE vms (
                    instance_id TEXT PRIMARY KEY NOT NULL,
                    tenant TEXT NOT NULL,
                    name TEXT NOT NULL,
                    creator TEXT,
                    auto_stop_hours INTEGER NOT NULL,
                    token_hash TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                )",
            )
            .await
            .map(|_| ())
    }
}
