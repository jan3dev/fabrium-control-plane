# VM management, web track: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Admins and owners list, create, start, stop, reboot, and delete agent VMs from the Fabrium web UI, through the zooid browser API under `/_fabrium/v1`.

**Architecture:** One typed client (`src/client/fabrium-api.ts`) owns the base URL, the Matrix OpenID token (cached, renewed once on `401`), and error mapping. A polling hook (`use-vms`) and a one-shot access hook (`use-vm-access`) feed a container (`VmRoute`) that renders presentational components (`VmView`, `NewVmDialog`). A `VM` rail button below Home shows after `GET /access` returns 200 and opens `/vm`.

**Tech Stack:** React 19, react-router-dom 6, Vite, Tailwind 4 (repo semantic tokens), shadcn/Radix primitives, matrix-js-sdk 34 (`getOpenIdToken`), Vitest + Testing Library + MSW, Storybook 10, Playwright with axe.

**Spec:** [VM management design](../specs/2026-10-26-vm-management-design.md) (sections "Web (packages/web)", "Errors", "Quick path"). Shared contracts, which win over the spec: [contracts](2026-10-26-vm-management-contracts.md) (section 2 is the API; section 1 has the VM and instance type objects). Work happens in `fabrium-web-ui`, worktree `fabrium-web-ui-worktrees/vm-web`, branch `feat/vm-web`, inside `packages/web` only.

**Status:** All five tasks are built, one commit each, plus a cleanup commit and a follow-up fix (rail state, see Decisions). Task 5 step 2 (the Playwright run) could not run in the build environment.

## Global Constraints

- Every call goes to `<base>/_fabrium/v1/...`. `<base>` is `fabrium_api_url` from `/config.json` when set, else the page origin (contracts section 2; `FABRIUM_WEB_UI_PLAN.md` section 5).
- Auth is `Authorization: Bearer <access_token>` from `client.getOpenIdToken()`. On `401`, get a new token and retry once. `403` and `404` mean no access (contracts section 2).
- Never write `aqua` or `jan3` (any case) anywhere in `packages/web`. Fixtures use `example.org`. The product name in the UI is `Fabrium`.
- UI copy uses workspace, project, channel, room, thread, agent, persona, approval. It never says space, bot, server, or guild. Plain technical English: short sentences, active voice.
- Semantic tokens only. Icons only from `@/components/icons`. `--trust-enclave` marks trust only (the `Confidential` marker). Type scale and radius rules of `AGENTS.md`.
- File names are kebab-case. Every `src/` file is reachable from `src/main.tsx` (the orphan guard counts tests and stories as nothing). Test fixtures live in `test/`.
- Presentational components take plain props typed from `src/model/types.ts`, and each has a Storybook story.
- Fixed stack: no TanStack Query (plain fetch in `use-vms`), no new dependency.
- Conventional Commits, one work-unit commit per task, no trailers, no push.

Commands. Run them from the worktree root. `T` means `corepack pnpm -C packages/web`.

- One file: `T exec vitest run <file>`. All tests: `T test`.
- Checks: `T typecheck`, `T lint`, `T build-storybook`, `T build`.

## Review Focus

Inputs and conditions the spec implies and a user will meet. Each line has a test in the task that owns the code (marked RF1 to RF5 below).

1. **A host that does not mount `/_fabrium` answers `200` with the SPA `index.html`** (static hosting, the Vite dev server). The client must fail with a typed error, the rail must hide `VM`, and `/vm` must show an error instead of crashing. Tasks 1, 2, and 3.
2. **A row action on a VM whose state changed since the last poll** (`409 {"error":"vm is stopped"}`). Show the API message in a toast and refresh the list, so the buttons match the real state. Task 2.
3. **An admission failure (`502` with `step`), then a retry, or a double click on Create.** One request per attempt. The dialog keeps the typed values, including the secrets, for the retry. It clears them on close and on success. Task 4.
4. **No free agent, or the instance types and agents fail to load.** The dialog says so and keeps Create disabled. Task 4.
5. **The VM disappears or is replaced while its delete confirmation is open.** The confirmation closes, stays closed if the VM returns, and nothing is deleted. Task 2.

---

## File structure

| File | Responsibility |
|---|---|
| `src/model/types.ts` (edit) | `Vm`, `VmState`, `VmSetup`, `VmInstanceType`, `VmAgent`, `VmAccess`, `VmFormOptions`, `CreateVmAgent`, `CreateVmRequest` |
| `src/client/fabrium-api.ts` | Base URL, OpenID token cache, `FabriumApiError`, one function per contracts route |
| `src/client/runtime-config.ts` (edit), `src/main.tsx` (edit) | Read `fabrium_api_url`, call `setFabriumApiUrl` at startup |
| `src/hooks/use-vms.ts` | Poll the VM list (5 s or 30 s), `refresh` |
| `src/hooks/use-vm-access.ts` | `true` once `GET /access` returns 200 |
| `src/hooks/use-vm-form-options.ts` | Load instance types and agents each time the dialog opens |
| `src/components/structures/vm-view.tsx` | Presentational page: states, table, row actions, delete confirmation |
| `src/components/structures/vm-page.tsx` | Container `VmRoute`: hooks, actions, toasts, dialog wiring |
| `src/components/dialogs/new-vm-dialog.tsx` | Presentational New VM form |
| `src/components/ui/native-select.tsx` | Native `<select>` styled like `Input` (no Radix Select: simpler, accessible by default) |
| `workspace-rail.tsx`, `logged-in-view.tsx`, `src/app.tsx`, `icons/index.ts` (edit) | `VM` button (`CpuIcon`), wiring, `/vm` route |
| `workspace-rail.stories.tsx` | Rail states: no VM button, VM closed, VM page open over Home or a workspace, workspace current |
| `test/vm-api.ts`, `test/vm-fixtures.ts` | `signInForVmApi` (tests only); `makeVm`, `INSTANCE_TYPES`, `AGENTS` (data only, so stories can import them) |
| `e2e/axe.spec.ts`, `README.md` (edit) | Axe sweep of `/vm`; document `fabrium_api_url` and the VM button |

## Decisions

- **Base URL.** `fabrium_api_url` is optional. `main.tsx` passes it to `setFabriumApiUrl` (same pattern as `setGlobalSearchEnabled`). A trailing slash is trimmed. Empty means same origin.
- **Token cache.** One entry, keyed by the Matrix client object, so a new login never reuses a token. It expires 30 s before `expires_in`. Callers share the in-flight request. A failed request is not cached. All calls send `cache: "no-store"`.
- **Errors.** `FabriumApiError` has `status`, `message` (the `error` field, else `Request failed (<status>)`), and optional `step`. A `2xx` body that is not JSON throws `FabriumApiError(<status>, "Unexpected response")`.
- **No access.** `isNoAccess(e)` is true for `403` and `404`. The poll stops on it.
- **Polling.** The delay derives from the latest snapshot: 0 before the first load, 5 s while any VM is `pending`, `stopping`, `shutting-down`, or has setup `pending`, else 30 s. After any failed poll the delay is 30 s and the last list stays.
- **Delete confirmation.** `VmView` keeps the VM id, looks the VM up in the list on each render, and forgets the id when the VM leaves the list.
- **Existing agent.** The VM name is the workstation name (contracts section 2). Existing mode sends `name` and `agent.name` equal to the chosen workstation, and shows no VM name field.
- **Secrets.** The dialog resets every field when `open` turns false, whoever closes it, and after a successful create. It trims names and secrets, and leaves empty optional fields out of the request.
- **Row actions** are text buttons named `<Verb> <vm name>`. Only the `Confidential` marker and the rail use icons.
- **Rail state is exclusive.** While `/vm` is open only VM is current, and no scope button is. `LoggedInView` sends a Home or workspace click to `/` when the VM page is open, so choosing one leaves the page and restores the normal state. A click on VM keeps the page. (A follow-up fix; the first build marked both.)
- **Tests run the real client, hooks, and components.** MSW plays the browser API (`signInForVmApi` plays the Matrix client). Only `sonner`'s `toast` is mocked. The polling tests fake `fetch` and timers instead of MSW.

---

### Task 1: API client, runtime config, types

**Files:**
- Create: `src/client/fabrium-api.ts`, `src/client/fabrium-api.test.ts`, `test/vm-api.ts`, `test/vm-fixtures.ts`
- Modify: `src/model/types.ts`, `src/client/runtime-config.ts`, `src/client/runtime-config.test.ts`, `src/main.tsx`, `README.md`

**Interfaces:**
- Produces (`fabrium-api.ts`):
  ```ts
  export class FabriumApiError extends Error { readonly status: number; readonly step?: string }
  export function isNoAccess(e: unknown): boolean            // 403 or 404
  export function setFabriumApiUrl(url: string | undefined): void
  export const getAccess: () => Promise<VmAccess>
  export const listVms: () => Promise<Vm[]>
  export const listInstanceTypes: () => Promise<VmInstanceType[]>
  export const listAgents: () => Promise<VmAgent[]>
  export const createVm: (req: CreateVmRequest) => Promise<Vm>
  export const startVm: (id: string) => Promise<Vm>          // stopVm, rebootVm: same shape
  export const deleteVm: (id: string) => Promise<void>
  ```
- Produces (`types.ts`): `Vm` has the contracts section 1 fields plus `agent_id: string | null`. `CreateVmRequest` is the contracts section 2 body.

- [x] **Step 1: Write the failing tests.** In `fabrium-api.test.ts` (MSW, `signInForVmApi`):
  - Token: `is sent as a Bearer header under /_fabrium/v1`; `is cached until 30 s before expires_in` (cached at 3569 s, renewed at 3571 s); `is requested once for concurrent calls`; `is renewed, and the call retried once, on a 401`; `is not retried a second time when the 401 repeats` (two requests); `is requested again after a failed request`.
  - Errors: `carry the status, the message, and the step` (`502 {"error":"matrix timeout","step":"whoami"}`); `fall back to a generic message when the body is not JSON` (`413`, `Request failed (413)`); `cover a 200 answer that is not JSON (a host without the API)` (RF1); `cover a call made while nobody is signed in, with no request`; `isNoAccess` for 401, 403, 404, 409, 502, and a plain `Error`.
  - Routes: `create posts the body as JSON`; `start, stop, and reboot post to /vms/<id>/<verb>`; `delete resolves on an empty 202`; `keep a VM id inside its own path segment`.
  - Base URL: `is fabrium_api_url when set, without a trailing slash`.
  In `runtime-config.test.ts`: `passes a string value through` and `leaves the key unset when it is not a string`.
- [x] **Step 2: Run to verify they fail.** `T exec vitest run src/client/fabrium-api.test.ts src/client/runtime-config.test.ts`. Expected: FAIL (module not found; `fabrium_api_url` undefined).
- [x] **Step 3: Implement.** Types first (`VmInstanceType`, because `InstanceType` is a TypeScript built-in). One private `call<T>(method, path, body?)` signs in with the cached token, retries once on `401`, throws `FabriumApiError`, and returns `undefined` for an empty body. Add `fabrium_api_url` to `RuntimeConfig`, call `setFabriumApiUrl` in `main.tsx`, document the key in the README.
- [x] **Step 4: Run to verify they pass.** Same command; then `T typecheck` and `T lint`.
- [x] **Step 5: Commit.** `feat(web): add the Fabrium service API client`.

---

### Task 2: `use-vms` hook and the VM page at `/vm`

**Files:**
- Create: `src/hooks/use-vms.ts`, `use-vms.test.tsx`, `src/components/structures/vm-view.tsx`, `vm-view.stories.tsx`, `vm-page.tsx`, `vm-page.test.tsx`
- Modify: `src/app.tsx` (`<Route path="vm" element={<VmRoute />} />` under the `/` layout route), `src/app.test.tsx`

**Interfaces:**
- Consumes: the Task 1 client.
- Produces:
  ```ts
  // use-vms.ts
  export function useVms(): { vms: Vm[] | null; error: unknown; refresh: () => void }
  // vm-view.tsx
  export type VmAction = "start" | "stop" | "reboot" | "delete";
  export function VmView(p: { vms: Vm[] | null; error: string | null; noAccess: boolean;
    busyIds: ReadonlySet<string>; onAction: (vm: Vm, a: VmAction) => void;
    onNew?: () => void; onRetry: () => void }): JSX.Element
  // vm-page.tsx
  export function VmRoute(): JSX.Element
  ```

- [x] **Step 1: Write the failing tests.**
  `use-vms.test.tsx` (fake timers, faked `fetch`, real client): `loads at once, then polls every 30 s while nothing is in transition`; `polls every 5 s while a VM is pending / stopping / shutting down / has setup pending` (`it.each`); `polls every 30 s again once the VM settles`; `stops polling on unmount`; `stops polling on 403` and `on 404`; `keeps the last list on another error and retries in 30 s` (the list had a pending VM, so a 5 s retry would fail it); `retries a failed first load in 30 s`; `refresh loads at once`.
  `vm-page.test.tsx` (`<VmRoute>` over a fake control plane in MSW):
  - `lists name, agent, type, state, setup, IP, and launch date` (`None` for a missing agent and IP).
  - `marks only SEV-SNP types as Confidential, and flags an expired setup`.
  - `offers actions by state` (`it.each`: stopped Start and Delete; running Stop, Reboot, Delete; pending and stopping Delete).
  - `runs an action, then shows the new state` (calls: `GET /vms`, `POST <id>/stop`, `GET /vms`); `disables the buttons of a VM while its action runs`.
  - `asks before deleting a VM` (Cancel calls nothing; confirm calls `DELETE <id>`).
  - `shows a toast when an action fails, then refreshes the list` (RF2: `409 vm is stopped`).
  - `shows the no-access state on 403` and `on 404`; `shows an error, and Retry loads again, after an error answer` and `after a 200 that is not JSON` (RF1); `shows the empty state when there are no VMs`.
  `<VmView>`: `closes the delete confirmation when the VM leaves the list` (RF5, including the VM returning); `disables only the buttons of a VM with an action in flight`; `keeps the list and shows an error when a refresh fails`; `shows a launch time it cannot parse as it is`; `offers New VM only when it can open the dialog`.
  `app.test.tsx`: `serves the VM page at /vm`.
- [x] **Step 2: Run to verify they fail.** `T exec vitest run src/hooks/use-vms.test.tsx src/components/structures/vm-page.test.tsx src/app.test.tsx`. Expected: FAIL (modules not found; `/vm` unrouted).
- [x] **Step 3: Implement.** `useVms` keeps one `{ vms, error }` snapshot. An effect on the snapshot arms one `setTimeout` (delay rules in Decisions) and clears it on cleanup; `403` and `404` arm none. `VmView` renders `<h1>VMs</h1>`, a `New VM` button when `onNew` is set and access is not denied, then loading, no-access, error with Retry, empty, or the table (`<th scope>`, state `Badge` with success, neutral, or warning tone, caption `Setup <value>`, `Badge tone="enclave"` with `ShieldCheckIcon` for `sev_snp`, `formatAbsoluteTime` for the date, falling back to the raw text). The delete confirmation is an `AlertDialog`. `VmRoute` calls the client, tracks `busyIds`, toasts `Could not <verb> <name>: <message>` on failure, and calls `refresh()` after every action. Story states: `AllStates`, `ActionInFlight`, `ConfirmDelete`, `Loading`, `Empty`, `NoAccess`, `LoadError`, `RefreshError`.
- [x] **Step 4: Run to verify they pass.** Same command; then `T typecheck`, `T lint`, and `T build-storybook`.
- [x] **Step 5: Commit.** `feat(web): add the VM page and its polling hook`.

---

### Task 3: `VM` rail button

**Files:**
- Create: `src/hooks/use-vm-access.ts`, `use-vm-access.test.tsx`
- Modify: `src/components/icons/index.ts` (`Cpu as CpuIcon`), `workspace-rail.tsx`, `workspace-rail.test.tsx`, `logged-in-view.tsx`, `logged-in-view.test.tsx`, `README.md`

**Interfaces:**
- Consumes: `getAccess` (Task 1), the `/vm` route (Task 2).
- Produces: `useVmAccess(): boolean`. `WorkspaceRail` gets `vm?: { active: boolean; onSelect: () => void }`. It stays free of fetches and router hooks, so its existing tests do not change.

- [x] **Step 1: Write the failing tests.**
  - `use-vm-access.test.tsx`: `is false until the API answers 200, then true`; `stays false after a 403`, `a 404`, `a 200 that is not JSON` (RF1), and `a network error`.
  - `workspace-rail.test.tsx`: `has no VM button unless the vm prop is set`; `puts the VM button directly below Home and opens it on click`; `marks only the VM button current while the VM page is open` (over Home and over a workspace); `marks the scope, not VM, current while the VM page is closed`.
  - `logged-in-view.test.tsx` (real `App`): `shows the VM button when the API grants access, and opens the VM page`; `marks only VM current on the VM page, and Home leaves the page`; `leaves the VM page through a workspace, which becomes the current one`; `has no VM button after a 403`, `a 404`, and `a 200 that is not JSON` (RF1).
- [x] **Step 2: Run to verify they fail.** `T exec vitest run src/hooks/use-vm-access.test.tsx src/components/structures/workspace-rail.test.tsx src/components/structures/logged-in-view.test.tsx`. Expected: FAIL.
- [x] **Step 3: Implement.** `useVmAccess` calls `getAccess()` once on mount and sets `true` on success. Every failure keeps it `false`. `WorkspaceRail` renders `<RailButton label="VM" ...><CpuIcon aria-hidden /></RailButton>` right after Home when `vm` is set. `LoggedInView` calls `useVmAccess()` and `useMatch("/vm")`, and passes `vm`.
- [x] **Step 4: Run to verify they pass.** Same command; then `T test`, `T typecheck`, `T lint`.
- [x] **Step 5: Commit.** `feat(web): add the VM button to the workspace rail`.

---

### Task 4: New VM dialog

**Files:**
- Create: `src/components/ui/native-select.tsx`, `src/components/dialogs/new-vm-dialog.tsx`, `new-vm-dialog.test.tsx`, `new-vm-dialog.stories.tsx`, `src/hooks/use-vm-form-options.ts`
- Modify: `src/components/structures/vm-page.tsx`, `vm-page.test.tsx`, `src/components/ui/primitives.stories.tsx` (`NativeSelects`), `test/vm-fixtures.ts` (`INSTANCE_TYPES`, `AGENTS`)

**Interfaces:**
- Consumes: `listInstanceTypes`, `listAgents`, `createVm`, `FabriumApiError` (Task 1); `VmRoute`, `VmView.onNew` (Task 2).
- Produces:
  ```ts
  export function NewVmDialog(p: { open: boolean; onOpenChange: (open: boolean) => void;
    options: VmFormOptions | null; optionsError: string | null;
    onSubmit: (req: CreateVmRequest) => Promise<void> }): JSX.Element
  export function useVmFormOptions(enabled: boolean): { options: VmFormOptions | null; error: string | null }
  ```

- [x] **Step 1: Write the failing tests.** `new-vm-dialog.test.tsx` (props only, `userEvent`):
  - `starts in New mode with VM name, agent name, persona, and project`.
  - `lists free agents only in Existing mode, and hides the name fields`.
  - `preselects the default instance type and marks SEV-SNP types as Confidential` (options read `t3a.medium · 2 vCPU · 4 GiB` and `c6a.large · 2 vCPU · 4 GiB · Confidential`).
  - `keeps Create disabled until the required fields are set`; `needs an agent and the key in Existing mode`.
  - `sends the new-agent request`: `{ name: "lima", instance_type: "t3a.medium", agent: { mode: "new", name: "echo", persona: "Reviewer", project: "Fabrium" }, haven_api_key: "hk-1", github_token: "gh-1" }`.
  - `leaves out the optional fields that are empty, and trims what it sends`; `sends the existing-agent request, named after its workstation`: `{ name: "orion", instance_type: "t3a.medium", agent: { mode: "existing", name: "orion" }, haven_api_key: "hk-1" }`.
  - `takes both secrets in password inputs`.
  - `stays open and disabled while creating` (RF3): Escape and a second click do nothing; `onSubmit` runs once.
  - `closes on request when nothing is being created`.
  - `shows a failure inline with its step, keeps the values, and sends the same body on retry` (RF3).
  - `clears both secrets after a successful create, even when the parent keeps the dialog open`, and `clears both secrets when the dialog is closed and opened again` (RF3).
  - `says so, and keeps Create disabled, when no agent is free`; `shows why the options did not load, and keeps Create disabled`; `shows a loading line, and keeps Create disabled, while the options load` (RF4).
  In `vm-page.test.tsx` (`New VM dialog`): `creates the VM, closes, and refreshes the list`; `loads the instance types and agents each time it opens`; `shows a failed create inline with its step, and stays open`; `shows why the options did not load`.
- [x] **Step 2: Run to verify they fail.** `T exec vitest run src/components/dialogs/new-vm-dialog.test.tsx src/components/structures/vm-page.test.tsx`. Expected: FAIL.
- [x] **Step 3: Implement.** `NativeSelect` is a `<select>` with the `Input` class set. `NewVmDialog` follows `create-room.tsx`: a `Dialog` with a `form`, close requests ignored while submitting, an inline error. The mode switch is a `SegmentedControl` ("New agent", "Existing agent"). Labels: "VM name", "Agent name", "Persona", "Project", "Agent", "Instance type", "Haven API key", "GitHub token". The submit button reads "Create VM", then "Creating…". Secret inputs use `type="password"` and `autoComplete="new-password"`. The inline error shows the message and, when `step` is set, "Failed at step: <step>. Select Create VM to retry from that step." `useVmFormOptions(enabled)` runs `Promise.all([listInstanceTypes(), listAgents()])` each time `enabled` turns true and ignores late answers. `VmRoute` opens the dialog from `onNew`; its submit calls `createVm` then `refresh()`. Stories: `NewAgent`, `NewAgentReady`, `ExistingAgent`, `Creating`, `AdmissionError`, `LoadingOptions`, `OptionsError`, `NoFreeAgent`.
- [x] **Step 4: Run to verify they pass.** Same command; then `T test`, `T typecheck`, `T lint`, `T build-storybook`.
- [x] **Step 5: Commit.** `feat(web): add the New VM dialog`.

---

### Task 5: Axe sweep of `/vm`

**Files:**
- Modify: `e2e/axe.spec.ts`

**Interfaces:** Consumes the whole feature. Produces nothing.

- [x] **Step 1: Extend the sweep.** Before the first sweep, `page.route` stubs for the OpenID request (`**/openid/request_token`, with CORS headers, because the homeserver is another origin), `**/_fabrium/v1/access`, and `**/_fabrium/v1/vms` (four VMs: each state tone, a Confidential type, an expired setup, no IP or agent). After `/invites`, `await sweep("/vm", text("tango"))`. The rail shows `VM` on every page after sign-in, so the other sweeps cover it. No dialog sweep: no Radix dialog is swept today, and the build environment had no browser to see how axe treats Radix's focus guards.
- [ ] **Step 2: Run it.** `T test:e2e e2e/axe.spec.ts` needs Docker (tuwunel) and Playwright browsers that can launch. Expected: PASS in both themes. **Not run in the build environment:** the cached Chromium is build 1208 (the repo's Playwright wants 1234) and the host lacks the shared libraries it needs (`libatk`, `libgbm`, and others).
- [x] **Step 3: Check types.** The directory is outside `tsc --noEmit`, so a temporary tsconfig over `e2e/` checked it. `axe.spec.ts` is clean. The only error is the existing `Preset` type in `e2e/fixtures/daemon-impersonator.ts`.
- [x] **Step 4: Commit.** `test(web): sweep the VM page with axe`.

---

## Self-review

- **Spec coverage.** Rail button (Task 3), list with state, setup, and `Confidential` (Task 2), row actions by state with delete confirmation (Task 2), polling 5 s or 30 s (Task 2), New VM dialog with both modes, instance types, secrets, inline errors with `step` (Task 4), client with token cache, one retry, typed functions (Task 1), no-access state (Tasks 2 and 3), stories (Tasks 2 and 4), axe (Task 5).
- **Contract gap, resolved.** Contracts section 2 does not say what the top-level `name` is for an existing agent. The build sends the workstation name, because the VM name is the workstation name. The coordinator confirmed that the daemon ignores `agent.name` in existing mode and uses the top-level `name`.
- **Out of scope.** Success toasts (decided: none; Reboot shows no visible change), pausing the poll in a hidden tab, readable names for `step` values, a note on non-SEV-SNP types in the dialog (decided: the `Confidential` marker is enough), a dialog axe sweep.
