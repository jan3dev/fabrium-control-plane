//! Pure `(run, event) -> (run, actions)` transition for one factory issue.

use crate::github::events::{FactoryEvent, has_ready_headings};

/// Cap on CI re-wakes while implementing.
pub const CI_ATTEMPT_CAP: i32 = 3;

/// Cap on review cycles before the run blocks.
pub const REVIEW_ATTEMPT_CAP: i32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Implementing,
    Review,
    Blocked,
    Done,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implementing => "implementing",
            Self::Review => "review",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "implementing" => Some(Self::Implementing),
            "review" => Some(Self::Review),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Implementing => "factory:implementing",
            Self::Review => "factory:review",
            Self::Blocked => "factory:blocked",
            Self::Done => "factory:done",
        }
    }
}

/// Snapshot the transition function mutates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunView {
    pub repo: String,
    pub issue: i64,
    pub state: RunState,
    pub branch: String,
    pub pr: Option<i64>,
    pub task_room: Option<String>,
    pub review_room: Option<String>,
    pub ci_attempts: i32,
    pub review_attempts: i32,
    pub review_head: Option<String>,
    pub budget_ends_at: Option<i64>,
}

/// Side effects the dispatcher applies after a transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Create `#<repo>/<issue>`, invite the implementer, post the start prompt.
    StartImplementing {
        repo: String,
        issue: i64,
        title: String,
        body: String,
        branch: String,
    },
    /// Re-wake the implementer in the task room with a CI failure summary.
    RewakeImplementer {
        repo: String,
        issue: i64,
        conclusion: String,
        ci_attempts: i32,
    },
    /// Create a fresh review room and wake the reviewer there.
    StartReview {
        repo: String,
        issue: i64,
        branch: String,
    },
    /// Re-wake the reviewer in the review room with a CI failure summary.
    RewakeReviewer {
        conclusion: String,
        review_attempts: i32,
    },
    /// Ask the reviewer to open `/pr`, set the blast-radius label, and merge.
    FinishReview {
        repo: String,
        issue: i64,
        branch: String,
    },
    /// Set the GitHub state label and drop the previous factory:* state labels.
    SetStateLabel {
        repo: String,
        issue: i64,
        state: RunState,
    },
    /// Drop `factory:ready` once work has started.
    RemoveReadyLabel { repo: String, issue: i64 },
    /// Leave a comment on the issue explaining a block.
    Comment {
        repo: String,
        issue: i64,
        body: String,
    },
    /// Archive the task room after a merge.
    ArchiveTaskRoom { room: String },
}

/// Apply `event` to an optional existing run. `now` is unix seconds.
pub fn transition(
    run: Option<RunView>,
    event: &FactoryEvent,
    now: i64,
) -> Option<(RunView, Vec<Action>)> {
    match event {
        FactoryEvent::Ready {
            repo,
            issue,
            title,
            body,
            from_app,
            ..
        } => {
            if *from_app {
                return None;
            }
            if run.is_some() {
                return None;
            }
            if !has_ready_headings(body) {
                // Ephemeral: `should_persist` is false, so only the comment runs.
                return Some((
                    RunView {
                        repo: repo.clone(),
                        issue: *issue,
                        state: RunState::Blocked,
                        branch: format!("factory/{issue}"),
                        pr: None,
                        task_room: None,
                        review_room: None,
                        ci_attempts: 0,
                        review_attempts: 0,
                        review_head: None,
                        budget_ends_at: None,
                    },
                    vec![Action::Comment {
                        repo: repo.clone(),
                        issue: *issue,
                        body: "factory:ready needs headings **Goal** and **Acceptance criteria**."
                            .into(),
                    }],
                ));
            }
            let branch = format!("factory/{issue}");
            let view = RunView {
                repo: repo.clone(),
                issue: *issue,
                state: RunState::Implementing,
                branch: branch.clone(),
                pr: None,
                task_room: None,
                review_room: None,
                ci_attempts: 0,
                review_attempts: 0,
                review_head: None,
                budget_ends_at: Some(now + 24 * 3600),
            };
            Some((
                view.clone(),
                vec![
                    Action::RemoveReadyLabel {
                        repo: repo.clone(),
                        issue: *issue,
                    },
                    Action::SetStateLabel {
                        repo: repo.clone(),
                        issue: *issue,
                        state: RunState::Implementing,
                    },
                    Action::StartImplementing {
                        repo: repo.clone(),
                        issue: *issue,
                        title: title.clone(),
                        body: body.clone(),
                        branch,
                    },
                ],
            ))
        }
        FactoryEvent::CheckSuite {
            repo,
            branch,
            sha,
            conclusion,
            all_green,
        } => {
            let mut run = run?;
            if run.repo != *repo || run.branch != *branch {
                return None;
            }
            if run.budget_ends_at.is_some_and(|t| now > t) {
                let state = run.state.as_str();
                return block(run, format!("time budget expired during {state}"));
            }
            match run.state {
                RunState::Implementing if !*all_green => {
                    run.ci_attempts += 1;
                    if run.ci_attempts >= CI_ATTEMPT_CAP {
                        return block(
                            run,
                            format!(
                                "CI failed {CI_ATTEMPT_CAP} times (last conclusion: {conclusion})"
                            ),
                        );
                    }
                    let actions = vec![Action::RewakeImplementer {
                        repo: run.repo.clone(),
                        issue: run.issue,
                        conclusion: conclusion.clone(),
                        ci_attempts: run.ci_attempts,
                    }];
                    Some((run, actions))
                }
                RunState::Implementing if *all_green => {
                    run.state = RunState::Review;
                    run.review_head = Some(sha.clone());
                    Some((
                        run.clone(),
                        vec![
                            Action::SetStateLabel {
                                repo: run.repo.clone(),
                                issue: run.issue,
                                state: RunState::Review,
                            },
                            Action::StartReview {
                                repo: run.repo.clone(),
                                issue: run.issue,
                                branch: run.branch.clone(),
                            },
                        ],
                    ))
                }
                RunState::Review if !*all_green => {
                    run.review_attempts += 1;
                    if run.review_attempts >= REVIEW_ATTEMPT_CAP {
                        return block(
                            run,
                            format!(
                                "review CI failed {REVIEW_ATTEMPT_CAP} times (last: {conclusion})"
                            ),
                        );
                    }
                    let actions = vec![Action::RewakeReviewer {
                        conclusion: conclusion.clone(),
                        review_attempts: run.review_attempts,
                    }];
                    Some((run, actions))
                }
                RunState::Review if *all_green => {
                    // A repeat green suite on the same head sha must not re-finish review.
                    if run.review_head.as_deref() == Some(sha.as_str()) {
                        return Some((run, vec![]));
                    }
                    run.review_head = Some(sha.clone());
                    let actions = vec![Action::FinishReview {
                        repo: run.repo.clone(),
                        issue: run.issue,
                        branch: run.branch.clone(),
                    }];
                    Some((run, actions))
                }
                _ => None,
            }
        }
        FactoryEvent::PullRequestClosed {
            repo,
            branch,
            pr,
            merged,
            issue,
        } => {
            let mut run = run?;
            if run.repo != *repo {
                return None;
            }
            if run.branch != *branch {
                if let Some(issue) = issue {
                    if run.issue != *issue {
                        return None;
                    }
                } else {
                    return None;
                }
            }
            run.pr = Some(*pr);
            if *merged {
                run.state = RunState::Done;
                let mut actions = vec![Action::SetStateLabel {
                    repo: run.repo.clone(),
                    issue: run.issue,
                    state: RunState::Done,
                }];
                if let Some(room) = run.task_room.clone() {
                    actions.push(Action::ArchiveTaskRoom { room });
                }
                Some((run, actions))
            } else {
                block(run, format!("pull request #{pr} closed without merge"))
            }
        }
        FactoryEvent::BudgetExpired { repo, issue } => {
            let run = run?;
            if run.repo != *repo || run.issue != *issue {
                return None;
            }
            if matches!(run.state, RunState::Done | RunState::Blocked) {
                return None;
            }
            block(run, "time budget expired".into())
        }
    }
}

fn block(mut run: RunView, reason: String) -> Option<(RunView, Vec<Action>)> {
    run.state = RunState::Blocked;
    Some((
        run.clone(),
        vec![
            Action::SetStateLabel {
                repo: run.repo.clone(),
                issue: run.issue,
                state: RunState::Blocked,
            },
            Action::Comment {
                repo: run.repo.clone(),
                issue: run.issue,
                body: format!("factory blocked: {reason}"),
            },
        ],
    ))
}

/// Whether the transition result should be written to `factory_runs`.
/// Missing headings produce a comment without creating a run.
pub fn should_persist(before: &Option<RunView>, after: &RunView, event: &FactoryEvent) -> bool {
    if before.is_some() {
        return true;
    }
    match event {
        FactoryEvent::Ready { body, .. } => {
            has_ready_headings(body) || after.state != RunState::Blocked
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(body: &str) -> FactoryEvent {
        FactoryEvent::Ready {
            repo: "acme/sandbox".into(),
            issue: 3,
            title: "Ship it".into(),
            body: body.into(),
            from_app: false,
            label: Some("factory:ready".into()),
        }
    }

    fn good_body() -> &'static str {
        "## Goal\n\nDo it.\n\n## Acceptance criteria\n\n- done\n"
    }

    fn suite(conclusion: &str, all_green: bool, sha: &str) -> FactoryEvent {
        FactoryEvent::CheckSuite {
            repo: "acme/sandbox".into(),
            branch: "factory/3".into(),
            sha: sha.into(),
            conclusion: conclusion.into(),
            all_green,
        }
    }

    #[test]
    fn ready_starts_implementing() {
        let (run, actions) = transition(None, &ready(good_body()), 1_000).unwrap();
        assert_eq!(run.state, RunState::Implementing);
        assert_eq!(run.branch, "factory/3");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::StartImplementing { .. }))
        );
        assert!(should_persist(&None, &run, &ready(good_body())));
    }

    #[test]
    fn ready_without_headings_comments_only() {
        let event = ready("no headings");
        let (run, actions) = transition(None, &event, 1_000).unwrap();
        assert_eq!(run.state, RunState::Blocked);
        assert!(matches!(actions[0], Action::Comment { .. }));
        assert!(!should_persist(&None, &run, &event));
    }

    #[test]
    fn ignores_app_sender() {
        let mut event = ready(good_body());
        if let FactoryEvent::Ready { from_app, .. } = &mut event {
            *from_app = true;
        }
        assert!(transition(None, &event, 1_000).is_none());
    }

    #[test]
    fn ci_failures_cap_at_three() {
        let (run, _) = transition(None, &ready(good_body()), 1_000).unwrap();
        let mut run = Some(run);
        for i in 1..=2 {
            let ev = suite("failure", false, "aaa111");
            let (next, actions) = transition(run, &ev, 1_000 + i).unwrap();
            assert_eq!(next.ci_attempts, i as i32);
            assert!(matches!(actions[0], Action::RewakeImplementer { .. }));
            run = Some(next);
        }
        let ev = suite("failure", false, "aaa111");
        let (next, actions) = transition(run, &ev, 1_003).unwrap();
        assert_eq!(next.state, RunState::Blocked);
        assert!(actions.iter().any(|a| matches!(a, Action::Comment { .. })));
    }

    #[test]
    fn green_ci_opens_review() {
        let (run, _) = transition(None, &ready(good_body()), 1_000).unwrap();
        let ev = suite("success", true, "abc123");
        let (next, actions) = transition(Some(run), &ev, 1_100).unwrap();
        assert_eq!(next.state, RunState::Review);
        assert_eq!(next.review_head.as_deref(), Some("abc123"));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::StartReview { .. }))
        );
    }

    #[test]
    fn review_red_ci_rewakes_reviewer() {
        let (mut run, _) = transition(None, &ready(good_body()), 1_000).unwrap();
        run.state = RunState::Review;
        let (next, actions) =
            transition(Some(run), &suite("failure", false, "bbb222"), 1_100).unwrap();
        assert_eq!(next.state, RunState::Review);
        assert_eq!(next.review_attempts, 1);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::RewakeReviewer { .. }))
        );
    }

    #[test]
    fn review_red_ci_caps_at_five() {
        let (mut run, _) = transition(None, &ready(good_body()), 1_000).unwrap();
        run.state = RunState::Review;
        let mut run = Some(run);
        for i in 1..=4 {
            let ev = suite("failure", false, "bbb222");
            let (next, actions) = transition(run, &ev, 1_000 + i).unwrap();
            assert_eq!(next.state, RunState::Review);
            assert_eq!(next.review_attempts, i as i32);
            assert!(matches!(actions[0], Action::RewakeReviewer { .. }));
            run = Some(next);
        }
        let (next, actions) = transition(run, &suite("failure", false, "bbb222"), 1_005).unwrap();
        assert_eq!(next.state, RunState::Blocked);
        assert!(actions.iter().any(|a| matches!(a, Action::Comment { .. })));
    }

    #[test]
    fn duplicate_green_does_not_refinish_review() {
        let (run, _) = transition(None, &ready(good_body()), 1_000).unwrap();
        let (run, _) = transition(Some(run), &suite("success", true, "aaa111"), 1_100).unwrap();
        assert_eq!(run.review_head.as_deref(), Some("aaa111"));
        let (run, actions) =
            transition(Some(run), &suite("success", true, "aaa111"), 1_200).unwrap();
        assert_eq!(run.state, RunState::Review);
        assert!(actions.is_empty());
        let (run, actions) =
            transition(Some(run), &suite("success", true, "ccc333"), 1_300).unwrap();
        assert_eq!(run.review_head.as_deref(), Some("ccc333"));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::FinishReview { .. }))
        );
        let (_, actions) = transition(Some(run), &suite("success", true, "ccc333"), 1_400).unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn review_green_without_head_finishes_legacy_run() {
        let (mut run, _) = transition(None, &ready(good_body()), 1_000).unwrap();
        run.state = RunState::Review;
        run.review_head = None;
        let (next, actions) =
            transition(Some(run), &suite("success", true, "aaa111"), 1_100).unwrap();
        assert_eq!(next.review_head.as_deref(), Some("aaa111"));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::FinishReview { .. }))
        );
    }

    #[test]
    fn merge_marks_done() {
        let (mut run, _) = transition(None, &ready(good_body()), 1_000).unwrap();
        run.state = RunState::Review;
        run.task_room = Some("!task:fabrium.net".into());
        let ev = FactoryEvent::PullRequestClosed {
            repo: "acme/sandbox".into(),
            branch: "factory/3".into(),
            pr: 12,
            merged: true,
            issue: Some(3),
        };
        let (next, actions) = transition(Some(run), &ev, 2_000).unwrap();
        assert_eq!(next.state, RunState::Done);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ArchiveTaskRoom { .. }))
        );
    }
}
