//! Load a run, run the pure transition, apply actions, persist.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use super::actions;
use super::machine::{self, Action, RunView};
use super::runtime::FactoryRuntime;
use crate::entity::factory_runs;
use crate::github::events::FactoryEvent;

pub async fn handle(
    rt: Arc<FactoryRuntime>,
    db: DatabaseConnection,
    event: FactoryEvent,
) -> Result<()> {
    let now = now_secs();
    let before = load_run(&db, &event).await?;
    let Some((mut after, action_list)) = machine::transition(before.clone(), &event, now) else {
        tracing::debug!(?event, "no factory transition");
        return Ok(());
    };

    for action in action_list {
        match action {
            Action::StartImplementing {
                repo,
                issue,
                title,
                body,
                branch,
            } => {
                let room = actions::create_task_room(&rt, &repo, issue, &title)
                    .await
                    .context("StartImplementing create room")?;
                after.task_room = Some(room.clone());
                save_run(&db, &after, now).await?;
                actions::wake_implementer(&rt, &room, issue, &branch, &title, &body)
                    .await
                    .context("StartImplementing wake")?;
            }
            Action::RewakeImplementer {
                conclusion,
                ci_attempts,
                ..
            } => {
                let room = after
                    .task_room
                    .as_deref()
                    .context("rewake without task room")?;
                actions::rewake_implementer(&rt, room, &conclusion, ci_attempts)
                    .await
                    .context("RewakeImplementer")?;
            }
            Action::RewakeReviewer {
                conclusion,
                review_attempts,
            } => {
                let room = after
                    .review_room
                    .as_deref()
                    .context("rewake reviewer without review room")?;
                actions::rewake_reviewer(&rt, room, &conclusion, review_attempts)
                    .await
                    .context("RewakeReviewer")?;
            }
            Action::StartReview {
                repo,
                issue,
                branch,
            } => {
                let room = actions::create_review_room(&rt, &repo, issue)
                    .await
                    .context("StartReview create room")?;
                after.review_room = Some(room.clone());
                save_run(&db, &after, now).await?;
                actions::wake_reviewer(&rt, &room, &repo, issue, &branch)
                    .await
                    .context("StartReview wake")?;
            }
            Action::FinishReview { branch, .. } => {
                let room = after
                    .review_room
                    .as_deref()
                    .context("finish review without review room")?;
                actions::finish_review(&rt, room, &branch)
                    .await
                    .context("FinishReview")?;
            }
            Action::SetStateLabel { repo, issue, state } => {
                actions::set_state_label(&rt, &repo, issue, state)
                    .await
                    .context("SetStateLabel")?;
            }
            Action::RemoveReadyLabel { repo, issue } => {
                actions::remove_ready_label(&rt, &repo, issue)
                    .await
                    .context("RemoveReadyLabel")?;
            }
            Action::Comment { repo, issue, body } => {
                actions::comment(&rt, &repo, issue, &body)
                    .await
                    .context("Comment")?;
            }
            Action::ArchiveTaskRoom { room } => {
                actions::archive_room(&rt, &room)
                    .await
                    .context("ArchiveTaskRoom")?;
            }
        }
    }

    if machine::should_persist(&before, &after, &event) {
        save_run(&db, &after, now).await?;
    }
    Ok(())
}

async fn load_run(db: &DatabaseConnection, event: &FactoryEvent) -> Result<Option<RunView>> {
    let (repo, issue) = match event {
        FactoryEvent::Ready { repo, issue, .. } | FactoryEvent::BudgetExpired { repo, issue } => {
            (repo.clone(), *issue)
        }
        FactoryEvent::CheckSuite { repo, branch, .. } => {
            let issue = branch
                .strip_prefix("factory/")
                .and_then(|s| s.parse::<i64>().ok());
            let Some(issue) = issue else {
                return Ok(None);
            };
            (repo.clone(), issue)
        }
        FactoryEvent::PullRequestClosed {
            repo,
            branch,
            issue,
            ..
        } => {
            let issue = issue.or_else(|| {
                branch
                    .strip_prefix("factory/")
                    .and_then(|s| s.parse::<i64>().ok())
            });
            let Some(issue) = issue else {
                return Ok(None);
            };
            (repo.clone(), issue)
        }
    };
    let row = factory_runs::Entity::find()
        .filter(factory_runs::Column::Repo.eq(repo))
        .filter(factory_runs::Column::Issue.eq(issue))
        .one(db)
        .await
        .context("load factory_run")?;
    Ok(row.map(model_to_view))
}

async fn save_run(db: &DatabaseConnection, after: &RunView, now: i64) -> Result<()> {
    let row = factory_runs::Entity::find()
        .filter(factory_runs::Column::Repo.eq(&after.repo))
        .filter(factory_runs::Column::Issue.eq(after.issue))
        .one(db)
        .await
        .context("load factory_run for save")?;
    if let Some(row) = row {
        let mut am: factory_runs::ActiveModel = row.into();
        am.state = Set(after.state.as_str().to_owned());
        am.branch = Set(Some(after.branch.clone()));
        am.pr = Set(after.pr);
        am.task_room = Set(after.task_room.clone());
        am.review_room = Set(after.review_room.clone());
        am.ci_attempts = Set(after.ci_attempts);
        am.review_attempts = Set(after.review_attempts);
        am.review_head = Set(after.review_head.clone());
        am.budget_ends_at = Set(after.budget_ends_at);
        am.updated_at = Set(now);
        am.update(db).await.context("update factory_run")?;
    } else {
        let am = factory_runs::ActiveModel {
            repo: Set(after.repo.clone()),
            issue: Set(after.issue),
            state: Set(after.state.as_str().to_owned()),
            branch: Set(Some(after.branch.clone())),
            pr: Set(after.pr),
            task_room: Set(after.task_room.clone()),
            review_room: Set(after.review_room.clone()),
            ci_attempts: Set(after.ci_attempts),
            review_attempts: Set(after.review_attempts),
            review_head: Set(after.review_head.clone()),
            budget_ends_at: Set(after.budget_ends_at),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        am.insert(db).await.context("insert factory_run")?;
    }
    Ok(())
}

fn model_to_view(m: factory_runs::Model) -> RunView {
    RunView {
        repo: m.repo,
        issue: m.issue,
        state: machine::RunState::parse(&m.state).unwrap_or(machine::RunState::Blocked),
        branch: m.branch.unwrap_or_default(),
        pr: m.pr,
        task_room: m.task_room,
        review_room: m.review_room,
        ci_attempts: m.ci_attempts,
        review_attempts: m.review_attempts,
        review_head: m.review_head,
        budget_ends_at: m.budget_ends_at,
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}
