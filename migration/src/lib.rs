pub use sea_orm_migration::prelude::*;

mod m20261030_000001_github_deliveries;
mod m20261030_000002_factory_runs;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261030_000001_github_deliveries::Migration),
            Box::new(m20261030_000002_factory_runs::Migration),
        ]
    }
}
