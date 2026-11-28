pub use sea_orm_migration::prelude::*;

mod m20261111_000001_vms;
mod m20261118_000001_agents;
mod m20261119_000001_projects;
mod m20261128_000001_github_deliveries;
mod m20261128_000002_factory_runs;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261111_000001_vms::Migration),
            Box::new(m20261118_000001_agents::Migration),
            Box::new(m20261119_000001_projects::Migration),
            Box::new(m20261128_000001_github_deliveries::Migration),
            Box::new(m20261128_000002_factory_runs::Migration),
        ]
    }
}
