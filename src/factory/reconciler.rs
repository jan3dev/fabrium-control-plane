//! Periodic reconciler: budget expiry and webhook drift catch-up.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use tokio::time::MissedTickBehavior;

use super::dispatch;
use super::machine::RunState;
use super::runtime::FactoryRuntime;
use crate::entity::factory_runs;
use crate::github::events::FactoryEvent;

pub const INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Spawn the reconciler. It never exits on its own.
pub fn spawn(rt: Arc<FactoryRuntime>, db: DatabaseConnection) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(INTERVAL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = tick_once(rt.clone(), &db).await {
                tracing::error!(error = %format!("{e:#}"), "factory reconciler tick failed");
            }
        }
    });
}

async fn tick_once(rt: Arc<FactoryRuntime>, db: &DatabaseConnection) -> anyhow::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;

    let active = factory_runs::Entity::find()
        .filter(
            factory_runs::Column::State
                .is_in([RunState::Implementing.as_str(), RunState::Review.as_str()]),
        )
        .all(db)
        .await?;

    for row in active {
        if let Some(ends) = row.budget_ends_at
            && now > ends
        {
            let event = FactoryEvent::BudgetExpired {
                repo: row.repo.clone(),
                issue: row.issue,
            };
            dispatch::handle(rt.clone(), db.clone(), event).await?;
        }
    }

    // Listing open factory issues/PRs via the GitHub API fills gaps when a webhook
    // was lost. Keep the first cut to budget expiry so the App stays optional in tests;
    // the poller expands once FACTORY_REPOS installations are live.
    let _ = rt.config.github.factory_repos.len();
    Ok(())
}
