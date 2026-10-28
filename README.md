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
   API_SOURCES=stage:192.0.2.10
   PUBLIC_URL=https://localhost
   ```

   `API_SOURCES` lists the addresses that `stage` calls from. The example uses a documentation address. Step 6 sends the same address in `X-Forwarded-For`, because the quick start has no proxy. `PUBLIC_URL` must be an https URL. A VM can redeem its token only if the URL is the real, public one. The VM routes also need AWS credentials. See [Configuration](#configuration).

5. Start the service. The first build needs a lot of memory. See [Build memory](#build-memory).

   ```sh
   docker compose up --build
   ```

6. Call the API from another terminal.

   ```sh
   curl -i localhost:8080/health
   # 200 {"status":"ok"}

   curl -i -H "Authorization: Bearer $KEY" -H "X-Forwarded-For: 192.0.2.10" localhost:8080/v1/ping
   # 200 {"pong":true,"client":"stage"}
   ```

The database lives in the `data` Docker volume. It survives restarts.

## Configuration

| Variable | Default | Docker value | Notes |
|----------|---------|--------------|-------|
| `API_KEYS` | none (required) | from `.env` | Comma-separated `<tenant>:<sha256-hex>` pairs. A tenant name uses only `a-z`, `0-9`, and `-`, because it becomes the value of the `fabrium:tenant` tag. The service refuses to start if the variable is missing, empty, or malformed, if a name is not valid, or if two entries share a name or a hash. |
| `API_SOURCES` | none (required) | from `.env` | Comma-separated `<tenant>:<ip-or-cidr>` entries, for example `stage:203.0.113.7,stage:198.51.100.0/24`. A tenant can have several entries. A bare IP is one host. A host that can call over IPv6 needs its IPv6 address too. The key of a tenant works only from the addresses of that tenant (see [API](#api)). A CIDR range must start at its network address: `192.0.2.0/24`, not `192.0.2.5/24`. The service refuses to start if the variable is missing, empty, or malformed, if an entry names a tenant that `API_KEYS` does not have, or if a tenant of `API_KEYS` has no entry. |
| `PUBLIC_URL` | none (required) | from `.env` | The base URL that VMs use to redeem their token. It must start with `https://` and must not hold a quote or a space. The service removes a trailing `/`. |
| `AWS_REGION` | `us-east-2` | from `.env` | AWS has SEV-SNP on shared tenancy only in `us-east-2` and `eu-west-1`. In another region, the service refuses to start if `INSTANCE_TYPES` has a SEV-SNP type. |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` | none | from `.env` | The AWS SDK reads them from the environment. Temporary credentials expire. Put new ones in `.env` and recreate the container. |
| `AMI_NAME` | `ubuntu-24.04-jan3-dev-env-v3` | from `.env` | The VM image. The service uses the newest available image that the account owns with this name and with the tag `Project=jan3-dev-env`. |
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

`cargo run` needs `API_KEYS`, `API_SOURCES`, and `PUBLIC_URL` in your environment or in `.env`. The tests use an in-memory fake of EC2 and canned EC2 responses. They need no AWS account and no network.

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

A valid key also needs a source address. The address is the last `X-Forwarded-For` entry, which Caddy sets: the service listens on localhost only. An IPv4-mapped IPv6 address counts as its IPv4 address. The service checks the address against the `API_SOURCES` entries of the tenant of the key. A request from another address, or with no usable address, returns `403 {"error":"forbidden"}`, and the service logs a warning with the tenant and the address. The key check comes first, so a bad key always returns `401`.

| Method | Path | Success | Errors |
|--------|------|---------|--------|
| GET | `/health` | `200 {"status":"ok"}` | `503 {"status":"unavailable"}` if the database does not answer |
| GET | `/v1/ping` | `200 {"pong":true,"client":"<tenant>"}` | `401`, `403` |
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
- `sev_snp` is `true` for the sizes that AWS launches with AMD SEV-SNP: `c6a.large` to `c6a.16xlarge`, `m6a.large` to `m6a.8xlarge`, and `r6a.large` to `r6a.4xlarge`. The service enables SEV-SNP for exactly those. A larger size of these families launches without it.
- `setup` is `delivered` if the VM has the tag of a redeemed token, `pending` while its token is live (not redeemed yet, or in the 60 s after the first redeem, before the tag shows), and `expired` if not. An `expired` VM never got its payload. Delete it and create it again.

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
| `files[].path` | Starts with `/` and does not end with `/`. No `..` segment. No control character, such as a newline or a NUL byte. |
| `files[].mode` | `^[0-7]{3,4}$` |
| `files[].owner` | `^[a-z_][a-z0-9_-]{0,31}$` |
| `files[].content` | A string. |
| `script` | A string that is not empty. |

A body larger than 256 KiB gives `413`. If a VM of the same tenant that is not terminated has the name, the result is `409 {"error":"name in use"}`. A VM of another tenant, or a terminated VM, does not count. A VM that still waits for its token also holds its name, even if EC2 does not list it yet. A VM that you delete keeps its name until EC2 reports it as `terminated`, usually after about a minute, so a create with the same name right after a delete can give `409`.

Creates run one at a time: one lock, held across the AWS calls (about 3 s), stops two requests with one name from both passing the name check. A create also runs in a task of its own, so the 10 s request timeout cannot separate the launch from the insert of the payload: a VM that EC2 accepted always has its payload, and never holds its name without one. The task keeps the lock until it ends, and the SDK timeouts bound that time. To run creates in parallel, use a lock per tenant and name. With more than one control plane instance, the lock must be shared.

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

The user data of a new VM is a short script with the public URL and a token. The token is 32 random bytes in base64url. The service keeps only its SHA-256, with the payload, in memory for 15 minutes.

The VM sends `POST /v1/bootstrap` with `Authorization: Bearer <token>`. The service returns the payload, then sets the tag `fabrium:setup=delivered` on the instance. A failed or slow tag (5 s limit) is logged, and the VM still gets its payload.

For 60 s after the first redeem, a repeat from the same address gets the same payload, so a VM whose first answer got lost can ask again. The address is the last `X-Forwarded-For` entry, which Caddy sets: the service listens on localhost only. A repeat from another address gets `404`, and the service logs a warning with both addresses: the token reached someone else than the VM. An unknown token, an expired token, a token after its 60 s, and a missing header also give `404 {"error":"not found"}`. The token is not a tenant API key, and a tenant API key is not a token.

A restart of the service loses the payloads that no VM redeemed yet. Those VMs show `expired`.

Logs never contain tokens, payloads, or file contents.

## Deploy on the control host

The AWS account needs these resources before the first create. The service looks them up by name at each launch.

- An image named `AMI_NAME` that the account owns, with the tag `Project=jan3-dev-env`. Without the tag the service does not find the image, and the IAM policy would refuse it anyway.
- The security group `SECURITY_GROUP` and the key pair `KEY_NAME`.
- An IAM identity with the policy of [IAM](#iam).

The host runs Docker Compose with Caddy in front of the service. Caddy gets the TLS certificate. For tests, use the host name `<ip-with-dashes>.sslip.io`. Open the ports 80 and 443 in the firewall.

The `control` instance has 1.9 GiB of RAM, and the build of the image needs more than 8 GiB (see [Build memory](#build-memory)). So the image is built on another machine, and the host only loads it and runs it. Each image has a tag, for example the short commit. `compose.yaml` names the image `fabrium-control-plane:${CP_TAG:-latest}`.

1. On a build machine with at least 10 GiB of RAM, clone the repository, build the image, and save it. The host is x86_64, so build on an x86_64 machine.

   ```sh
   git clone https://github.com/jan3dev/fabrium-control-plane.git
   cd fabrium-control-plane
   TAG=$(git rev-parse --short HEAD)
   docker build -t fabrium-control-plane:$TAG .
   docker save fabrium-control-plane:$TAG | gzip > fabrium-control-plane-$TAG.tar.gz
   ```

2. Copy the file to the control host and load it there. A registry works too: pull the image on the host and give it the name `fabrium-control-plane:<tag>` with `docker tag`.

   ```sh
   docker load < fabrium-control-plane-<tag>.tar.gz
   ```

3. On the control host, clone the repository for the Compose files and the `Caddyfile`. Nothing is built there.

   ```sh
   git clone https://github.com/jan3dev/fabrium-control-plane.git
   cd fabrium-control-plane
   ```

4. Create `.env` and set every variable of [Configuration](#configuration) that has no default: `API_KEYS`, `API_SOURCES`, `PUBLIC_URL`, `CP_HOST`, and the AWS credentials. `PUBLIC_URL` is `https://` plus `CP_HOST`. Set `API_SOURCES` to the public IP of the zooid host of each tenant before you deploy. A tenant whose zooid host is not in the list gets `403` on every call.

   ```sh
   cp .env.example .env
   ```

5. Start the service and Caddy with the image that you loaded. `--no-build` keeps Compose from building on the host.

   ```sh
   CP_TAG=<tag> docker compose -f compose.yaml -f compose.caddy.yaml up -d --no-build
   ```

6. Check it.

   ```sh
   curl -i https://<host>/health
   ```

To update, build a new tag, load it on the host, and run step 5 with the new tag. If the Compose files changed, update the clone on the host first.

`compose.caddy.yaml` adds the `caddy` service on the ports 80 and 443, with the named volumes `caddy_data` and `caddy_config`. The `Caddyfile` sends every request to `control-plane:8080`. Caddy sees only `CP_HOST`, not the AWS keys.

Caddy must stay the only way in. The source check trusts the last `X-Forwarded-For` entry, and a client that reaches the app port directly can send any address. `compose.yaml` publishes the port on `127.0.0.1` only. Do not publish it on another interface, and do not open it in the firewall.

### Build memory

The AWS SDK crate `aws-sdk-ec2` is very large. Measured on a 5-core machine:

| Build | Time | Peak memory of the compiler |
|-------|------|-----------------------------|
| `cargo check` (no code generation) | 2 min | 5.6 GiB |
| Release, SDK crate at `opt-level = 0` | 4 min | 7.3 GiB |
| Release, default profile (what the Dockerfile builds) | 7.5 min | 8.3 GiB |

The compiler alone peaks at 8.3 GiB, so a machine with 8 GiB of RAM is not enough without swap. Use 10 GiB or more. On the `control` instance, `docker compose up --build` fails with an out-of-memory error, and a large swap file only makes the build very slow.

## IAM

`docs/iam-policy.json` is the policy of the AWS identity that the control plane uses. Replace `<ACCOUNT_ID>` and `<SECURITY_GROUP_ID>`, and validate it with IAM Access Analyzer before use.

- `Describe*` on every resource, because EC2 has no resource-level permissions for describe actions.
- `RunInstances` only with the request tag `fabrium:tenant`, IMDSv2 required, and an instance type of an allowed family. It accepts only images that the account owns with the tag `Project=jan3-dev-env`, one security group, and one key pair.
- `CreateTags` only during a launch, plus the single key `fabrium:setup` on instances that have `fabrium:tenant`.
- Start, stop, reboot, and terminate only on instances that have `fabrium:tenant`.
- No `iam:PassRole`, because VMs get no instance profile.

The code enforces tenant isolation. IAM keeps the control plane away from every instance that Fabrium did not launch or adopt.
