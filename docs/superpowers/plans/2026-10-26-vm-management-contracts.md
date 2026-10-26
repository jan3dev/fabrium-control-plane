# VM management: shared contracts

The control plane, the zooid daemon, and the web UI share these shapes. The three implementation plans use the values here exactly. If a plan and this file disagree, this file wins. The design is in [the spec](../specs/2026-10-26-vm-management-design.md).

| Plan | Repository | Branch |
|---|---|---|
| [Control plane](2026-10-26-vm-management-control-plane.md) | `fabrium-control-plane` | `feat/vm-api` |
| [zooid daemon and deploy](2026-10-26-vm-management-zooid.md) | `fabrium-web-ui` | `feat/vm-daemon-api` |
| [Web](2026-10-26-vm-management-web.md) | `fabrium-web-ui` | `feat/vm-web` |

## 1. Control plane API (zooid to control plane)

Every route except `/health` and `/v1/bootstrap` needs `Authorization: Bearer <tenant API key>`. A missing or wrong key gives `401 {"error":"missing or invalid API key"}`.

Every error body is `{"error":"<short message>"}`. A `413` from the body limit can have a plain-text body. Clients branch on the status code, never on the message.

### VM object

```json
{
  "id": "i-0123456789abcdef0",
  "name": "lima",
  "state": "running",
  "instance_type": "t3a.medium",
  "sev_snp": false,
  "public_ip": "203.0.113.10",
  "launched_at": "2026-10-26T19:00:00Z",
  "setup": "delivered"
}
```

| Field | Rule |
|---|---|
| `state` | The EC2 state name: `pending`, `running`, `stopping`, `stopped`, or `shutting-down`. `terminated` never appears. |
| `public_ip` | A string, or `null`. |
| `launched_at` | EC2 `LaunchTime`, RFC 3339, UTC, whole seconds. EC2 resets it at each start. |
| `setup` | `delivered` if the instance has the tag `fabrium:setup=delivered`. Otherwise `pending` if the bootstrap store has a live entry for the instance. Otherwise `expired`. |

The `setup` rule refines the 15-minute rule of the spec. A store entry lives 15 minutes, so the result is the same. After a restart of the control plane, a VM that did not redeem its token shows `expired` at once, which is correct: its payload is gone.

### Instance type object

```json
{ "name": "t3a.medium", "vcpus": 2, "memory_mib": 4096, "sev_snp": false, "default": true }
```

The list keeps the order of `INSTANCE_TYPES`. `sev_snp` is true for the families `c6a`, `m6a`, and `r6a`.

### Routes

| Method and path | Success | Errors |
|---|---|---|
| `GET /v1/instance-types` | `200 [type]` | `502` |
| `GET /v1/vms` | `200 [vm]`, sorted by `name` | `502` |
| `POST /v1/vms` | `201 vm` (`state` is `pending`, `setup` is `pending`) | `400`, `409`, `413`, `502` |
| `POST /v1/vms/{id}/start` | `202 vm` | `404`, `409`, `502` |
| `POST /v1/vms/{id}/stop` | `202 vm` | `404`, `409`, `502` |
| `POST /v1/vms/{id}/reboot` | `202 vm` | `404`, `409`, `502` |
| `DELETE /v1/vms/{id}` | `202`, empty body | `404`, `502` |
| `POST /v1/bootstrap` | `200 {"files":[...],"script":"..."}` | `404 {"error":"not found"}` |

State rules for actions. Any other state gives `409 {"error":"vm is <state>"}`:

| Action | Allowed state |
|---|---|
| start | `stopped` |
| stop | `running` |
| reboot | `running` |
| delete | any state that the list shows |

An `{id}` that is not of the form `^i-[0-9a-f]{8,17}$`, a VM of another tenant, and a terminated VM all give `404 {"error":"not found"}`.

### Create request

```json
{
  "name": "lima",
  "instance_type": "t3a.medium",
  "bootstrap": {
    "files": [
      { "path": "/home/ubuntu/zooid/.env", "mode": "600", "owner": "ubuntu", "content": "..." }
    ],
    "script": "#!/bin/bash\n..."
  }
}
```

| Field | Rule (else `400`) |
|---|---|
| `name` | `^[a-z0-9][a-z0-9-]{0,62}$` |
| `instance_type` | Optional. In the allowlist. If absent, `DEFAULT_INSTANCE_TYPE`. |
| `files[].path` | Starts with `/`. No `..` segment. |
| `files[].mode` | `^[0-7]{3,4}$` |
| `files[].owner` | `^[a-z_][a-z0-9_-]{0,31}$` |
| `files[].content` | A string. |
| `script` | A string that is not empty. |

- A body larger than 256 KiB gives `413`.
- A VM of the same tenant with that name that is not terminated gives `409 {"error":"name in use"}`.

### Bootstrap redeem

- `POST /v1/bootstrap` with `Authorization: Bearer <bootstrap token>`. No tenant key.
- `200`: the `bootstrap` object of the create request, as JSON.
- `404 {"error":"not found"}`: a token that is unknown, already used, or expired.

## 2. zooid browser API (web to zooid)

- Same origin as the web UI. Prefix `/_fabrium/v1`. Caddy sends `/_fabrium/*` to `zooid:9000`.
- Every route needs `Authorization: Bearer <access_token>`, where the token comes from `client.getOpenIdToken()` of matrix-js-sdk.
- `401 {"error":"invalid token"}`: the web gets a new OpenID token and retries once.
- `403 {"error":"forbidden"}`: the user is not joined to the workforce space, or has a power level below 90 and is not its v12 creator.
- If `FABRIUM_CONTROL_PLANE_URL` or `FABRIUM_CONTROL_PLANE_KEY_FILE` is not set, the routes are not mounted, and every path gives `404`. The web treats `404` from `/access` as "VM management is off".
- Control plane errors pass through with their status and body. A failed connection to the control plane gives `502 {"error":"control plane unreachable"}`.
- zooid never logs or returns `haven_api_key` or `github_token`.

### Routes

| Method and path | Success | Note |
|---|---|---|
| `GET /_fabrium/v1/access` | `200 {"user_id":"@ann:example.org","role":"admin"}` | `role` is `admin` or `owner`. The web shows the VM rail button only after a `200`. |
| `GET /_fabrium/v1/vms` | `200 [vm & {"agent_id": string or null}]` | `agent_id` comes from the bundle `workstations/<name>/`. `null` if there is no bundle. |
| `GET /_fabrium/v1/instance-types` | `200 [type]` | Proxy. |
| `GET /_fabrium/v1/agents` | `200 [{"workstation":"lima","agent_id":"@lima.echo:example.org","preset":"opencode","free":true}]` | Every bundle, sorted by `workstation`. `free` is true if `preset` is `opencode` and no VM in the list has that name. |
| `POST /_fabrium/v1/vms` | `201 vm & {"agent_id": string}` | Body below. |
| `POST /_fabrium/v1/vms/:id/start`, `/stop`, `/reboot` | `202 vm` | Proxy. |
| `DELETE /_fabrium/v1/vms/:id` | `202`, empty body | Proxy. |

### Create request

```json
{
  "name": "lima",
  "instance_type": "t3a.medium",
  "agent": { "mode": "new", "name": "echo", "persona": "Reviewer", "project": "Fabrium" },
  "haven_api_key": "<haven key>",
  "github_token": "<github token>"
}
```

| Field | Rule (else `400`) |
|---|---|
| `name` | The VM name and the workstation name. `^[a-z0-9][a-z0-9-]{0,62}$`, and not `zooid`. |
| `instance_type` | Optional. Passed to the control plane. |
| `agent.mode` | `new` or `existing`. |
| `agent.name` | With `new`: required, `^[a-z][a-z0-9-]{0,31}$`. |
| `agent.persona`, `agent.project` | With `new`: optional strings. |
| `agent` with `existing` | `name` must be a free agent in `GET /_fabrium/v1/agents`. |
| `haven_api_key` | Required. Not empty. |
| `github_token` | Optional. An empty string is the same as absent. |

An admission step that fails gives `502 {"error":"<message>","step":"<step>"}`. `step` is one of `bundle`, `register_appservice`, `whoami`, `register_user`, `join`, `roster`. A retry with the same body continues from the step that failed.

## 3. Bootstrap payload (zooid builds it, the control plane holds it, the VM runs it)

| `path` | `owner` | `mode` | `content` |
|---|---|---|---|
| `/home/ubuntu/zooid/.env` | `ubuntu` | `600` | The bundle `.env` |
| `/home/ubuntu/zooid/workforce/zooid.yaml` | `ubuntu` | `600` | The bundle `workforce/zooid.yaml` |
| `/run/fabrium-agent-secrets.env` | `ubuntu` | `600` | `HAVEN_API_KEY`, `HAVEN_MODEL`, and `GH_TOKEN` (only if set), one `KEY='value'` per line. Each `'` in a value becomes `'\''`. |

The script runs as root, from the user data stub of the control plane (spec, Appendix B). Its shape:

```bash
#!/bin/bash
set -euo pipefail
uid="$(id -u ubuntu)"
for _ in $(seq 60); do
  systemctl is-active --quiet "user@${uid}.service" && break
  sleep 2
done
as_ubuntu() {
  sudo -u ubuntu -H env XDG_RUNTIME_DIR="/run/user/${uid}" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/${uid}/bus" "$@"
}
trap 'rm -f /run/fabrium-agent-secrets.env' EXIT
as_ubuntu bash -lc 'bash ~/fabrium-web-ui/deploy/agent-vm/provision.sh --ref <ZOOID_COMMIT>'
as_ubuntu bash -lc 'set -a; . /run/fabrium-agent-secrets.env; set +a; bash ~/fabrium-web-ui/deploy/agent-vm/configure-agent.sh'
```

`<ZOOID_COMMIT>` is the full commit of the running zooid image. The VM fetches it from the public `jan3dev/fabrium-web-ui` repository, so the commit must be pushed.

## 4. Tags

| Key | Value | Set by | When |
|---|---|---|---|
| `Name` | VM name | Control plane | Launch (instance and volume) |
| `Project` | `jan3-dev-env` | Control plane | Launch (instance and volume) |
| `fabrium:tenant` | Tenant name | Control plane | Launch (instance and volume) |
| `fabrium:setup` | `delivered` | Control plane | Redeem (instance) |

## 5. Tenants

| Tenant | zooid |
|---|---|
| `stage` | Stage zooid |
| `prod` | Prod zooid |
| `prod-sim` | Tests only: the isolation check |
