# VM management, zooid daemon and deploy track: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The zooid daemon serves the VM management browser API under `/_fabrium/v1`: OpenID auth, role gate, control plane proxy, runtime identity admission, and the bootstrap payload. The deploy files route it, configure it, and document it.

**Architecture:** A new folder `packages/cli/src/fabrium-api/` holds one Hono sub-app. `mountFabriumApi(app, deps)` mounts it on `transport.app` before `serve()`, like `mountPushGateway` and `mountWebhookRoutes`, and only when `FABRIUM_CONTROL_PLANE_URL` and `FABRIUM_CONTROL_PLANE_KEY_FILE` are set. Matrix calls reuse `MatrixClient`; the four calls it lacks (OpenID userinfo, whoami, one state event, username availability) are small functions over an injectable `fetch`. The last commit changes only `deploy/`.

**Tech Stack:** TypeScript (ESM, Node 22), Hono 4, `yaml` (already a dependency of `zooid`), `MatrixClient` from `@zooid/transport-matrix`, Vitest 3. No new dependency.

**Spec:** [VM management design](../specs/2026-10-26-vm-management-design.md) (sections "zooid (fabrium-web-ui)", "Errors", "Security notes", "Testing"). Shared contracts, which win over the spec: [contracts](2026-10-26-vm-management-contracts.md) (sections 1 to 4). Work happens in `fabrium-web-ui`, worktree `fabrium-web-ui-worktrees/vm-daemon`, branch `feat/vm-daemon-api`. Nothing under `packages/web`.

## Global Constraints

- Prefix `/_fabrium/v1`. Every route needs `Authorization: Bearer <OpenID access token>`. `401 {"error":"invalid token"}`, `403 {"error":"forbidden"}`. Unmounted when `FABRIUM_CONTROL_PLANE_URL` or `FABRIUM_CONTROL_PLANE_KEY_FILE` is unset (every path gives `404`).
- OpenID check: `GET <internal homeserver>/_matrix/federation/v1/openid/userinfo?access_token=...`; `sub` must be a user of this server; cache valid results 5 minutes at most.
- Role: joined to the workforce space, and power level 90 or more (`admin`), or 100 or more or v12 creator (`owner`). Same thresholds as `packages/web/src/lib/roles.ts`.
- Control plane errors pass through with their status and body. A failed connection gives `502 {"error":"control plane unreachable"}`.
- Admission failure gives `502 {"error":"<message>","step":"<step>"}`, `step` one of `bundle`, `register_appservice`, `whoami`, `register_user`, `join`, `roster`. A retry with the same body continues from the failed step.
- Names: VM and workstation `^[a-z0-9][a-z0-9-]{0,62}$` and not `zooid`; agent `^[a-z][a-z0-9-]{0,31}$`.
- Never log or return the tenant key, `haven_api_key`, `github_token`, the Matrix tokens, or an OpenID token.
- Bundle `.env` and `workforce/zooid.yaml` follow `deploy/add-workstation.sh` and `deploy/agent-vm/zooid.yaml.example`. Payload follows contracts section 3 exactly.
- `ZOOID_COMMIT` is the full 40-character commit of the running image. Missing gives `503 {"error":"ZOOID_COMMIT is not set"}`; no `FABRIUM_HAVEN_MODEL` gives `503 {"error":"FABRIUM_HAVEN_MODEL is not set"}`.
- Code style of the package: no semicolons, single quotes, `.js` import suffixes, sparse comments. Artifacts in English. No private data in tracked files (`example.org`, no real hosts or tokens).
- Conventional Commits, one work-unit commit per task, no trailers, no push. The deploy work unit is the last commit and touches only `deploy/`.

Commands. Run them from the worktree root. `T` means `corepack pnpm -C packages/cli`.

- One file: `T exec vitest run src/fabrium-api/<file>.test.ts`. The folder: `T exec vitest run src/fabrium-api`.
- Checks: `T typecheck`. CI unit set: `T exec vitest run --exclude '**/*integration*.test.ts' --exclude '**/integration/**' --passWithNoTests` (needs `corepack pnpm --filter @zooid/context-mcp build` first, or `build-registry.context.test.ts` fails on a missing `dist/bin.js`; that failure exists on `main` too).

## Review Focus

Inputs and conditions the spec implies and an admin will meet. Each line has a test in the task that owns the code (RF1 to RF5).

1. **`@zooid` is not yet a tuwunel admin.** tuwunel refuses the send to the admin room with `403`, because only members of that room are admins. The step `register_appservice` must fail with a message that names the one-time setup. A command that runs but fails (bad YAML, a duplicate token) shows only as a missing registration: the step `whoami` must time out with a message that names the admin room. Task 5.
2. **A VM name that equals an existing Matrix user** (`ann`, `conduit`). A registration with `sender_localpart: ann` hands the VM token the identity of `@ann`. The create must refuse with `409` before any file or registration. Task 5.
3. **Two creates of one name at once** (a double click, two admins). One bundle, one registration, the same tokens for both. Task 5.
4. **A control plane, or a proxy in front of it, that answers with a non-JSON body** (`413` plain text, `502` HTML). Relay status, body, and content type unchanged. Task 4.
5. **A secret with `'`, `$`, a backtick, a double quote, or a newline.** The line must survive `. file` in bash unchanged; a control character is a `400`, not a corrupt secrets file. Tasks 1 and 6.

---

## Verified tuwunel facts

Source: `matrix-construct/tuwunel` tag `v1.9.3` (stage runs 1.9.3). Paths are under `https://github.com/matrix-construct/tuwunel/blob/v1.9.3/`.

| Fact | Source |
|---|---|
| Command: `!admin appservices register`, the first line. The registration YAML follows in a Markdown code block: the second line starts with three backticks, the last line is three backticks. The parser drops blank lines. | `src/admin/appservice/register.rs`, `src/service/admin/processor.rs` (`parse`) |
| Remove: `!admin appservices unregister <id>`. Only registrations made by the admin command can be removed; file and config registrations cannot. | `src/admin/appservice/mod.rs`, `src/service/appservice/mod.rs` |
| A second register of a loaded ID fails with `Duplicate id` (the docs say "replaces"; the code does not). A duplicate `as_token` also fails. The registration lands in the database and survives restarts. | `src/service/appservice/mod.rs` (`load_appservice`, `register_appservice`), `docs/appservices.md` |
| Admin room alias: `#admins:<server_name>`, room version 11, join rule `invite`. | `src/service/admin/mod.rs`, `src/service/admin/create.rs` |
| A user is an admin when joined to the admin room. There is no power level check. A message that starts with `!admin`, sent in the admin room by a joined local user, runs. The server user's own messages do not. | `src/service/admin/mod.rs` (`user_is_admin`, `is_admin_command`), `src/service/rooms/timeline/append.rs` |
| Make `@zooid` an admin: send `!admin users make-user-admin zooid` in the admin room as an existing admin. tuwunel invites and force-joins the local user and sets power level 100. Reply: `<user> has been granted admin privileges.` Without an admin: start tuwunel once with `--execute "users make_user_admin zooid"`. | `src/admin/user/make_user_admin.rs`, `src/service/admin/grant.rs`, `docs/troubleshooting.md` |
| Reply format: a Markdown `m.notice` from the server user (`@conduit` by default) in the admin room, as a reply to the command. Success: `Appservice registered with ID: <id>`. Failure: `Command failed with error:` and a code block. | `src/service/admin/respond.rs`, `src/admin/appservice/register.rs` |
| `GET /_matrix/federation/v1/openid/userinfo?access_token=` is served even with federation off. It answers `{"sub":"@user:server"}`, or `401` for an unknown or expired token. | `src/api/router.rs` (`register_server_misc_routes`), `src/api/server/openid.rs` |
| `GET /_matrix/client/v3/register/available?username=` with an appservice token: `200` if free, `400 M_USER_IN_USE` if the user exists. The server user and `zooid` exist, so they are refused too. | `src/api/client/register/available.rs` |
| An appservice sender (`@<sender_localpart>`) is always in its own namespace, also when the user existed before. Namespace regexes are unanchored. | `src/service/appservice/registration_info.rs`, `docs/appservices.md` |

Observed on the image that `deploy/docker-compose.yml` pins (digest `sha256:678b7f53…`, `tuwunel 1.9.3`, revision `7801b8ec`, the commit of the tag `v1.9.3`), in the integration check below the tasks:

| Observation | Result |
|---|---|
| `@zooid` sends the command before it is an admin | `PUT .../send/m.room.message` answers `403`. |
| `!admin users make-user-admin zooid` by an admin | Reply, an `m.notice` of `@conduit`: `@zooid:localhost has been granted admin privileges.` |
| `!admin appservices register` with the code block | Reply: `Appservice registered with ID: lima`. `whoami` with the new token answers `@lima` at once. |
| The same command again | Reply: `Command failed with error:` and a code block with `Failed to register appservice: Duplicate id: lima`. |
| `!admin appservices unregister lima` | Reply: `Appservice unregistered.` The token answers `401` at once. |
| tuwunel restart | A registration made by the command survives. |
| A registration file with the ID of a database registration, then a restart | `ERROR service "appservice" aborted: Duplicate id`. This is why `add-workstation.sh` refuses a marked workstation. |
| The daemon, once `@zooid` is in the admin room | Its `[matrix] inbound msg` log line printed the first 200 characters of the command, with the VM tokens. Fixed in `transport-matrix` (no body logged for `!admin`). |

## File structure

All new files are in `packages/cli/src/fabrium-api/` unless noted.

| File | Responsibility |
|---|---|
| `errors.ts` | `ApiError(status, {error, step?})`; the sub-app `onError` turns it into JSON |
| `matrix.ts` | `MatrixConn`, `openIdSub`, `whoami`, `stateEvent`, `usernameAvailable` |
| `auth.ts` | OpenID verifier with cache, role resolver, `requireRole` middleware |
| `control-plane.ts` | `createControlPlane` (key, timeout, unreachable), `relay` |
| `vms.ts` | `fetchVms` (the VM list or the control plane failure), `agentsWithFree` (the `free` rule, shared by `GET /agents` and create) |
| `bundle.ts` | Render and read the VM bundle: `.env`, `zooid.yaml`, registration YAML, marker, `listBundles`, `bundleAgent` |
| `payload.ts` | `buildPayload`, `shellQuote` |
| `admission.ts` | `createAdmission(deps)`: the six steps, resume, serialization |
| `create.ts` | Validate the create body and run create (new or existing agent) |
| `index.ts` | `fabriumApi(deps)`, `mountFabriumApi(app, deps)`, `FabriumApiDeps`, all routes |
| `config.ts` | `loadFabriumApiConfig(env)` |
| `fake-matrix.ts` | Test helper: an in-memory homeserver behind a `fetch`. Not imported by production code |
| `test-api.ts` | Test helper: the whole API on a Hono app, with the fake homeserver, a scripted control plane, and a temporary `ZOOID_HOME` |
| `*.test.ts` | One per module (`payload`, `bundle`, `matrix`, `auth`, `routes`, `admission`, `create`, `config`) |
| `packages/cli/src/daemon/start-daemon.ts` (edit) | Mount in the `shouldBindHttpListener` block |
| `packages/transport-matrix/src/transport.ts`, `transport.test.ts` (edit) | `logInbound` does not log the body of an `!admin` command |
| `.changeset/vm-management-api.md`, `.changeset/admin-commands-not-logged.md` | `zooid` minor; `@zooid/transport-matrix` patch |
| `deploy/Caddyfile`, `docker-compose.yml`, `Dockerfile`, `deploy.sh`, `add-workstation.sh`, `README.md`, `.env.example` (edit) | Task 8 |

## Decisions

- **Place.** The daemon's Hono app lives in `zooid` (`packages/cli`). Hono, `yaml`, and `MatrixClient` are already there, so no dependency is added.
- **Role is read on every request.** Only the OpenID result is cached (5 minutes). A demoted admin loses access at the next call. The role costs two small state reads (`m.room.member` of the user and `m.room.power_levels`) with the central token. The creators of the space are read once and kept: the `m.room.create` content does not name its sender, so for a room from version 12 the daemon reads the full room state once.
- **Config errors.** A key file that is unreadable or empty, a bad URL, a bad room, or a missing `ZOOID_HOME` or `HOMESERVER_URL` stops the daemon at start with a message that names the variable and never the key. The control plane URL must be `https` (`http` only for `localhost`), because create sends the Haven and GitHub secrets. Only `FABRIUM_HAVEN_MODEL` and `ZOOID_COMMIT` (missing, or not 40 hex characters) are runtime `503`s.
- **Secrets in the create body.** Both are trimmed. A control character is a `400`: it would split a line of the secrets file. A blank `github_token` is absent. Persona and project are 200 characters at most.
- **Paths.** `ZOOID_HOME` (passed by compose; the daemon sees the same path as the host) gives `workstations/` and `data/matrix/config/registrations/`. `HOMESERVER_URL` is the public URL written into the bundle `.env`; the internal `transports.matrix.homeserver` is for daemon calls.
- **Resume rules for `agent.mode: new`.** `registrations/<ws>.yaml` exists: `409` (add-workstation.sh owns it). `workstations/<ws>/` exists without the marker: `409`. Marker present and `.env` names another agent: `409`. Marker present and the same agent: resume. No directory: `GET /register/available?username=<ws>` must pass, else `409` (RF2). The marker is written first, so a crash never leaves an unmarked bundle.
- **Steps check state first.** `bundle`: reuse the tokens of an existing `.env`. `register_appservice`: skip when `whoami` with the VM token already answers `@<ws>:<server>` (a second register would fail with `Duplicate id`). `whoami`: poll every 500 ms, 30 s at most. `register_user`: `registerBot` already tolerates `M_USER_IN_USE`. `join`: read the member event; `join` skips, `ban` fails, otherwise invite with the central token (errors ignored: a restricted room needs none) and join with the VM token. `roster`: write only when the state differs.
- **Serialization.** One in-process queue for admissions. `ponytail:` a global queue; per-name locks only if admissions must run in parallel.
- **Bundle text.** Same lines as the script. The first comment line says the daemon wrote the file (the script's line would be false). `.env` mode 600, directories 700, `zooid.yaml` default mode.
- **Registration YAML.** The template `registration-workstation.yaml` without comments. Server name dots escaped for the regex.
- **`existing` mode.** The workstation is the top-level `name`. `agent.name`, when present, is ignored (the web track sends both equal). Not free, no bundle, or a preset other than `opencode`: `400`.
- **`GET /agents`.** `preset` is `null` when `acp` has no `preset` (the echo fixture). A directory without a usable `.env` and `workforce/zooid.yaml` is not a bundle and is skipped.
- **`ZOOID_COMMIT`.** `deploy.sh` exports `git rev-parse HEAD` and passes it as a build arg; the `daemon` stage turns it into `ENV ZOOID_COMMIT`. The image carries its own commit, so a later `up` cannot override it. Compose does not pass it at runtime.
- **Names under a workstation.** A new appservice acts as every user named `<ws>.*`. A fresh admission refuses (`409`) a name when a member of the workforce space has such a name, or when `@<ws>.<agent>` exists. A user outside the space is not seen: the README says so.

---

### Task 1: Bootstrap payload builder

**Files:**
- Create: `payload.ts`, `payload.test.ts`

**Interfaces:**
- Produces:
  ```ts
  export interface BootstrapFile { path: string; mode: string; owner: string; content: string }
  export interface BootstrapPayload { files: BootstrapFile[]; script: string }
  export interface PayloadInput { env: string; yaml: string; havenApiKey: string; havenModel: string; githubToken?: string; commit: string }
  export const FULL_COMMIT_RE: RegExp                 // /^[0-9a-f]{40}$/
  export function shellQuote(value: string): string   // 'x' with each ' as '\''
  export function buildPayload(input: PayloadInput): BootstrapPayload
  ```

- [ ] **Step 1: Write the failing tests** in `payload.test.ts`:
  - `lists the three files with owner ubuntu and mode 600`: paths are `/home/ubuntu/zooid/.env`, `/home/ubuntu/zooid/workforce/zooid.yaml`, `/run/fabrium-agent-secrets.env` in that order; the first two contents equal `env` and `yaml`.
  - `writes the secrets as KEY='value' lines`: `HAVEN_API_KEY='hk'\nHAVEN_MODEL='glm-5-3'\nGH_TOKEN='gh'\n`.
  - `omits GH_TOKEN when the token is empty or absent` (`it.each(['', undefined])`): content is the first two lines only.
  - `quotes a single quote as '\''`: `shellQuote("a'b")` is `'a'\''b'`.
  - `survives sourcing in bash` (RF5): for `it's $HOME "x" \`id\` \\ end`, a value with a newline, and an empty-looking `'`, write the secrets file to a temp dir, run `bash -c 'set -a; . "$0"; set +a; printf %s "$HAVEN_API_KEY"' <file>`, and expect the original value.
  - `builds the script of contracts section 3 with the commit`: equals the exact script text with `--ref <40 hex>` (built in the test from an array of lines).
  - `rejects a commit that is not 40 hex characters` (`it.each(['', 'abc123', 'main', `${'a'.repeat(40)};id`])`): throws.
- [ ] **Step 2: Run to verify they fail.** `T exec vitest run src/fabrium-api/payload.test.ts`. Expected: FAIL (module not found).
- [ ] **Step 3: Implement** `payload.ts`. The script is an array of lines joined with `\n` (a template literal would eat the `\` line continuation and expand `${uid}`). Validate `commit` with `FULL_COMMIT_RE` before it enters the script.
- [ ] **Step 4: Run to verify they pass.** Same command, PASS. `T typecheck` passes.
- [ ] **Step 5: Commit.** `git add packages/cli/src/fabrium-api` then `git commit -m "feat(cli): build the bootstrap payload for agent VMs"`.

---

### Task 2: VM bundle files

**Files:**
- Create: `bundle.ts`, `bundle.test.ts`

**Interfaces:**
- Produces:
  ```ts
  export const MARKER = '.runtime-registration'
  export interface BundleSpec { workstation: string; agent: string; serverName: string; homeserverUrl: string; space: string; rooms: string[] }
  export interface Bundle { agentId: string; asToken: string; hsToken: string; env: string; yaml: string }
  export interface BundleSummary { workstation: string; agent: string; agentId: string; preset: string | null }
  export function renderEnv(spec: BundleSpec, tokens: { asToken: string; hsToken: string }): string
  export function renderYaml(spec: BundleSpec): string
  export function renderRegistration(spec: BundleSpec, tokens: { asToken: string; hsToken: string }): string
  export function withServer(room: string, serverName: string): string   // '#general' -> '#general:example.org'; '!id:x' and '#a:x' unchanged
  export async function ensureBundle(dir: string, spec: BundleSpec): Promise<Bundle>
  export async function readBundle(dir: string): Promise<(Bundle & { summary: BundleSummary }) | null>
  export async function listBundles(root: string): Promise<BundleSummary[]>   // sorted by workstation
  ```

- [ ] **Step 1: Write the failing tests** in `bundle.test.ts` (temp dirs, `example.org`):
  - `renders .env in the format of add-workstation.sh`: exact text, `WORKSTATION`, `AGENT_NAME`, `SERVER_NAME`, `HOMESERVER_URL`, `WORKFORCE_SPACE`, `MATRIX_AS_TOKEN`, `MATRIX_HS_TOKEN`, then the secrets comment.
  - `renders zooid.yaml like deploy/agent-vm/zooid.yaml.example`: read the template (path from `import.meta.url`), replace `${WORKSTATION}`, `${SERVER_NAME}`, `${WORKFORCE_SPACE}`, `${AGENT_NAME}`, `${AGENT_ACP}` (`{ preset: 'opencode' }`), `${AGENT_ROOMS}` (`'#general:example.org'`), and expect equal lines once comment lines are dropped from both.
  - `renders the registration like registration-workstation.yaml`: same comparison with `${WORKSTATION}`, `${WS_AS_TOKEN}`, `${WS_HS_TOKEN}`, `${SERVER_NAME_RE}` (`example\.org`); `yaml.parse` of the output has `id: 'lima'`, `url: null`, `namespaces.users[0] = {exclusive: true, regex: '@lima\\..*:example\\.org'}`.
  - `adds the server name to room aliases and keeps room ids`.
  - `creates the marker, .env, and zooid.yaml with modes 700, 600, default` (stat the dir and `.env`; the marker exists).
  - `reuses the tokens of an existing .env` (two calls, same tokens, `.env` bytes unchanged) and `completes a bundle that lost zooid.yaml` (resume).
  - `reads a bundle and its summary`, `reads preset null for an acp without preset`, `returns null without a .env`.
  - `lists bundles sorted by workstation and skips directories without a usable .env`.
- [ ] **Step 2: Run to verify they fail.** `T exec vitest run src/fabrium-api/bundle.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement** `bundle.ts`. Tokens are `randomBytes(32).toString('hex')` (as `openssl rand -hex 32`). Reuse `readTokens` from `../bootstrap/tokens.js` for an existing `.env`. Write with `flag: 'wx'` so a file is never replaced. Order: directories, marker, `.env`, `zooid.yaml`. The preset comes from `yaml.parse(...).agents[AGENT_NAME].acp.preset`.
- [ ] **Step 4: Run to verify they pass.** Same command, PASS. `T typecheck` passes.
- [ ] **Step 5: Commit.** `git commit -m "feat(cli): write and read agent VM bundles"`.

---

### Task 3: Matrix helpers, OpenID check, and role gate

**Files:**
- Create: `errors.ts`, `matrix.ts`, `auth.ts`, `fake-matrix.ts`, `auth.test.ts`, `matrix.test.ts`

**Interfaces:**
- Produces:
  ```ts
  // errors.ts
  export class ApiError extends Error { constructor(readonly status: ContentfulStatusCode, readonly body: { error: string; step?: string }) }
  // matrix.ts
  export interface MatrixConn { homeserver: string; fetch: typeof fetch }
  export function openIdSub(c: MatrixConn, accessToken: string): Promise<string | null>   // null on 4xx; ApiError 502 on 5xx or network
  export function whoami(c: MatrixConn, token: string): Promise<string | null>
  export function stateEvent(c: MatrixConn, token: string, roomId: string, type: string, stateKey: string): Promise<Record<string, unknown> | null>  // null on 404
  export function usernameAvailable(c: MatrixConn, token: string, localpart: string): Promise<boolean>
  // auth.ts
  export type Role = 'admin' | 'owner'
  export interface AuthDeps { conn: MatrixConn; client: MatrixClient; serverName: string; asToken: string; asUserId: string; getSpaceRoomId(): string | undefined; now?: () => number }
  export function requireRole(deps: AuthDeps): MiddlewareHandler   // sets c.get('caller') = { userId, role }
  export interface Caller { userId: string; role: Role }
  // fake-matrix.ts (test helper)
  export function createFakeMatrix(opts?: { serverName?: string }): FakeMatrix
  ```
  `FakeMatrix` exposes `fetch`, `centralToken`, `openIdToken(userId)`, `setMember(roomId, userId, membership)`, `setPowerLevels(roomId, content)`, `setCreate(roomId, content)`, `spaceId`, and a `calls` log of `{method, path}`. Task 5 extends it.

- [ ] **Step 1: Write the failing tests** in `auth.test.ts` (Hono `app.request`, a probe route behind `requireRole`):
  - `accepts a valid token and sets the caller`: status 200, body has `userId`.
  - `answers 401 {"error":"invalid token"} for an unknown token, no header, and a non-Bearer header` (`it.each`).
  - `rejects a user of another server`: the fake answers `sub: "@x:other.org"`, status 401.
  - `caches a valid result for 5 minutes`: two requests make one userinfo call; with `now` advanced 5 minutes plus 1 ms the third request calls again.
  - `does not cache an invalid token`: two requests, two calls.
  - `answers 502 {"error":"homeserver unreachable"} when fetch rejects or userinfo answers 500`.
  - `maps power levels to roles` (`it.each`): 90 and 95 give `admin`; 100 and 150 give `owner`.
  - `gives owner to a v12 creator without a power level`: a create event with `room_version: "12"` whose `sender` is the user, and one that lists the user in `additional_creators`. A v11 creator without a level is refused.
  - `refuses with 403 {"error":"forbidden"}`: level 50, level 0, and `users_default` only; membership `leave`, `invite`, `ban`, and no member event, each at level 100.
  - `reads the role at every request` (RF-role): with a cached token, lower the user to 0; the next request is 403.
  - `answers 503 while the workforce space is unknown`.
- [ ] **Step 2: Run to verify they fail.** `T exec vitest run src/fabrium-api/auth.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement.** `fake-matrix.ts` answers `GET /_matrix/federation/v1/openid/userinfo`, `GET /_matrix/client/v3/rooms/{room}/state/{type}/{key}`, `GET /_matrix/client/v3/account/whoami`, `GET /_matrix/client/v3/register/available`. The verifier keys its cache by token and prunes expired entries on insert. `requireRole` reads the three state events in parallel with `asToken`. Version rule as `isRoomCreator` in `roles.ts`: `Number(room_version ?? '1') >= 12`.
- [ ] **Step 4: Run to verify they pass.** Same command, PASS. `T typecheck` passes.
- [ ] **Step 5: Commit.** `git commit -m "feat(cli): check OpenID tokens and roles for the VM API"`.

---

### Task 4: Control plane client and read, proxy, and agent routes

**Files:**
- Create: `control-plane.ts`, `vms.ts`, `index.ts`, `test-api.ts`, `routes.test.ts`

**Interfaces:**
- Consumes: `requireRole`, `ApiError` (Task 3); `readBundle`, `listBundles` (Task 2).
- Produces:
  ```ts
  // control-plane.ts
  export interface ControlPlane { request(method: string, path: string, body?: unknown): Promise<Response> }  // ApiError 502 'control plane unreachable'
  export function createControlPlane(o: { baseUrl: string; apiKey: string; fetch?: typeof fetch; timeoutMs?: number }): ControlPlane
  export function relay(upstream: Response): Promise<Response>   // status, body, content type unchanged
  // index.ts
  export interface FabriumApiDeps { /* config + daemon facts, see Task 7 */ }
  export function fabriumApi(deps: FabriumApiDeps): Hono
  export function mountFabriumApi(app: Hono, deps: FabriumApiDeps): void   // app.route('/_fabrium/v1', fabriumApi(deps))
  ```
  Routes of this task: `GET /access`, `GET /vms`, `GET /instance-types`, `GET /agents`, `POST /vms/:id/{start,stop,reboot}`, `DELETE /vms/:id`. `POST /vms` comes in Task 6.

- [ ] **Step 1: Write the failing tests** in `routes.test.ts` (the fake Matrix plus a scripted control plane `fetch`, bundles in a temp dir):
  - `GET /access returns the user and role`: `{"user_id":"@ann:example.org","role":"admin"}`.
  - `answers 401 and 403 without calling the control plane`.
  - `GET /vms adds agent_id from the bundle and null without one`.
  - `sends the tenant key to the control plane and never returns it`: the upstream call has `Authorization: Bearer <key>`; no response body or console output contains the key.
  - `passes a control plane error through` (`it.each` of 404, 409 with `{"error":...}`): same status and body.
  - `relays a non-JSON error body untouched` (RF4): a `413` `text/plain` body and a `502` HTML body keep status, body, and content type.
  - `answers 502 {"error":"control plane unreachable"}` when fetch rejects and on timeout (`timeoutMs: 20`, a hanging fetch that honors the abort signal).
  - `GET /instance-types proxies`.
  - `GET /agents lists every bundle sorted by workstation`: `free` is true for `opencode` with no VM of that name, false when a VM has that name, false for `preset: null`; a control plane failure is passed through.
  - `start, stop, and reboot proxy to /v1/vms/<id>/<action>` (`it.each`) and return `202` with the VM; `DELETE` returns `202` with an empty body.
  - `encodes the VM id in the upstream path`: id `a/../b` is sent as `a%2F..%2Fb`.
  - `mounts only under /_fabrium/v1`: `/vms` is 404, `/_fabrium/v1/vms` is not.
- [ ] **Step 2: Run to verify they fail.** `T exec vitest run src/fabrium-api/routes.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement.** `request` aborts with `AbortSignal.timeout(timeoutMs ?? 15_000)` and maps any thrown error to the `502`. `relay` copies `content-type` and builds a new `Response`. `fabriumApi` creates `new Hono()`, `app.onError` maps `ApiError` to `c.json(body, status)` and anything else to `500 {"error":"internal error"}` (logs the message only), applies `requireRole` to `*`, then the routes. The agents list is computed from `listBundles` and the control plane list: `free = preset === 'opencode' && !vmNames.has(workstation)`. VM names are checked against the name regex before a bundle path is built.
- [ ] **Step 4: Run to verify they pass.** Same command, PASS. `T typecheck` passes.
- [ ] **Step 5: Commit.** `git commit -m "feat(cli): serve the VM list, agents, and actions under /_fabrium/v1"`.

---

### Task 5: Runtime identity admission

**Files:**
- Create: `admission.ts`, `admission.test.ts`
- Modify: `fake-matrix.ts`

**Interfaces:**
- Consumes: `ensureBundle`, `renderRegistration`, `withServer`, `MARKER` (Task 2); `whoami`, `stateEvent`, `usernameAvailable` (Task 3); `MatrixClient`.
- Produces:
  ```ts
  export interface AdmissionDeps { conn: MatrixConn; client: MatrixClient; asToken: string; asUserId: string; serverName: string
    homeserverUrl: string; workforceSpace: string; getSpaceRoomId(): string | undefined; rooms: string[]
    workstationsDir: string; registrationsDir: string; whoamiTimeoutMs?: number; pollIntervalMs?: number }
  export interface NewAgent { workstation: string; agent: string; persona?: string; project?: string }
  export function createAdmission(deps: AdmissionDeps): { admitNew(a: NewAgent): Promise<Bundle> }   // ApiError 409 or 502 with step
  export const REGISTER_COMMAND = '!admin appservices register'
  export function registerMessage(registrationYaml: string): string   // command, ```, yaml, ```
  ```
  The fake gains: the admin room (`#admins:example.org`, `@zooid` joined), a registry of appservice tokens, `POST /register`, `POST /join`, `POST /invite`, `PUT` state and `PUT` message in the admin room, plus knobs `adminCommandWorks`, `registerAfterWhoamiPolls`, and `failOnce(method, pathRegex, status)`.

- [ ] **Step 1: Write the failing tests** in `admission.test.ts` (temp `workstationsDir` and `registrationsDir`, `whoamiTimeoutMs: 100`, `pollIntervalMs: 5`):
  - `admits a new agent end to end`: the bundle has the marker, `.env`, and `zooid.yaml`; exactly one admin message; `@lima.echo:example.org` exists; `@lima` and `@lima.echo` are joined to the space; the agent is joined to `#general`; the roster `dev.zooid.workforce/lima` equals `{version:1, agents:[{user_id:'@lima.echo:example.org', name:'echo', rooms:['!general:example.org'], persona:'Reviewer', project:'Fabrium'}]}`.
  - `sends the registration as a code block under !admin appservices register`: body is `!admin appservices register\n```\n<yaml>\n```` and the yaml has the bundle tokens.
  - `waits for whoami to accept the VM token`: `registerAfterWhoamiPolls: 3` passes after at least 3 polls.
  - `names the setup when @zooid is not in the admin room` (RF1): the send answers `403`, `ApiError` 502, `step: 'register_appservice'`, a message that contains `make-user-admin`; the bundle stays. `names the admin room when the command has no effect` (RF1): `adminCommandWorks: false` gives `step: 'whoami'` and a message with `#admins:example.org` and `make-user-admin`.
  - `resumes after a failed step without new tokens or a second registration`: `failOnce('POST', /\/join\//, 500)` gives `step: 'join'`; the retry succeeds, tokens equal the first run's, one admin message in total.
  - `sends the command again when the first one had no effect`: fail at `whoami`, set `adminCommandWorks: true`, retry: two admin messages, success.
  - `names the failing step in a 502` (`it.each` over `register_appservice` (admin send 403), `register_user` (500), `join` (missing room), `roster` (state PUT 500), `bundle` (workstations dir is a file)): `step` equals the name and no token appears in the message.
  - `refuses a workstation that add-workstation.sh registered`: `registrations/lima.yaml` exists, 409.
  - `refuses a bundle without the marker, and a bundle of another agent`: 409 each.
  - `refuses a name that is already a Matrix user` (RF2) (`ann` and the server user): 409, no directory, no admin message.
  - `serializes concurrent admissions of one workstation` (RF3): `Promise.all` of two calls gives one admin message and equal tokens.
  - `skips the roster write when the entry already matches` and `skips invite for a joined member`; `fails the join when a user is banned`.
- [ ] **Step 2: Run to verify they fail.** `T exec vitest run src/fabrium-api/admission.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement** `admission.ts` as the Decisions describe. Run each step through `step(name, fn)`, which turns a thrown error into `ApiError(502, {error: message, step: name})` and logs `[fabrium] admission of <ws> failed at <step>: <message>`. Put the verified syntax (the facts table) in a header comment. The `whoami` timeout message: `tuwunel did not load the registration within 30 s. Check that @zooid is a server admin (!admin users make-user-admin zooid, see deploy/README.md).` Queue: `queue = queue.then(() => fn()).catch(() => {})`-style tail with the real result returned.
- [ ] **Step 4: Run to verify they pass.** Same command, PASS. `T typecheck` passes.
- [ ] **Step 5: Commit.** `git commit -m "feat(cli): admit a new agent identity at runtime"`.

---

### Task 6: Create route

**Files:**
- Create: `create.ts`, `create.test.ts`
- Modify: `index.ts` (add `POST /vms`)

**Interfaces:**
- Consumes: `createAdmission` (Task 5); `buildPayload` (Task 1); `readBundle`, `listBundles` (Task 2); `ControlPlane` (Task 4).
- Produces:
  ```ts
  export type CreateRequest =
    | { name: string; instanceType?: string; agent: { mode: 'new'; name: string; persona?: string; project?: string }; havenApiKey: string; githubToken?: string }
    | { name: string; instanceType?: string; agent: { mode: 'existing' }; havenApiKey: string; githubToken?: string }
  export function parseCreateRequest(body: unknown): CreateRequest   // ApiError 400
  export function createVm(deps: CreateDeps, req: CreateRequest): Promise<Response>
  ```

- [ ] **Step 1: Write the failing tests** in `create.test.ts` (fake Matrix, scripted control plane, temp dirs):
  - `answers 503 without FABRIUM_HAVEN_MODEL` and `without ZOOID_COMMIT` with the exact bodies, and calls nothing.
  - `answers 400 for an invalid body` (`it.each`): name `Lima`, `-x`, 64 chars, `zooid`; no `agent`; mode `other`; agent name `1abc`, 33 chars; missing, empty, or non-string `haven_api_key`; a `haven_api_key` or `github_token` with a control character (RF5); `instance_type` that is not a string; persona or project not a string or over 200 characters; invalid JSON; each has `{"error":...}` and no secret in it.
  - `creates a VM with a new agent`: the control plane receives `POST /v1/vms` with body `{name, instance_type, bootstrap: {files, script}}`; files are the three paths of contracts section 3; the secrets file has the Haven key, `HAVEN_MODEL` from the config, and `GH_TOKEN`; the response is `201` with the VM plus `agent_id` `@lima.echo:example.org`.
  - `omits instance_type and GH_TOKEN when absent or empty`.
  - `creates a VM for an existing free agent`: the bundle `.env` is sent unchanged; the fake Matrix receives no call.
  - `refuses an existing agent that is not free` (a VM has the name, a `preset: null` bundle, no bundle): 400 each.
  - `passes a control plane failure through and keeps the identity free`: `409 {"error":"name in use"}` and `502` come back unchanged; afterwards `GET /agents` shows `free: true`.
  - `answers 502 with the step when admission fails`.
  - `never logs or returns the secrets` (RF5): across success and failure paths, spies on `console.log`, `console.warn`, and `console.error`, and every response body, contain neither key.
- [ ] **Step 2: Run to verify they fail.** `T exec vitest run src/fabrium-api/create.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement.** Config `503`s first, then parse (`400`), then admission or bundle load, then `buildPayload`, then `POST /v1/vms`. The body is `{name, instance_type?, bootstrap}`. On a `2xx` JSON body, add `agent_id`; any other status is relayed. Existing mode computes `free` with the same function as `GET /agents`.
- [ ] **Step 4: Run to verify they pass.** Same command, PASS. `T typecheck` passes.
- [ ] **Step 5: Commit.** `git commit -m "feat(cli): create VMs through the control plane"`.

---

### Task 7: Config and daemon mount

**Files:**
- Create: `config.ts`, `config.test.ts`, `.changeset/vm-management-api.md`
- Modify: `packages/cli/src/daemon/start-daemon.ts`, `index.ts` (finish `FabriumApiDeps`)

**Interfaces:**
- Produces:
  ```ts
  export interface FabriumApiConfig { controlPlaneUrl: string; controlPlaneKey: string; havenModel?: string; vmRooms: string[]
    zooidCommit?: string; homeserverUrl: string; zooidHome: string }
  export function loadFabriumApiConfig(env?: NodeJS.ProcessEnv, readKey?: (path: string) => string): FabriumApiConfig | undefined
  ```

- [ ] **Step 1: Write the failing tests** in `config.test.ts`:
  - `returns undefined unless both the URL and the key file are set` (four combinations, empty strings count as unset).
  - `reads the key file once and trims it`: the reader is called once; the key is in the result.
  - `fails when the key file cannot be read or is empty`: the message names the variable and the path; it never contains the key.
  - `strips a trailing slash`, `rejects a URL that is not a URL or not http or https`, `refuses a plain-http URL that is not on this host` (the payload holds secrets), and `accepts http for localhost, 127.0.0.1, and [::1]`. The https rule is a separate commit, `feat(cli): refuse a plain-http control plane URL`, so the deploy commit stays last.
  - `defaults FABRIUM_VM_ROOMS to #general, splits a comma list, trims, and drops empty entries`; `rejects a room with a space, a quote, a backslash, or no # or !`.
  - `requires ZOOID_HOME and HOMESERVER_URL when enabled`.
  - `reads FABRIUM_HAVEN_MODEL and ZOOID_COMMIT, with empty meaning unset`.
- [ ] **Step 2: Run to verify they fail.** `T exec vitest run src/fabrium-api/config.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement** `config.ts` and the mount. In `start-daemon.ts`, inside `if (shouldBindHttpListener(mode))`, after the webhook routes and before `serve()`:
  `const vmApi = loadFabriumApiConfig(); if (vmApi) { mountFabriumApi(transport.app, { ...vmApi, conn: { homeserver: matrix.transport.homeserver, fetch: opts.fetch ?? fetch }, client, asToken: matrix.transport.as_token, asUserId, serverName, workforceSpace: matrix.transport.space ?? 'dev', getSpaceRoomId: () => spaceRoomId }); console.log('[fabrium] VM management API at /_fabrium/v1') }`.
  The changeset: `'zooid': minor`, one paragraph that names the routes and the variables.
- [ ] **Step 4: Run to verify they pass.** `T exec vitest run src/fabrium-api`, then the CI unit set, `T typecheck`, and `corepack pnpm -C packages/cli build` (tsup bundles the new folder). All pass.
- [ ] **Step 5: Commit.** `git add packages/cli/src .changeset` then `git commit -m "feat(cli): mount the VM management API in the daemon"`.

---

### Integration check (no commit; done, 47 checks passed)

Needs Docker and a pull of the digest in `deploy/docker-compose.yml`. Everything is removed afterwards.

1. Run the image from the compose digest with a `tuwunel.toml` like `deploy/templates/tuwunel.toml` for `localhost`, open registration, and a `zooid` registration file. Print `--version`.
2. Register a first user (it becomes the first admin), `ann`, and `bob`. Give `ann` power level 90 in the workforce space.
3. Run the real daemon (the built `packages/cli/dist/bin.js`, with `--data` on a short path: the socket path limit is 81 bytes) against it, with a fake control plane (a Node HTTP server that records the create body) and the `FABRIUM_*` variables.
4. With real OpenID tokens: `GET /access` (admin, member, no token, unknown token), then `POST /vms` for a new agent before and after `@zooid` is an admin. Check `whoami` with the bundle token, the users, the memberships, the roster state, and the recorded payload. Repeat the create (resume), send a second register of the same ID, create for an existing agent, try the names `ann`, `conduit`, and `zooid`, then unregister.
5. Scan the daemon log for the key, the Haven key, the GitHub token, and the tokens. Restart tuwunel to see that the registration persists, and again with a registration file of the same ID. Stop and remove the container and the temp directories.

The scan found the admin command in the daemon log (see the table above). The fix is the `transport-matrix` commit. Everything else passed.

---

### Task 8: Deploy work unit

Touches only `deploy/`. It becomes its own pull request.

**Files:**
- Modify: `deploy/Caddyfile`, `deploy/docker-compose.yml`, `deploy/Dockerfile`, `deploy/deploy.sh`, `deploy/add-workstation.sh`, `deploy/README.md`, `deploy/.env.example`

**Interfaces:** Consumes the environment names of `config.ts` (Task 7). Produces `ZOOID_COMMIT` in the daemon image and the route `/_fabrium/*` to `zooid`.

- [ ] **Step 1: Caddy.** Add `handle /_fabrium/* { reverse_proxy zooid:{$APPSERVICE_PORT:9000} }` before the web catch-all `handle`.
- [ ] **Step 2: Compose and image.** In `docker-compose.yml`: `build.args` of `zooid` gets `ZOOID_COMMIT: ${ZOOID_COMMIT:-}`; `environment` gets the key-only entries `FABRIUM_CONTROL_PLANE_URL`, `FABRIUM_CONTROL_PLANE_KEY_FILE`, `FABRIUM_HAVEN_MODEL`, `FABRIUM_VM_ROOMS`, `HOMESERVER_URL`, `ZOOID_HOME`. The key file needs no mount: `ZOOID_HOME` is mounted at the same path. In `Dockerfile` (`daemon` stage): `ARG ZOOID_COMMIT=` and `ENV ZOOID_COMMIT=${ZOOID_COMMIT}`. In `deploy.sh`: after `NEW_SHA`, `export ZOOID_COMMIT="$(git -C "$REPO_DIR" rev-parse HEAD)"` and `--build-arg ZOOID_COMMIT="$ZOOID_COMMIT"` on `compose build`.
- [ ] **Step 3: `.env.example`.** Document the four `FABRIUM_*` variables in the commented style of the file: the key file path (`$ZOOID_HOME/control-plane.key`, mode 600), the Haven model, the rooms (quote values that start with `#`). **Not done:** the permission settings and the Safety Net hook of this session deny every access to files named `.env*`. The README documents the variables; the orchestrator adds the block to `.env.example` (text in the report).
- [ ] **Step 4: `add-workstation.sh`.** Before it touches the bundle or the registration, `die` when `$ZOOID_HOME/workstations/$WS/.runtime-registration` exists, with a message that names the cause (registration in the tuwunel database; a file registration with the same ID stops tuwunel from loading the registrations after it) and the way out (Remove a VM). Add one line to `usage`.
- [ ] **Step 5: README.** A "VM management" section: the variables table (`FABRIUM_*`, `ZOOID_HOME`, `HOMESERVER_URL`, `ZOOID_COMMIT`); the one-time `@zooid` setup with the verified command and reply, and the `--execute` fallback; the files (`workstations/<ws>/.runtime-registration`); a runtime variant of "Remove a VM" (`!admin appservices unregister <ws>`, then the bundle, then the rooms and roster steps as before). Add the `/_fabrium/*` route to the service table and the marker to the layout list.
- [ ] **Step 6: Verify.** `docker compose -f deploy/docker-compose.yml config` with sample variables shows the build arg and the environment entries. `shellcheck` on `add-workstation.sh` and `deploy.sh` (through the `koalaman/shellcheck` image if the binary is missing) has no new finding. Run `add-workstation.sh` against a temp `ZOOID_HOME` that has a marker and see the refusal, exit 1, before any write. `docker buildx build --check -f deploy/Dockerfile .` has no new warning.
- [ ] **Step 7: Commit.** `git add deploy` then `git commit -m "feat(deploy): route, configure, and document VM management"`.

---

## Self-review

- **Spec coverage.** Mount and config (Task 7), auth and role (Task 3), routes (Tasks 4 and 6), admission with resume and step names (Task 5), existing agent (Task 6), payload (Task 1), tuwunel facts in code and README (Tasks 5 and 8, check above), `ZOOID_COMMIT` flow and deploy unit (Task 8), tests of the spec list (Tasks 1 to 6).
- **Type consistency.** `Bundle`, `BundleSpec`, `MatrixConn`, `ApiError`, `FabriumApiDeps`, and `AdmissionDeps` are defined once and reused by name.
- **Contract gaps for the orchestrator.** (1) For `existing`, `name` is the workstation and `agent.name` is ignored. (2) `preset` in `GET /agents` is `null` for a bundle without a preset. (3) Create adds a `409` for a name that is a Matrix user, and a `400` for control characters in the two secrets. (4) `.env` first comment line differs from the script on purpose.
- **Out of scope.** Per-name admission locks, finding users outside the workforce space, persisting the OpenID cache, rate limits, redacting the register command in the admin room after the registration loads (the room keeps the VM tokens; its members are server admins).

## As built (2026-10-26)

Branch `feat/vm-daemon-api`, ten commits, in this order:

| Commit | Message |
|---|---|
| `4d4ec8e` | `feat(cli): build the bootstrap payload for agent VMs` |
| `f0e4545` | `feat(cli): write and read agent VM bundles` |
| `0f602ce` | `feat(cli): check OpenID tokens and roles for the VM API` |
| `eb8f7b1` | `feat(cli): serve the VM list, agents, and actions under /_fabrium/v1` |
| `5c31703` | `feat(cli): admit a new agent identity at runtime` |
| `b1e9162` | `feat(cli): create VMs through the control plane` |
| `29c38a9` | `feat(cli): mount the VM management API in the daemon` |
| `59eb21e` | `fix(transport-matrix): do not log the body of an admin command` |
| `1e4d976` | `feat(cli): refuse a plain-http control plane URL` |
| `3efe758` | `feat(deploy): route, configure, and document VM management` (touches only `deploy/`) |

Differences from the plan above: the extra `transport-matrix` and https commits; `matrix.test.ts`, `vms.ts`, and `test-api.ts`; `AuthDeps` keeps `client` and `asUserId` (the v12 creators need the full room state once); `.env.example` is not updated (blocked).

## Review fixes (2026-10-26)

An independent review and the stage deploy found eight items. They are nine more commits on the same branch; the daemon commits touch only `packages/` and `.changeset/`, the deploy commits only `deploy/`.

| Commit | Item |
|---|---|
| `8a001fe` `fix(cli): refuse a secret that the VM script refuses` | 1. A secret with a backslash or a leading quote is a `400`, with the wording of `configure-agent.sh`; `FABRIUM_HAVEN_MODEL` follows the same rule at start. A parity test runs the check block of the script itself. |
| `903fff3` `fix(cli): list an agent as free only when its registration is live` | 2. For a bundle with the marker, `GET /agents` says `free: false` and a create for the existing agent answers `409` unless `whoami` with the bundle token returns `@<ws>:<server>`. Read-only; unmarked bundles are not asked. |
| `fe1d2d0` `fix(cli): refuse a workstation name that would act as users of the space` | 3. A fresh admission refuses a name when a joined member of the space starts with `<ws>.`, or when `@<ws>.<agent>` exists. |
| `bdc285e` `fix(cli): keep the control plane key out of the log and out of a bad key file` | 4. The key file must be printable ASCII without space; the log of a failed request has the error name and code only. |
| `e3b99e3` `fix(cli): answer 502 when the control plane stalls after its headers` | 5. `readBody` wraps every body read; a lost create answer says that the VM may exist. |
| `8ea69bd` `fix(transport-matrix): do not log an admin command given as a server user mention` | 7. tuwunel runs a message that starts with the server user (`@conduit:…`) as a command too (`is_admin_command`, `src/service/admin/mod.rs` lines 290 to 292; `parse_line` keeps an argument that starts with `@`). |
| `d38abf0` `fix(deploy): back up the control plane key file` | 6. `backup.sh` includes the key file when it is under `ZOOID_HOME`; the README example is `/home/ubuntu/zooid/control-plane-api-key`, an absolute path. |
| `a583c3f` `fix(deploy): rebuild the daemon image when a cached layer has the wrong ZOOID_COMMIT` | 8. After the build, `deploy.sh` reads `ZOOID_COMMIT` from the image; on a mismatch it rebuilds the zooid service with `--no-cache`, and dies if it is still wrong. |
| `c3e2225` `docs(deploy): describe the secret rules, half-admitted agents, and the name check` | Docs for items 1 to 3, with the residual gap (users outside the space). |
