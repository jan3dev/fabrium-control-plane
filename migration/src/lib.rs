pub use sea_orm_migration::prelude::*;

mod m20261111_000001_vms;
mod m20261118_000001_agents;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261111_000001_vms::Migration),
            Box::new(m20261118_000001_agents::Migration),
        ]
    }
}
