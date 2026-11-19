use sea_orm_migration::prelude::*;

/// The projects of each tenant, and the `project_id` of an agent and of a VM. A project name
/// is unique in its tenant without regard to the case of ASCII letters. No foreign key: the
/// API checks a `project_id` in the statement that writes it, with its own error messages.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261119_000001_projects"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for sql in [
            "CREATE TABLE projects (
                id TEXT PRIMARY KEY NOT NULL,
                tenant TEXT NOT NULL,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            )",
            "CREATE UNIQUE INDEX projects_tenant_name ON projects (tenant, name COLLATE NOCASE)",
            "ALTER TABLE agents ADD COLUMN project_id TEXT",
            "ALTER TABLE vms ADD COLUMN project_id TEXT",
            "CREATE INDEX agents_tenant_project ON agents (tenant, project_id)",
            "CREATE INDEX vms_tenant_project ON vms (tenant, project_id)",
        ] {
            db.execute_unprepared(sql).await?;
        }
        Ok(())
    }
}
