use std::{path::Path, str::FromStr};

use anyhow::{Context, Result};
use migration::{Migrator, MigratorTrait};
use sea_orm::{
    DatabaseConnection, SqlxSqliteConnector,
    sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

pub async fn connect(url: &str) -> Result<DatabaseConnection> {
    let options = SqliteConnectOptions::from_str(url)
        .context("DATABASE_URL is not a valid SQLite URL")?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let file = options.get_filename();
    if let Some(dir) = file.parent().filter(|d| *d != Path::new("")) {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    let db = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool);
    Migrator::up(&db, None).await.context("migrations failed")?;
    Ok(db)
}
