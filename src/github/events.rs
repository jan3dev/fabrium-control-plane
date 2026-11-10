//! Typed normalizer for the GitHub webhook events the factory cares about.

use serde::Deserialize;
use serde_json::Value;

/// An event the factory state machine understands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactoryEvent {
    /// An issue gained `factory:ready`.
    Ready {
        repo: String,
        issue: i64,
        title: String,
        body: String,
        /// When true, the sender is our GitHub App (ignore for loop guards on labels we set).
        from_app: bool,
        label: Option<String>,
    },
    /// A check suite finished for a `factory/<issue>` branch.
    CheckSuite {
        repo: String,
        branch: String,
        /// Head sha the suite ran against; gates duplicate green events in review.
        sha: String,
        /// GitHub conclusion: success, failure, cancelled, timed_out, …
        conclusion: String,
        /// True when every check suite on `sha` passed. One webhook sees one suite
        /// (GitHub Actions makes one per workflow), so the dispatcher replaces this
        /// and `conclusion` with [`combine_check_suites`] before the transition.
        all_green: bool,
    },
    /// A pull request closed. `merged` distinguishes a merge from a plain close.
    PullRequestClosed {
        repo: String,
        branch: String,
        pr: i64,
        merged: bool,
        issue: Option<i64>,
    },
    /// Reconciler-only: the run's budget ended.
    BudgetExpired { repo: String, issue: i64 },
}

#[derive(Debug, Deserialize)]
struct RepoFull {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct Issue {
    number: i64,
    title: String,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Label {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Sender {
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IssuesPayload {
    action: String,
    issue: Issue,
    repository: RepoFull,
    label: Option<Label>,
    sender: Option<Sender>,
}

#[derive(Debug, Deserialize)]
struct CheckSuitePayload {
    action: String,
    check_suite: CheckSuite,
    repository: RepoFull,
}

#[derive(Debug, Deserialize)]
struct CheckSuite {
    conclusion: Option<String>,
    head_branch: Option<String>,
    head_sha: Option<String>,
    /// Present on completed suites; unused beyond logging for now.
    #[allow(dead_code)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PullRequestPayload {
    action: String,
    pull_request: PullRequest,
    repository: RepoFull,
}

#[derive(Debug, Deserialize)]
struct PullRequest {
    number: i64,
    merged: Option<bool>,
    head: PrHead,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PrHead {
    #[serde(rename = "ref")]
    branch: String,
}

/// Turn a GitHub webhook `X-GitHub-Event` + JSON body into zero or one factory event.
pub fn normalize(event_name: &str, body: &Value) -> Option<FactoryEvent> {
    match event_name {
        "issues" => normalize_issues(body),
        "check_suite" => normalize_check_suite(body),
        "pull_request" => normalize_pull_request(body),
        // issue_comment and pull_request_review are accepted by the webhook but
        // do not drive the state machine yet.
        "issue_comment" | "pull_request_review" | "ping" => None,
        _ => None,
    }
}

fn normalize_issues(body: &Value) -> Option<FactoryEvent> {
    let p: IssuesPayload = serde_json::from_value(body.clone()).ok()?;
    let from_app = p
        .sender
        .as_ref()
        .is_some_and(|s| s.kind.as_deref() == Some("Bot"));
    // Only `labeled`: adding a label needs triage access, editing an issue body does not.
    // An issue created with the label also sends `labeled`.
    let labeled_ready =
        p.action == "labeled" && p.label.as_ref().is_some_and(|l| l.name == "factory:ready");
    if !labeled_ready {
        return None;
    }
    Some(FactoryEvent::Ready {
        repo: p.repository.full_name,
        issue: p.issue.number,
        title: p.issue.title,
        body: p.issue.body.unwrap_or_default(),
        from_app,
        label: p.label.map(|l| l.name),
    })
}

fn normalize_check_suite(body: &Value) -> Option<FactoryEvent> {
    let p: CheckSuitePayload = serde_json::from_value(body.clone()).ok()?;
    if p.action != "completed" {
        return None;
    }
    let branch = p.check_suite.head_branch?;
    let sha = p.check_suite.head_sha?;
    if !branch.starts_with("factory/") {
        return None;
    }
    let conclusion = p.check_suite.conclusion.unwrap_or_else(|| "neutral".into());
    let all_green = is_passing(&conclusion);
    Some(FactoryEvent::CheckSuite {
        repo: p.repository.full_name,
        branch,
        sha,
        conclusion,
        all_green,
    })
}

/// GitHub counts a skipped or neutral required check as passing.
fn is_passing(conclusion: &str) -> bool {
    matches!(conclusion, "success" | "neutral" | "skipped")
}

/// The settled result of every check suite on one commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckVerdict {
    pub all_green: bool,
    /// `success`, or the conclusion of the first suite that did not pass.
    pub conclusion: String,
}

#[derive(Debug, Deserialize)]
struct CheckSuiteList {
    check_suites: Vec<ListedSuite>,
}

#[derive(Debug, Deserialize)]
struct ListedSuite {
    status: Option<String>,
    conclusion: Option<String>,
    latest_check_runs_count: Option<u64>,
}

/// Fold a `GET /repos/{repo}/commits/{sha}/check-suites` body into one verdict.
/// Returns `None` while a suite with check runs is still running. Suites without
/// check runs are skipped: GitHub leaves them queued when no workflow matched.
pub fn combine_check_suites(body: &Value) -> Option<CheckVerdict> {
    let list: CheckSuiteList = serde_json::from_value(body.clone()).ok()?;
    let suites: Vec<_> = list
        .check_suites
        .iter()
        .filter(|s| s.latest_check_runs_count.unwrap_or(0) > 0)
        .collect();
    if suites.is_empty()
        || suites
            .iter()
            .any(|s| s.status.as_deref() != Some("completed"))
    {
        return None;
    }
    let failed = suites
        .iter()
        .map(|s| s.conclusion.as_deref().unwrap_or("neutral"))
        .find(|c| !is_passing(c));
    Some(CheckVerdict {
        all_green: failed.is_none(),
        conclusion: failed.unwrap_or("success").to_owned(),
    })
}

fn normalize_pull_request(body: &Value) -> Option<FactoryEvent> {
    let p: PullRequestPayload = serde_json::from_value(body.clone()).ok()?;
    if p.action != "closed" {
        return None;
    }
    let branch = p.pull_request.head.branch;
    if !branch.starts_with("factory/") {
        return None;
    }
    let issue = branch
        .strip_prefix("factory/")
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| issue_from_body(p.pull_request.body.as_deref()));
    Some(FactoryEvent::PullRequestClosed {
        repo: p.repository.full_name,
        branch,
        pr: p.pull_request.number,
        merged: p.pull_request.merged.unwrap_or(false),
        issue,
    })
}

fn issue_from_body(body: Option<&str>) -> Option<i64> {
    let body = body?;
    // Common "Fixes #N" / "Closes #N" forms.
    for token in body.split_whitespace() {
        if token.contains('#')
            && let Ok(n) = token
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse::<i64>()
        {
            return Some(n);
        }
    }
    None
}

/// An issue is ready only when it has Goal and Acceptance criteria headings.
pub fn has_ready_headings(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let has_goal = lower.contains("# goal") || lower.contains("## goal");
    let has_ac = lower.contains("acceptance criteria");
    has_goal && has_ac
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn labeled_ready_becomes_event() {
        let body = json!({
            "action": "labeled",
            "issue": {
                "number": 7,
                "title": "Do the thing",
                "body": "## Goal\n\nx\n\n## Acceptance criteria\n\n- y",
                "labels": [{"name": "factory:ready"}]
            },
            "label": {"name": "factory:ready"},
            "repository": {"full_name": "acme/sandbox"},
            "sender": {"type": "User"}
        });
        let ev = normalize("issues", &body).unwrap();
        assert!(matches!(
            ev,
            FactoryEvent::Ready {
                issue: 7,
                from_app: false,
                ..
            }
        ));
        assert!(has_ready_headings("## Goal\n\n## Acceptance criteria\n"));
    }

    #[test]
    fn edit_of_labeled_issue_is_ignored() {
        let body = json!({
            "action": "edited",
            "issue": {
                "number": 7,
                "title": "t",
                "body": "## Goal\n\nx\n\n## Acceptance criteria\n\n- y",
                "labels": [{"name": "factory:ready"}]
            },
            "repository": {"full_name": "acme/sandbox"},
            "sender": {"type": "User"}
        });
        assert!(normalize("issues", &body).is_none());
    }

    #[test]
    fn other_label_is_ignored() {
        let body = json!({
            "action": "labeled",
            "issue": {"number": 1, "title": "t", "body": "", "labels": []},
            "label": {"name": "bug"},
            "repository": {"full_name": "acme/sandbox"},
            "sender": {"type": "User"}
        });
        assert!(normalize("issues", &body).is_none());
    }

    #[test]
    fn check_suite_on_factory_branch() {
        let body = json!({
            "action": "completed",
            "check_suite": {
                "conclusion": "failure",
                "head_branch": "factory/3",
                "head_sha": "abc123",
                "status": "completed"
            },
            "repository": {"full_name": "acme/sandbox"}
        });
        match normalize("check_suite", &body).unwrap() {
            FactoryEvent::CheckSuite {
                repo,
                branch,
                sha,
                conclusion,
                all_green: false,
            } => {
                assert_eq!(repo, "acme/sandbox");
                assert_eq!(branch, "factory/3");
                assert_eq!(sha, "abc123");
                assert_eq!(conclusion, "failure");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn skipped_suite_counts_as_passing() {
        let body = json!({
            "action": "completed",
            "check_suite": {
                "conclusion": "skipped",
                "head_branch": "factory/3",
                "head_sha": "abc123",
                "status": "completed"
            },
            "repository": {"full_name": "acme/sandbox"}
        });
        assert!(matches!(
            normalize("check_suite", &body),
            Some(FactoryEvent::CheckSuite {
                all_green: true,
                ..
            })
        ));
    }

    fn suite_list(suites: &[(&str, Option<&str>, u64)]) -> Value {
        let suites: Vec<Value> = suites
            .iter()
            .map(|(status, conclusion, runs)| {
                json!({
                    "status": status,
                    "conclusion": conclusion,
                    "latest_check_runs_count": runs
                })
            })
            .collect();
        json!({ "total_count": suites.len(), "check_suites": suites })
    }

    #[test]
    fn combined_waits_for_running_suite() {
        let body = suite_list(&[("completed", Some("success"), 1), ("in_progress", None, 1)]);
        assert_eq!(combine_check_suites(&body), None);
    }

    #[test]
    fn combined_green_needs_every_suite() {
        let body = suite_list(&[
            ("completed", Some("success"), 1),
            ("completed", Some("skipped"), 1),
            ("queued", None, 0),
        ]);
        assert_eq!(
            combine_check_suites(&body),
            Some(CheckVerdict {
                all_green: true,
                conclusion: "success".into()
            })
        );
    }

    #[test]
    fn combined_red_names_failing_conclusion() {
        let body = suite_list(&[
            ("completed", Some("success"), 1),
            ("completed", Some("timed_out"), 2),
        ]);
        assert_eq!(
            combine_check_suites(&body),
            Some(CheckVerdict {
                all_green: false,
                conclusion: "timed_out".into()
            })
        );
    }

    #[test]
    fn combined_without_runs_is_unsettled() {
        assert_eq!(
            combine_check_suites(&suite_list(&[("queued", None, 0)])),
            None
        );
    }

    #[test]
    fn check_suite_without_sha_is_ignored() {
        let body = json!({
            "action": "completed",
            "check_suite": {
                "conclusion": "success",
                "head_branch": "factory/3",
                "status": "completed"
            },
            "repository": {"full_name": "acme/sandbox"}
        });
        assert!(normalize("check_suite", &body).is_none());
    }

    #[test]
    fn merged_pr_maps_issue_from_branch() {
        let body = json!({
            "action": "closed",
            "pull_request": {
                "number": 9,
                "merged": true,
                "head": {"ref": "factory/4"},
                "body": "Fixes the thing"
            },
            "repository": {"full_name": "acme/sandbox"}
        });
        match normalize("pull_request", &body).unwrap() {
            FactoryEvent::PullRequestClosed {
                merged: true,
                issue: Some(4),
                pr: 9,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    }
}
