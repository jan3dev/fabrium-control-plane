//! Typed normalizer for the GitHub webhook events the factory cares about.

use serde::Deserialize;
use serde_json::Value;

/// An event the factory state machine understands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactoryEvent {
    /// An issue gained `factory:ready` (or already had it on open/edit with the headings).
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
        /// True when every required check (CI + checker) passed.
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
    labels: Option<Vec<Label>>,
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
        // do not drive the state machine yet; the reconciler covers drift.
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
    let labels = p.issue.labels.as_deref().unwrap_or(&[]);
    let has_ready = labels.iter().any(|l| l.name == "factory:ready");
    let labeled_ready =
        p.action == "labeled" && p.label.as_ref().is_some_and(|l| l.name == "factory:ready");
    let opened_or_edited_ready = matches!(p.action.as_str(), "opened" | "edited") && has_ready;
    if !(labeled_ready || opened_or_edited_ready) {
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
    let all_green = conclusion == "success";
    Some(FactoryEvent::CheckSuite {
        repo: p.repository.full_name,
        branch,
        sha,
        conclusion,
        all_green,
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
