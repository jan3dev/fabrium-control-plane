# Control Plane Backend for Fabrium

The control plane API for Fabrium. See the [system design](https://github.com/jan3dev/fabrium) for the full picture.

The control plane launches and manages the agent VMs of each tenant on AWS EC2. Each new VM gets its bootstrap payload (files and a setup script) once. The caller of the API is the zooid server of a tenant. The key name in `API_KEYS` is the tenant.

## Quick start with Docker

1. Copy the example environment file.

   ```sh
   cp .env.example .env
   ```

2. Generate a key and keep it safe.

   ```sh
   KEY=$(openssl rand -hex 32)
   ```

3. Hash the key. Use `shasum -a 256` instead of `sha256sum` on macOS.

   ```sh
   printf %s "$KEY" | sha256sum
   ```

4. Set these variables in `.env`. Use only the 64-character hash, not the key.

   ```sh
   API_KEYS=stage:<hash>
   PUBLIC_URL=https://localhost
   ```

   `PUBLIC_URL` must be an https URL. A VM can redeem its token only if the URL is the real, public one. The VM routes also need AWS credentials. See [Configuration](#configuration).

5. Start the service. The first build needs a lot of memory. See [Build memory](#build-memory).

   ```sh
   docker compose up --build
   ```

6. Call the API from another terminal.

   ```sh
   curl -i localhost:8080/health
   # 200 {"status":"ok"}

   curl -i -H "Authorization: Bearer $KEY" localhost:8080/v1/ping
   # 200 {"pong":true,"client":"stage"}
   ```

The database lives in the `data` Docker volume. It survives restarts.

## Configuration

| Variable | Default | Docker value | Notes |
|----------|---------|--------------|-------|
| `API_KEYS` | none (required) | from `.env` | Comma-separated `<tenant>:<sha256-hex>` pairs. A tenant name uses only `a-z`, `0-9`, and `-`, because it becomes the value of the `fabrium:tenant` tag. The service refuses to start if the variable is missing, empty, or malformed, if a name is not valid, or if two entries share a name or a hash. |
| `PUBLIC_URL` | none (required) | from `.env` | The base URL that VMs use to redeem their token. It must start with `https://` and must not hold a quote or a space. The service removes a trailing `/`. |
| `AWS_REGION` | `us-east-2` | from `.env` | |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` | none | from `.env` | The AWS SDK reads them from the environment. Temporary credentials expire. Put new ones in `.env` and recreate the container. |
| `AMI_NAME` | `ubuntu-24.04-jan3-dev-env-v3` | from `.env` | The VM image. The service uses the newest available image that the account owns with this name. |
| `INSTANCE_TYPES` | `t3a.medium,t3a.large,t3.medium,t3.large,m6a.large,c6a.large,r6a.large` | from `.env` | The allowlist, in the order that the API shows it. Each entry is `<family>.<size>`. The family is `t3`, `t3a`, `c6a`, `m6a`, or `r6a`. The service refuses to start with another family or with a duplicate. |
| `DEFAULT_INSTANCE_TYPE` | `t3a.medium` | from `.env` | Must be in `INSTANCE_TYPES`. |
| `SECURITY_GROUP` | `jan3-dev-env-ssh` | from `.env` | The security group of new VMs, by name. |
| `KEY_NAME` | `fabrium-vms` | from `.env` | The key pair of new VMs, for debugging. |
| `CP_HOST` | none | from `.env` | The host name that Caddy serves. Only `compose.caddy.yaml` uses it. |
| `BIND_ADDR` | `127.0.0.1:8080` | `0.0.0.0:8080` | Set by the image. |
| `DATABASE_URL` | `sqlite://data/control-plane.db` | `sqlite:///data/control-plane.db` | Set by the image. The service creates the file and its directory. |
| `RUST_LOG` | `info` | from `.env` | Log filter. At `trace`, the AWS SDK logs whole requests, and the launch request holds the token of the new VM in its user data. Keep `info` in production. |

An empty value counts as not set. A `.env` file in the working directory is loaded automatically for local runs.

## Local development without Docker

You need Rust 1.99 or newer.

```sh
cargo run
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

`cargo run` needs `API_KEYS` and `PUBLIC_URL` in your environment or in `.env`. The tests use an in-memory fake of EC2 and canned EC2 responses. They need no AWS account and no network.

## Migrations

The service runs pending migrations at startup. Use the CLI to run them by hand.

```sh
cargo run -p migration -- up
cargo run -p migration -- down
cargo run -p migration -- status
```

The CLI reads `DATABASE_URL`. There are no migrations yet. Generate entities later with `sea-orm-cli`.

## API

Every route except `/health` and `/v1/bootstrap` needs `Authorization: Bearer <tenant API key>`. The server hashes the key with SHA-256 and compares it with the stored hashes. The scheme name is case-insensitive, and extra spaces before the key are ignored. A missing header, a wrong scheme, or an unknown key returns `401` with `WWW-Authenticate: Bearer` and `{"error":"missing or invalid API key"}`.

| Method | Path | Success | Errors |
|--------|------|---------|--------|
| GET | `/health` | `200 {"status":"ok"}` | `503 {"status":"unavailable"}` if the database does not answer |
| GET | `/v1/ping` | `200 {"pong":true,"client":"<tenant>"}` | `401` |
| GET | `/v1/instance-types` | `200 [type]` | `502` |
| GET | `/v1/vms` | `200 [vm]`, sorted by `name` | `502` |
| POST | `/v1/vms` | `201 vm` | `400`, `409`, `413`, `502` |
| POST | `/v1/vms/{id}/start` | `202 vm` | `404`, `409`, `502` |
| POST | `/v1/vms/{id}/stop` | `202 vm` | `404`, `409`, `502` |
| POST | `/v1/vms/{id}/reboot` | `202 vm` | `404`, `409`, `502` |
| DELETE | `/v1/vms/{id}` | `202`, empty body | `404`, `502` |
| POST | `/v1/bootstrap` | `200 {"files":[...],"script":"..."}` | `404` |

Every error body is `{"error":"<short message>"}`. A `413` can have a plain-text body. Clients must branch on the status code, not on the message.

### Objects

A VM:

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

- `state` is the EC2 state name: `pending`, `running`, `stopping`, `stopped`, or `shutting-down`. The API never shows `terminated`.
- `public_ip` is a string, or `null`.
- `launched_at` is the EC2 launch time in UTC, in whole seconds. EC2 resets it at each start.
- `sev_snp` is `true` for the families `c6a`, `m6a`, and `r6a`. The service enables AMD SEV-SNP for exactly those.
- `setup` is `delivered` if the VM redeemed its token, `pending` if its token can still be redeemed, and `expired` if not. An `expired` VM never got its payload. Delete it and create it again.

An instance type, in the order of `INSTANCE_TYPES`. The sizes come from EC2 and are cached after the first success.

```json
{ "name": "t3a.medium", "vcpus": 2, "memory_mib": 4096, "sev_snp": false, "default": true }
```

### Create a VM

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
|-------|-------------------|
| `name` | `^[a-z0-9][a-z0-9-]{0,62}$` |
| `instance_type` | Optional. In `INSTANCE_TYPES`. The default is `DEFAULT_INSTANCE_TYPE`. |
| `files[].path` | Starts with `/`. No `..` segment. |
| `files[].mode` | `^[0-7]{3,4}$` |
| `files[].owner` | `^[a-z_][a-z0-9_-]{0,31}$` |
| `files[].content` | A string. |
| `script` | A string that is not empty. |

A body larger than 256 KiB gives `413`. If a VM of the same tenant that is not terminated has the name, the result is `409 {"error":"name in use"}`. A VM of another tenant, or a terminated VM, does not count. A VM that you delete keeps its name until EC2 reports it as `terminated`, usually after about a minute, so a create with the same name right after a delete can give `409`.

### Actions

| Action | Allowed state | The `202` shows |
|--------|---------------|-----------------|
| start | `stopped` | `pending` |
| stop | `running` | `stopping` |
| reboot | `running` | `running` |
| delete | any state that the list shows | |

Another state gives `409 {"error":"vm is <state>"}`. EC2 still reports the old state for a moment, so the `202` shows the state that the action causes.

An `{id}` that does not match `^i-[0-9a-f]{8,17}$`, a VM of another tenant, a VM without the `fabrium:tenant` tag, and a terminated VM all give `404 {"error":"not found"}`. A response never shows that a VM of another tenant exists. The code checks the tag of every VM that EC2 returns. IAM backs this up.

A failed AWS call gives `502 {"error":"aws request failed"}`. The AWS detail goes to the log.

### Bootstrap

The user data of a new VM is a short script with the public URL and a single-use token. The token is 32 random bytes in base64url. The service keeps only its SHA-256, with the payload, in memory for 15 minutes.

The VM sends `POST /v1/bootstrap` with `Authorization: Bearer <token>`. The service removes the entry, sets the tag `fabrium:setup=delivered` on the instance, and returns the payload. A second try, an unknown token, an expired token, and a missing header all give `404 {"error":"not found"}`. A failed or slow tag (5 s limit) is logged, and the VM still gets its payload. The token is not a tenant API key, and a tenant API key is not a token.

A restart of the service loses the payloads that no VM redeemed yet. Those VMs show `expired`.

Logs never contain tokens, payloads, or file contents.

## Deploy on the control host

The host runs Docker Compose with Caddy in front of the service. Caddy gets the TLS certificate. For tests, use the host name `<ip-with-dashes>.sslip.io`. Open the ports 80 and 443 in the firewall.

1. Clone the repository on the host.

   ```sh
   git clone https://github.com/jan3dev/fabrium-control-plane.git
   cd fabrium-control-plane
   ```

2. Create `.env` and set every variable of [Configuration](#configuration) that has no default: `API_KEYS`, `PUBLIC_URL`, `CP_HOST`, and the AWS credentials. `PUBLIC_URL` is `https://` plus `CP_HOST`.

   ```sh
   cp .env.example .env
   ```

3. Start the service and Caddy. Read [Build memory](#build-memory) first.

   ```sh
   docker compose -f compose.yaml -f compose.caddy.yaml up -d --build
   ```

4. Check it.

   ```sh
   curl -i https://<host>/health
   ```

`compose.caddy.yaml` adds the `caddy` service on the ports 80 and 443, with the named volumes `caddy_data` and `caddy_config`. The `Caddyfile` sends every request to `control-plane:8080`. Caddy sees only `CP_HOST`, not the AWS keys.

### Build memory

The AWS SDK crate `aws-sdk-ec2` is very large. Measured on a 5-core machine:

| Build | Time | Peak memory of the compiler |
|-------|------|-----------------------------|
| `cargo check` (no code generation) | 2 min | 5.6 GiB |
| Release, SDK crate at `opt-level = 0` | 4 min | 7.3 GiB |
| Release, default profile (what the Dockerfile builds) | 7.5 min | 8.3 GiB |

The `control` instance has 1.9 GiB of RAM, so the build runs out of memory there. Build the image on a machine with at least 10 GiB of free RAM, and load it on the host with `docker save` and `docker load`. A registry works too. A large swap file only makes the build very slow.

## IAM

`docs/iam-policy.json` is the policy of the AWS identity that the control plane uses. Replace `<ACCOUNT_ID>` and `<SECURITY_GROUP_ID>`, and validate it with IAM Access Analyzer before use.

- `Describe*` on every resource, because EC2 has no resource-level permissions for describe actions.
- `RunInstances` only with the request tag `fabrium:tenant`, IMDSv2 required, and an instance type of an allowed family. It accepts only images that the account owns with the tag `Project=jan3-dev-env`, one security group, and one key pair.
- `CreateTags` only during a launch, plus the single key `fabrium:setup` on instances that have `fabrium:tenant`.
- Start, stop, reboot, and terminate only on instances that have `fabrium:tenant`.
- No `iam:PassRole`, because VMs get no instance profile.

The code enforces tenant isolation. IAM keeps the control plane away from every instance that Fabrium did not launch or adopt.
