//! Apply factory side effects via GitHub and Matrix.

use anyhow::{Context, Result};
use reqwest::Method;
use serde_json::json;

use super::machine::RunState;
use super::runtime::FactoryRuntime;

pub async fn create_task_room(
    rt: &FactoryRuntime,
    repo: &str,
    issue: i64,
    title: &str,
) -> Result<String> {
    let alias = room_alias(repo, issue);
    rt.matrix
        .create_task_room(
            &alias,
            &format!("{repo}#{issue}: {title}"),
            &rt.matrix.implementer,
        )
        .await
        .context("create task room")
}

pub async fn wake_implementer(
    rt: &FactoryRuntime,
    room: &str,
    issue: i64,
    branch: &str,
    title: &str,
    body: &str,
) -> Result<()> {
    let prompt = start_prompt(issue, branch, title, body);
    rt.matrix
        .wake(room, &rt.matrix.implementer, &prompt)
        .await
        .context("wake implementer")
}

pub async fn rewake_implementer(
    rt: &FactoryRuntime,
    room: &str,
    conclusion: &str,
    ci_attempts: i32,
) -> Result<()> {
    let prompt = format!(
        "CI failed (conclusion: {conclusion}, attempt {ci_attempts}/{}). \
         Read the failure, fix it with `/tdd`, and push.",
        super::machine::CI_ATTEMPT_CAP
    );
    rt.matrix.wake(room, &rt.matrix.implementer, &prompt).await
}

pub async fn rewake_reviewer(
    rt: &FactoryRuntime,
    room: &str,
    conclusion: &str,
    review_attempts: i32,
) -> Result<()> {
    let prompt = format!(
        "CI failed during review (conclusion: {conclusion}, attempt {review_attempts}/{}). \
         Read the failure, fix it on the branch, and push.",
        super::machine::REVIEW_ATTEMPT_CAP
    );
    rt.matrix.wake(room, &rt.matrix.reviewer, &prompt).await
}

pub async fn create_review_room(rt: &FactoryRuntime, repo: &str, issue: i64) -> Result<String> {
    let alias = format!("{}-review", room_alias(repo, issue));
    rt.matrix
        .create_task_room(
            &alias,
            &format!("{repo}#{issue} review"),
            &rt.matrix.reviewer,
        )
        .await
        .context("create review room")
}

pub async fn wake_reviewer(
    rt: &FactoryRuntime,
    room: &str,
    repo: &str,
    issue: i64,
    branch: &str,
) -> Result<()> {
    let prompt = format!(
        "Review branch `{branch}` for {repo}#{issue}. Fix findings on the branch. \
         When CI is green and findings are clear, `/pr`, set the blast-radius label, \
         and merge when the PR is mergeable."
    );
    rt.matrix
        .wake(room, &rt.matrix.reviewer, &prompt)
        .await
        .context("wake reviewer")
}

pub async fn finish_review(rt: &FactoryRuntime, room: &str, branch: &str) -> Result<()> {
    let prompt = format!(
        "CI is green on `{branch}`. Run `/pr` if needed, set the blast-radius label, \
         and merge when mergeable. If GitHub refuses the merge, comment the rule name."
    );
    rt.matrix.wake(room, &rt.matrix.reviewer, &prompt).await
}

pub async fn set_state_label(
    rt: &FactoryRuntime,
    repo: &str,
    issue: i64,
    state: RunState,
) -> Result<()> {
    for name in [
        "factory:implementing",
        "factory:review",
        "factory:blocked",
        "factory:done",
    ] {
        if name != state.label() {
            let _ = remove_label(rt, repo, issue, name).await;
        }
    }
    add_label(rt, repo, issue, state.label()).await
}

pub async fn remove_ready_label(rt: &FactoryRuntime, repo: &str, issue: i64) -> Result<()> {
    remove_label(rt, repo, issue, "factory:ready").await
}

pub async fn comment(rt: &FactoryRuntime, repo: &str, issue: i64, body: &str) -> Result<()> {
    let token = rt.github_app.token_for_repo(repo).await?;
    let path = format!("/repos/{repo}/issues/{issue}/comments");
    let _: serde_json::Value = rt
        .github_api
        .request_as_install(Method::POST, &path, &token, Some(&json!({ "body": body })))
        .await
        .with_context(|| format!("comment on {repo}#{issue}"))?;
    Ok(())
}

pub async fn archive_room(rt: &FactoryRuntime, room: &str) -> Result<()> {
    rt.matrix.archive(room).await
}

async fn add_label(rt: &FactoryRuntime, repo: &str, issue: i64, label: &str) -> Result<()> {
    let token = rt.github_app.token_for_repo(repo).await?;
    let path = format!("/repos/{repo}/issues/{issue}/labels");
    let _: serde_json::Value = rt
        .github_api
        .request_as_install(Method::POST, &path, &token, Some(&json!([label])))
        .await
        .with_context(|| format!("add label {label} on {repo}#{issue}"))?;
    Ok(())
}

async fn remove_label(rt: &FactoryRuntime, repo: &str, issue: i64, label: &str) -> Result<()> {
    let token = rt.github_app.token_for_repo(repo).await?;
    let encoded = label.replace('/', "%2F").replace(':', "%3A");
    let path = format!("/repos/{repo}/issues/{issue}/labels/{encoded}");
    match rt
        .github_api
        .request_empty_as_install(Method::DELETE, &path, &token, None)
        .await
    {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().contains("404") => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove label {label} on {repo}#{issue}")),
    }
}

fn room_alias(repo: &str, issue: i64) -> String {
    let flat: String = repo
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{flat}-{issue}")
}

fn start_prompt(issue: i64, branch: &str, title: &str, body: &str) -> String {
    format!(
        "Implement {title}. Use `/tdd`, branch `{branch}` (worktree). Issue #{issue}.\n\n{body}"
    )
}
