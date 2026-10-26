# VM management from the Fabrium web UI

Workspace admins create, list, start, stop, reboot, and delete agent VMs from the Fabrium web UI. A new VM boots from the agent VM image, receives its Matrix identity and its secrets once, and its agent comes online in the workspace. Nobody uses SSH. The control plane owns the EC2 lifecycle. zooid owns Matrix identities and secrets. One control plane serves every zooid instance (prod and stage), and each zooid sees only its own VMs.

| | |
|---|---|
| Status | Design approved on 2026-10-26. Not built. |
| Repositories | `fabrium-control-plane` (this repository), `fabrium-web-ui` (zooid fork: daemon, deploy, and web), `fabrium` (docs and scripts) |
| Replaces | `scripts/create-vm.sh`, `scripts/start-vm.sh`, and `scripts/stop-vm.sh` in `fabrium`. This work deprecates them. |

## Quick path: create a VM

1. An admin opens **VM** in the workspace rail and selects **New VM**.
2. The admin selects a new agent (VM name and agent name) or a free existing agent, and an instance type. The admin types a Haven API key and, optionally, a GitHub token.
3. The browser sends the form to its zooid at `POST /_fabrium/v1/vms` with a Matrix OpenID token.
4. zooid checks that the user is an admin or an owner of the workspace.
5. zooid grants or loads the agent identity. Then it builds a bootstrap payload: the bundle files, the secrets, and a setup script.
6. zooid calls `POST /v1/vms` on the control plane with its tenant API key.
7. The control plane launches the VM. The user data holds only a single-use token and the control plane URL. The payload stays in memory for 15 minutes.
8. The VM boots and redeems the token at `POST /v1/bootstrap`. It writes the files and runs the setup script: `provision.sh`, then `configure-agent.sh`.
9. The agent joins its rooms. The VM list shows `setup: delivered`, and the agent is online in Matrix.

## Decisions

| Topic | Decision | Reason |
|---|---|---|
| Matrix identity | zooid grants it. The control plane never sees Matrix. | zooid already owns the workforce model and the central appservice. |
| Caller of the control plane | zooid, server to server. The browser never calls the control plane. | The tenant comes from the API key, zooid resolves roles, and Matrix tokens never pass through the browser. |
| Tenancy | One control plane for all zooid instances. The API key name is the tenant. Every VM has the tag `fabrium:tenant`. | Prod and stage share one service and one AWS account. The code enforces the isolation. |
| Secret delivery | A single-use bootstrap token in the user data. The VM redeems it once over HTTPS. | Every PowerUser of the account can read user data in plain text, and it stays for the life of the VM. Design §10.3 already defines a bootstrap token. |
| GitHub credential | The admin pastes a token in the form. No deploy keys and no GitHub App tokens in v1. | It is the simplest path that works. Deploy keys are SSH only and need repository admin rights. |
| Identity registration | Runtime appservice registration through the tuwunel admin room (`!admin appservices register`). | No tuwunel restart for each new agent. A restart interrupts chat for every user. |
| Instance types | Allowlist: `c6a`, `m6a`, and `r6a` (SEV-SNP on), `t3`, and `t3a`. Default `t3a.medium`. | `t3a.medium` (2 vCPU, 4 GiB) is the cheapest 4 GiB type that runs the x86_64 image: 27.45 USD per month in Ohio, plus about 1.60 USD for the disk (`fabrium/docs/tech/ec2-instance-types.md`). arm64 types are out of scope. |
| VM name | Unique per tenant among VMs that are not terminated. It is also the workstation name. | One VM per identity, with no extra state. |
| Delete | Terminates the VM. The identity stays and can go to a new VM. | Agents outlive VMs. |
| Hosting | The control plane runs on the dedicated `control` instance, behind Caddy. AWS credentials come from environment variables. | It does not depend on the prod or stage zooid hosts. |
| Design exception | v1 puts Haven, GitHub, and Matrix credentials in the VM. This breaks design §2 (principle 3, "Secrets never enter the sandbox") and §10.5. A decision record in `fabrium/docs/decisions/` documents the exception. | The broker that keeps keys out of the VM does not exist yet. |

## Architecture

```text
Browser ──OpenID──▶ Caddy ──/_fabrium/*──▶ zooid daemon ──API key──▶ control plane ──▶ EC2
                      │                        │                          ▲
                      └──/_matrix/*──▶ tuwunel ◀── AS tokens               │ POST /v1/bootstrap
                                                                           │ (token, once)
                                                               agent VM ───┘
```

| Component | Owns | Never touches |
|---|---|---|
| Web (`packages/web`) | VM page, New VM dialog, OpenID token, polling | The control plane; secrets after submit |
| zooid daemon | Browser API, role check, identity admission, bootstrap payload, tenant API key | EC2 |
| Control plane | EC2 lifecycle, tenant isolation, instance type allowlist, bootstrap delivery | Matrix, Haven, and GitHub semantics |
| Agent VM | Redeems the token once, runs the setup script, runs the agent | The control plane API key |

## Control plane

### API

Every route except `/health` and `/v1/bootstrap` needs `Authorization: Bearer <tenant API key>`. The key name in `API_KEYS` is the tenant. The parser accepts one key per name. A tenant name matches `^[a-z0-9-]+$`, because it becomes a tag value; the service does not start with another name.

| Method and path | Request | Response |
|---|---|---|
| `GET /v1/instance-types` | None | `200 [{name, vcpus, memory_mib, sev_snp, default}]` |
| `GET /v1/vms` | None | `200 [{id, name, state, instance_type, sev_snp, public_ip, launched_at, setup}]` |
| `POST /v1/vms` | `{name, instance_type?, bootstrap: {files: [{path, mode, owner, content}], script}}` | `201 {vm}` with `state: pending` |
| `POST /v1/vms/{id}/start` | None | `202 {vm}` |
| `POST /v1/vms/{id}/stop` | None | `202 {vm}` |
| `POST /v1/vms/{id}/reboot` | None | `202 {vm}` |
| `DELETE /v1/vms/{id}` | None | `202` |
| `POST /v1/bootstrap` | `Authorization: Bearer <bootstrap token>` | `200 {files, script}` once, then `404` |

### Rules

- A VM of another tenant, or a VM that does not exist, gives `404`. A response never shows that a VM of another tenant exists.
- `name` matches `^[a-z0-9][a-z0-9-]{0,62}$`. If a VM of the same tenant that is not terminated has the name, the result is `409`.
- `instance_type` must be in the allowlist. If the request has no `instance_type`, the default applies.
- An action that the current state does not allow, for example a reboot of a stopped VM, gives `409`.
- A create holds an in-process lock for its tenant and name, so two concurrent requests cannot both pass the name check. `ponytail:` single-process lock; a shared lock is necessary only with more than one control plane instance.
- The list hides terminated instances.
- The bootstrap payload is 256 KiB or less. A larger payload gives `413`.
- `setup` needs no database:
  - `delivered`: the instance has the tag `fabrium:setup=delivered`. The control plane sets the tag when the VM redeems its token.
  - `pending`: the instance has no tag and launched less than 15 minutes ago.
  - `expired`: the instance has no tag and launched 15 minutes ago or more. The VM never got its payload. Delete it and create it again.

### Bootstrap

- The token is 32 random bytes, base64url. The store keeps only the SHA-256 hash of the token, with the payload, the instance ID, and the expiry (15 minutes).
- A redeem removes the entry, sets the `fabrium:setup` tag, and returns the payload. A second redeem gives `404`.
- A delete removes the entry of that VM, if it is still there.
- The store is in memory. `ponytail:` a restart loses the payloads that are not redeemed; those VMs show `expired` and must be created again. Persist the store only if restarts during launches become frequent.
- Logs never contain tokens, payloads, or file contents.
- The user data is a short bash script (Appendix B). Cloud-init runs it once, as root. It tries the redeem for up to 10 minutes, writes each file with its mode and owner, runs `script` as root, and removes the payload from `/run`.

### EC2 launch

| Parameter | Value | Source |
|---|---|---|
| Region | `us-east-2` | `AWS_REGION` |
| AMI | The newest image that the account owns with the name `ubuntu-24.04-jan3-dev-env-v3` | `AMI_NAME` |
| Instance type | From the request. Default `t3a.medium`. | `INSTANCE_TYPES`, `DEFAULT_INSTANCE_TYPE` |
| SEV-SNP | `CpuOptions.AmdSevSnp=enabled` for `c6a`, `m6a`, and `r6a` | Code |
| Security group | `jan3-dev-env-ssh` | `SECURITY_GROUP` |
| Key pair | `fabrium-vms`, for debugging | `KEY_NAME` |
| Root volume | 20 GiB, gp3, encrypted | Code |
| Instance metadata | IMDSv2 required | Code |
| Instance profile | None | None |
| Tags on the instance and the volume | `Name=<name>`, `Project=jan3-dev-env`, `fabrium:tenant=<tenant>` | Code |
| User data | Bootstrap stub with `PUBLIC_URL` and the token | `PUBLIC_URL` |

These values move the logic of `scripts/create-vm.sh`, `scripts/start-vm.sh`, and `scripts/stop-vm.sh` into this service.

### Configuration

| Variable | Example | Note |
|---|---|---|
| `API_KEYS` | `stage:<sha256>,prod:<sha256>` | Exists. The name is the tenant. |
| `PUBLIC_URL` | `https://<host>` | Base URL that VMs use to redeem tokens. |
| `AWS_REGION` | `us-east-2` | |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` | None | Temporary credentials for now. The dedicated profile replaces them. |
| `AMI_NAME` | `ubuntu-24.04-jan3-dev-env-v3` | |
| `INSTANCE_TYPES` | `t3a.medium,t3a.large,t3.medium,t3.large,m6a.large,c6a.large,r6a.large` | The allowlist. |
| `DEFAULT_INSTANCE_TYPE` | `t3a.medium` | Must be in the allowlist. |
| `SECURITY_GROUP` | `jan3-dev-env-ssh` | |
| `KEY_NAME` | `fabrium-vms` | |

### IAM

The production profile gets the policy in Appendix A. In short:

- `Describe*` on `*`, because EC2 has no resource-level permissions for describe actions.
- `RunInstances` needs the request tag `fabrium:tenant`, IMDSv2, and an instance type of an allowed family. It accepts only images that the account owns with the tag `Project=jan3-dev-env`, one security group, and one key pair.
- `CreateTags` only during a launch, plus the single key `fabrium:setup` on instances that have `fabrium:tenant`.
- Start, stop, reboot, and terminate only on instances that have `fabrium:tenant`.
- No `iam:PassRole`, because VMs get no instance profile.
- The code enforces the tenant isolation. IAM keeps the control plane away from every instance that Fabrium did not launch or adopt.

### Deploy

- Docker Compose on the `control` instance (Ubuntu 24.04, 2 vCPU, 1.9 GiB RAM), with Caddy in front for TLS.
- For tests, the host name is `<ip-with-dashes>.sslip.io`. The final DNS name replaces it.
- The Lightsail firewall opens ports 80 and 443.

## zooid (fabrium-web-ui)

### Browser API

The routes mount on the Hono app of the daemon under `/_fabrium/v1`. Caddy sends `/_fabrium/*` to `zooid:9000`, before the catch-all of the web UI. The origin is the same as the web UI, so CORS is not necessary.

| Method and path | Does |
|---|---|
| `GET /_fabrium/v1/vms` | The control plane list, plus the agent ID from each bundle |
| `POST /_fabrium/v1/vms` | Admission, payload, and control plane create |
| `POST /_fabrium/v1/vms/:id/start`, `/stop`, `/reboot` | Proxy to the control plane |
| `DELETE /_fabrium/v1/vms/:id` | Proxy to the control plane |
| `GET /_fabrium/v1/instance-types` | Proxy to the control plane |
| `GET /_fabrium/v1/agents` | Bundles as `{workstation, agent_id, preset, free}` |

Create request:

```json
{
  "name": "lima",
  "instance_type": "t3a.medium",
  "agent": { "mode": "new", "name": "echo", "persona": "Reviewer", "project": "Fabrium" },
  "haven_api_key": "<haven key>",
  "github_token": "<github token>"
}
```

With `"agent": {"mode": "existing"}`, `name` is the workstation of that agent.

### Authentication and role

1. The browser sends `Authorization: Bearer <OpenID access token>`.
2. zooid calls `GET /_matrix/federation/v1/openid/userinfo` on the internal tuwunel. The `sub` must be a user of this server.
3. zooid caches the result for 5 minutes or less.
4. The user must have power level 90 or more (admin) in the workforce space, or be its v12 creator (owner). These are the thresholds of `packages/web/src/lib/roles.ts`. A user that is not joined gets `403`.

`FABRIUM_WEB_UI_PLAN.md` (line 95) planned OpenID tokens for the service API. The only change is that zooid serves that API.

### Admission

New agent, as a port of `deploy/add-workstation.sh`:

1. Check the names. The workstation matches `[a-z0-9-]+` and is not `zooid`. The agent matches `^[a-z][a-z0-9-]{0,31}$`. Refuse if `workstations/<ws>/` or `registrations/<ws>.yaml` exists.
2. Generate the AS token and the HS token.
3. Write the bundle in `workstations/<ws>/`: `.env` and `workforce/zooid.yaml`, in the format of the script, with preset `opencode` and the rooms from the configuration. Add the marker file `.runtime-registration`.
4. As `@zooid`, send `!admin appservices register` with the registration YAML to the tuwunel admin room.
5. Poll `whoami` with the new AS token until it returns `@<ws>:<server>`, for 30 seconds or less.
6. Register `@<ws>.<agent>`. Join it and `@<ws>` to the workforce space, and join the agent to the configured rooms. The central token invites; the VM token joins.
7. Publish the roster entry `dev.zooid.workforce` with the state key `<ws>`.

Each step checks the current state before it writes. A retry after a failure continues where the failure stopped.

Existing agent:

- zooid reads `workstations/<ws>/`. The agent is free if no VM of this tenant that is not terminated has that name.
- Only bundles with preset `opencode` qualify, because `configure-agent.sh` requires it.

Guards:

- `add-workstation.sh` refuses a workstation that has the `.runtime-registration` marker. If an ID is in the registrations directory and in the tuwunel database, the next tuwunel start stops loading the registrations that come after it.
- "Remove a VM" in `deploy/README.md` gets a runtime variant: `!admin appservices unregister <ws>`.
- One-time setup: `@zooid` joins the tuwunel admin room.

### Bootstrap payload

| File | Owner and mode | Content |
|---|---|---|
| `/home/ubuntu/zooid/.env` | `ubuntu`, 600 | The bundle `.env` |
| `/home/ubuntu/zooid/workforce/zooid.yaml` | `ubuntu`, 600 | The bundle `zooid.yaml` |
| `/run/fabrium-agent-secrets.env` | `ubuntu`, 600 | `HAVEN_API_KEY`, `HAVEN_MODEL`, `GH_TOKEN` |

The script runs as root:

1. Wait for the user manager of `ubuntu`. Linger is on in the image.
2. As `ubuntu`, run `bash ~/fabrium-web-ui/deploy/agent-vm/provision.sh --ref <zooid commit>`.
3. As `ubuntu`, with the secrets in the environment, run `bash ~/fabrium-web-ui/deploy/agent-vm/configure-agent.sh`.
4. Delete `/run/fabrium-agent-secrets.env`.

### Configuration

| Variable | Note |
|---|---|
| `FABRIUM_CONTROL_PLANE_URL` | Base URL of the control plane |
| `FABRIUM_CONTROL_PLANE_KEY_FILE` | Path to a file (mode 600) with the tenant API key. The key is in a file, not in the environment, because local agents inherit the environment of the daemon. |
| `FABRIUM_HAVEN_MODEL` | Default Haven model, for example `glm-5-3` |
| `FABRIUM_VM_ROOMS` | Rooms for new agents. Default `#general`. |

## Web (packages/web)

- **Navigation:** a `VM` rail button below Home opens `/vm`. Only admins and owners of the workspace see it. Other users get a no-access state on that route.
- **List:** name, agent, type, state (EC2 state and setup), IP, and launch date. SEV-SNP types show a `Confidential` marker with the `--trust-enclave` token.
- **Row actions by state:** start when stopped. Stop and reboot when running. Delete always, after a confirmation.
- **Refresh:** every 5 seconds while a VM is in transition or in setup. Every 30 seconds in other cases.
- **New VM dialog:**
  - Agent mode. New: VM name, agent name, and optional persona and project. Existing: a select of free agents.
  - Instance type. Default `t3a.medium`.
  - Haven API key (required) and GitHub token (optional). Both are password inputs, and the UI never shows their values again.
  - The dialog stays open while it creates, and shows errors inline, as `create-room.tsx` does.
- **Data:** `src/client/fabrium-api.ts` gets and renews the OpenID token and calls `/_fabrium/v1`. A `use-vms` hook uses plain `fetch`, with no TanStack Query.
- **Repository rules:** a story for each presentational component, icons from the barrel, semantic tokens only, kebab-case file names, no `jan3` or `aqua` strings (fixtures use neutral names), the new route in `e2e/axe.spec.ts`, and no "server" or "bot" in UI copy.

## Errors

| Where | Case | Result |
|---|---|---|
| Control plane | Input not valid | `400 {error}` |
| Control plane | VM missing, or VM of another tenant | `404` |
| Control plane | Name in use, or action not allowed in the current state | `409` |
| Control plane | Payload too large | `413` |
| Control plane | AWS error | `502 {error}` with a short message. The AWS detail goes to the log. |
| zooid | OpenID token not valid or expired | `401` |
| zooid | User is not an admin or an owner | `403` |
| zooid | An admission step fails | `502` with the name of the step. A retry continues. |
| zooid | The control plane fails after the admission | The identity stays free for the next attempt. |
| VM | The redeem fails for 10 minutes | Setup `expired`. The log is `/var/log/fabrium-bootstrap.log`. |
| Web | Dialog error | Inline message. A row action error shows as a toast. |

The control plane keeps its request timeout of 10 seconds. EC2 calls return in seconds, and no request waits for a VM to boot.

## Security notes

- The bootstrap token stays in the user data until the VM redeems it, 15 minutes or less. A PowerUser can read it there. Because the token works once, a theft is visible: the redeem of the real VM gets `404`, its agent never comes online, and `/var/log/fabrium-bootstrap.log` on the VM shows the failure. The list shows `delivered`, because the redeem of the thief set the tag.
- v1 does not protect against insiders of the AWS account. A PowerUser can also make a snapshot of a disk.
- Secrets on the VM: `~/zooid/.env` (the AS token of the VM) and `~/zooid/secrets.env` (Haven and GitHub), mode 600. The agent can read them. "Security" in `deploy/README.md` accepts this.
- Types without SEV-SNP are allowed, and the UI marks them. `fabrium/docs/tech/create-vm-from-image.md` advises against secrets on them.
- `@zooid` becomes a tuwunel admin. Its `@.*` appservice namespace already lets it act as any local user.
- OpenID tokens give the browser API the least privilege. They cannot call other Matrix endpoints.

## Testing

- [ ] Control plane unit tests with a fake EC2: tenant filter, `404` across tenants, `409`, allowlist and default, SEV-SNP only on `c6a`, `m6a`, and `r6a`, single-use token, TTL, `setup` states, payload limit.
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --check` pass.
- [ ] `RunInstances` dry runs against AWS, for one type of each family.
- [ ] zooid vitest: OpenID check, role gate, proxy, admission against a fake Matrix, payload builder.
- [ ] Web vitest and Testing Library: page, dialog, hook. Stories. Axe.
- [ ] Stage, end to end: create a `t3a.medium` VM with a new agent from the UI. The agent answers in `#general`.
- [ ] Stage, end to end: stop, start, reboot, and delete.
- [ ] Stage, end to end: put an existing identity on a new VM.
- [ ] Isolation: a second key (tenant `prod-sim`) cannot see or change the stage VMs.

## Delivery

1. Control plane: small pull requests, one for each work unit.
2. zooid daemon and deploy changes, on `main` after the upstream zooid sync.
3. Web.
4. `fabrium` docs: deprecate the three scripts, update `docs/tech/create-vm-from-image.md` and `scripts/AGENTS.md`, and add the decision record. The design owner reviews the decision record.
5. Stage: tag kingston into the `stage` tenant (`fabrium:tenant=stage`, `fabrium:setup=delivered`).

Final configuration by the user:

- [ ] A dedicated IAM profile with the policy in Appendix A.
- [ ] A DNS name for the control plane.
- [ ] The prod tenant API key in the prod zooid.
- [ ] `@zooid` in the admin room of the prod tuwunel.

## To verify during implementation

- NitroTPM and UEFI support for `t3` and `t3a` with the v3 image (`describe-instance-types`). kingston already runs on `t3a`.
- The exact tuwunel 1.9.3 syntax of `appservices register`, and how `@zooid` joins the admin room.
- Where the zooid image exposes its commit for `--ref`.
- Whether an encrypted root volume with the default EBS key needs a KMS permission.
- Build time and memory of the control plane image on the `control` instance (2 vCPU, 1.9 GiB).

## Out of scope

GitHub deploy keys and App tokens, arm64 types and images, deleting agent identities, changes to a VM after launch, secret rotation, attestation, prices in the UI, and a database in the control plane.

## Appendix A: IAM policy (draft)

Replace `<ACCOUNT_ID>` and `<SECURITY_GROUP_ID>`. Validate with IAM Access Analyzer before use.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "Describe",
      "Effect": "Allow",
      "Action": [
        "ec2:DescribeImages",
        "ec2:DescribeInstances",
        "ec2:DescribeInstanceTypes",
        "ec2:DescribeSecurityGroups"
      ],
      "Resource": "*"
    },
    {
      "Sid": "RunTaggedInstance",
      "Effect": "Allow",
      "Action": "ec2:RunInstances",
      "Resource": "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:instance/*",
      "Condition": {
        "Null": { "aws:RequestTag/fabrium:tenant": "false" },
        "StringEquals": { "ec2:MetadataHttpTokens": "required" },
        "StringLike": { "ec2:InstanceType": ["t3.*", "t3a.*", "c6a.*", "m6a.*", "r6a.*"] }
      }
    },
    {
      "Sid": "RunFromFabriumImage",
      "Effect": "Allow",
      "Action": "ec2:RunInstances",
      "Resource": "arn:aws:ec2:us-east-2::image/*",
      "Condition": {
        "StringEquals": {
          "ec2:Owner": "<ACCOUNT_ID>",
          "ec2:ResourceTag/Project": "jan3-dev-env"
        }
      }
    },
    {
      "Sid": "RunWithFixedNetworkAndKey",
      "Effect": "Allow",
      "Action": "ec2:RunInstances",
      "Resource": [
        "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:security-group/<SECURITY_GROUP_ID>",
        "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:key-pair/fabrium-vms",
        "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:subnet/*",
        "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:network-interface/*",
        "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:volume/*"
      ]
    },
    {
      "Sid": "TagAtLaunch",
      "Effect": "Allow",
      "Action": "ec2:CreateTags",
      "Resource": [
        "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:instance/*",
        "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:volume/*"
      ],
      "Condition": { "StringEquals": { "ec2:CreateAction": "RunInstances" } }
    },
    {
      "Sid": "MarkSetupDelivered",
      "Effect": "Allow",
      "Action": "ec2:CreateTags",
      "Resource": "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:instance/*",
      "Condition": {
        "Null": { "aws:ResourceTag/fabrium:tenant": "false", "aws:TagKeys": "false" },
        "ForAllValues:StringEquals": { "aws:TagKeys": ["fabrium:setup"] }
      }
    },
    {
      "Sid": "Lifecycle",
      "Effect": "Allow",
      "Action": [
        "ec2:StartInstances",
        "ec2:StopInstances",
        "ec2:RebootInstances",
        "ec2:TerminateInstances"
      ],
      "Resource": "arn:aws:ec2:us-east-2:<ACCOUNT_ID>:instance/*",
      "Condition": { "Null": { "aws:ResourceTag/fabrium:tenant": "false" } }
    }
  ]
}
```

## Appendix B: user data stub (shape)

The control plane fills in `<PUBLIC_URL>` and `<TOKEN>`. The token goes to curl through its configuration on stdin, not on the command line.

```bash
#!/bin/bash
# Fabrium bootstrap stub. Cloud-init runs it once, as root, at first boot.
set -euo pipefail
exec >>/var/log/fabrium-bootstrap.log 2>&1
umask 077
URL='<PUBLIC_URL>/v1/bootstrap'
TOKEN='<TOKEN>'
PAYLOAD=/run/fabrium-bootstrap.json

deadline=$((SECONDS + 600))
while [ "$SECONDS" -lt "$deadline" ]; do
  if printf 'header = "Authorization: Bearer %s"\n' "$TOKEN" |
    curl -fsS -K - --max-time 20 -X POST -o "$PAYLOAD" "$URL"; then
    break
  fi
  sleep 10
done
[ -s "$PAYLOAD" ] || { echo "bootstrap: no payload"; exit 1; }

count="$(jq '.files | length' "$PAYLOAD")"
for i in $(seq 0 $((count - 1))); do
  path="$(jq -r ".files[$i].path" "$PAYLOAD")"
  owner="$(jq -r ".files[$i].owner" "$PAYLOAD")"
  if [ "$owner" = root ]; then
    mkdir -p "$(dirname "$path")"
  else
    sudo -u "$owner" mkdir -p -m 700 "$(dirname "$path")"
  fi
  jq -j ".files[$i].content" "$PAYLOAD" >"$path"
  chmod "$(jq -r ".files[$i].mode" "$PAYLOAD")" "$path"
  chown "$owner:" "$path"
done

jq -r '.script' "$PAYLOAD" >/run/fabrium-setup.sh
bash /run/fabrium-setup.sh
rm -f "$PAYLOAD" /run/fabrium-setup.sh
```
