# Agent factory: GitHub listener and dispatcher

The control plane receives GitHub App webhooks and runs one **factory run** per issue as a state machine. It wakes zooid agents by posting mentions in Matrix rooms.

This document is the control-plane half of the factory design.

## Flow

```text
GitHub App ──POST /v1/github/webhook──▶ HMAC + delivery dedupe
                                          │
                                          ▼
                                    FactoryDispatcher
                                          │
                    ┌─────────────────────┼─────────────────────┐
                    ▼                     ▼                     ▼
              factory_runs          GitHub labels         Matrix rooms
              (SQLite)              + comments            (wake agents)
                    ▲
                    │
              Reconciler (5 min): budget expiry, later drift catch-up
```

## Endpoint

`POST /v1/github/webhook` is public (no tenant API key). Authentication is `X-Hub-Signature-256` (HMAC-SHA256 of the raw body with `GITHUB_WEBHOOK_SECRET`), compared in constant time.

- Missing or bad signature → `403`
- Duplicate `X-GitHub-Delivery` → `200` with `{"ok":true,"duplicate":true}`. A delivery whose dispatch failed is not a duplicate.
- Repository not in `FACTORY_REPOS` → `200` with `ignored: repo`
- Factory env not configured → `404`
- Accepted events return `200` immediately; dispatch runs in a background task (the global 10 s request timeout must not cut GitHub’s wait)

Subscribed events: `issues`, `issue_comment`, `check_suite`, `pull_request`, `pull_request_review`. Only a subset drives the machine today (`issues` labeled `factory:ready`, completed `check_suite` on `factory/<n>`, closed `pull_request` on that branch).

## State machine (one row in `factory_runs`)

| State | Meaning |
|-------|---------|
| `implementing` | Task room exists; implementer owns the branch `factory/<issue>` |
| `review` | Separate review room; reviewer fixes, `/pr`, merges |
| `blocked` | Attempt or time budget exhausted, or merge refused |
| `done` | PR merged; task room archived |

Transitions (pure function in `src/factory/machine.rs`):

"CI" is the combined result of every check suite on the head SHA. GitHub Actions makes one suite per workflow, so `factory-ci` and `factory-checker` arrive as separate `check_suite` events. On each one, the dispatcher lists all suites on that SHA. It does nothing while a suite still runs. When all have completed, CI is green only if each suite is `success`, `neutral`, or `skipped`. Suites without check runs do not count. Two suites that finish together can both report the same red SHA, so a red result on the SHA recorded in `ci_failed_sha` is ignored.

1. **ready → implementing** — Issue has `factory:ready` and headings **Goal** and **Acceptance criteria**. The dispatcher creates `#<repo>/<issue>`, invites the implementer, and posts the start prompt (`/tdd`, branch, worktree). Then it saves the run row and swaps the labels to `factory:implementing`.
2. **implementing + red CI** — Re-wake implementer; `ci_attempts` += 1. Cap **3** → `blocked`.
3. **implementing + green CI** — Create a **new** review room (empty context) and wake the reviewer → `review`. The reviewed commit is recorded as `review_head`.
4. **review + green CI** — The first wake-up already tells the reviewer to `/pr` and merge when it has no findings. So a green result on `review_head` is ignored, including repeats. A green result on a new SHA (the reviewer pushed fixes) records it as `review_head` and wakes the reviewer to `/pr`, set the blast-radius label, and merge when mergeable.
5. **review + red CI** — Re-wake the reviewer in the review room to fix and push; `review_attempts` += 1. Cap **5** → `blocked`.
6. **PR merged** → `done`, archive task room.
7. **Budget expired** (reconciler) → `blocked`.

The dispatcher saves a state change (keyed on repo + issue) only after the wake-up for it succeeds. One lock serializes dispatch, so two events for one run never act at the same time. If an action fails, the webhook handler deletes the delivery id. A GitHub redelivery of that event then runs the step again, and room creation reuses the room that already has the alias. A retry can post the same prompt twice. The reconciler does not redeliver yet, so a failed step waits for a manual redelivery or the time budget. GitHub installation access tokens are minted per repo and cached in memory for ~50 minutes.

Loop guards:

- Ignore `factory:ready` events whose sender is a Bot (own label writes).
- Attempt caps and a per-run time budget (`budget_ends_at`, default 24 h from start).
- Humans promote `factory:proposed` issues; the factory never sets `factory:ready` on its own proposals.

## Config

All of these together enable the factory; a partial set refuses to start. Omit all of them to leave the VM API unchanged.

| Variable | Role |
|----------|------|
| `GITHUB_APP_ID` | App id for JWTs |
| `GITHUB_APP_PRIVATE_KEY_FILE` or `GITHUB_APP_PRIVATE_KEY` | PEM for App JWTs |
| `GITHUB_WEBHOOK_SECRET` | HMAC secret |
| `FACTORY_REPOS` | Comma-separated `owner/name` allowlist |
| `MATRIX_HOMESERVER` | e.g. `https://fabrium.net` |
| `MATRIX_ACCESS_TOKEN` | Integration bot token |
| `MATRIX_IMPLEMENTER` / `MATRIX_REVIEWER` | Matrix user ids to mention |

Webhook URL: `https://<CP_HOST>/v1/github/webhook`.

## Infra and pilot (outside this repo)

1. Register the GitHub App; install it on the sandbox; store the PEM and webhook secret on the control host.
2. Create a Matrix bot; add a second zooid agent (reviewer). Confirm zooid reacts to bot mentions and keeps one session per room.
3. Sandbox repo: CI + checker, `CODING_STANDARDS.md`, labels, branch rule without required reviews. Acceptance: label an issue `factory:ready` and watch the merge with no further human action; then exercise CI-cap, checker, and merge-block failure paths.
