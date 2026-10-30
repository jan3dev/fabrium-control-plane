# Agent factory: GitHub listener and dispatcher

The control plane receives GitHub App webhooks and runs one **factory run** per issue as a state machine. It wakes zooid agents by posting mentions in Matrix rooms.

This document is the control-plane half of the factory design. Decision records that change Fabrium design rules (agents merge freely; the dispatcher may wake agents) live in the `fabrium` docs repository (`docs/decisions/0002`, `0003`).

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
- Duplicate `X-GitHub-Delivery` → `200` with `{"ok":true,"duplicate":true}`
- Repository not in `FACTORY_REPOS` → `200` with `ignored: repo`
- Factory env not configured → `404`
- Accepted events return `200` immediately; dispatch runs in a background task (the global 10 s request timeout must not cut GitHub’s wait)

Subscribed events: `issues`, `issue_comment`, `check_suite`, `pull_request`, `pull_request_review`. Only a subset drives the machine today (`issues` labeled/opened/edited with `factory:ready`, completed `check_suite` on `factory/<n>`, closed `pull_request` on that branch).

## State machine (one row in `factory_runs`)

| State | Meaning |
|-------|---------|
| `implementing` | Task room exists; implementer owns the branch `factory/<issue>` |
| `review` | Separate review room; reviewer fixes, `/pr`, merges |
| `blocked` | Attempt or time budget exhausted, or merge refused |
| `done` | PR merged; task room archived |

Transitions (pure function in `src/factory/machine.rs`):

1. **ready → implementing** — Issue has `factory:ready` and headings **Goal** and **Acceptance criteria**. Dispatcher creates `#<repo>/<issue>`, persists the run row, then invites the implementer and posts the start prompt (`/tdd`, branch, worktree), swaps labels to `factory:implementing`.
2. **implementing + red CI** — Re-wake implementer; `ci_attempts` += 1. Cap **3** → `blocked`.
3. **implementing + green CI** — Create a **new** review room (empty context), persist it, wake reviewer → `review`. The reviewed commit is recorded as `review_head`.
4. **review + green CI** — Only a check suite whose head SHA equals `review_head` counts; any other run is ignored. Wake reviewer to `/pr`, blast-radius label, merge when mergeable.
5. **review + red CI** — Re-wake the reviewer in the review room to fix and push; `review_attempts` += 1. Cap **5** → `blocked`.
6. **PR merged** → `done`, archive task room.
7. **Budget expired** (reconciler) → `blocked`.

Every state change is persisted (upsert keyed on repo + issue) before the dispatcher wakes an agent, so a crash between room creation and wake-up heals on the next event. GitHub installation access tokens are minted per repo and cached in memory for ~50 minutes.

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
