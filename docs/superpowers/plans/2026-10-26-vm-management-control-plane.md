# VM management, control plane track: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The control plane creates, lists, starts, stops, reboots, and deletes the VMs of one tenant on EC2, and hands each new VM its bootstrap payload exactly once.

**Architecture:** An `Ec2` trait hides the AWS SDK. `AwsEc2` implements it for production and `FakeEc2` implements it in memory for tests. The rules (tenant isolation, state table, name check, validation) live in the HTTP handlers, never in the adapter, so a wrong adapter filter cannot leak a VM. An in-memory `bootstrap::Store` holds the payloads, keyed by the SHA-256 of a single-use token.

**Tech Stack:** Rust 1.99 (edition 2024), axum 0.8, tokio, `aws-config` and `aws-sdk-ec2` (rustls with `ring`), `base64`, `getrandom`, `async-trait`. Tests use `tower::ServiceExt::oneshot` against the real router with `FakeEc2`.

**Spec:** [VM management design](../specs/2026-10-26-vm-management-design.md) (sections "Control plane", "Errors", "Security notes", "Testing", Appendix A and B). Shared contracts, which win over the spec: [contracts](2026-10-26-vm-management-contracts.md) (sections 1, 3, 4, 5). Work happens in `fabrium-control-plane`, worktree `fabrium-control-plane-worktrees/vm-api`, branch `feat/vm-api`.

## Global Constraints

- Every route except `/health` and `/v1/bootstrap` needs `Authorization: Bearer <tenant API key>`. The key name is the tenant. A tenant name matches `^[a-z0-9-]+$`, or the service does not start.
- Error bodies are `{"error":"<short message>"}`. A `413` from the body limit can be plain text. AWS errors give `502 {"error":"aws request failed"}`, and the detail goes to the log only.
- VM `name`: `^[a-z0-9][a-z0-9-]{0,62}$`. `{id}`: `^i-[0-9a-f]{8,17}$`. File `path`: starts with `/`, no `..` segment. `mode`: `^[0-7]{3,4}$`. `owner`: `^[a-z_][a-z0-9_-]{0,31}$`. `script`: not empty.
- Create body limit: 256 KiB (`DefaultBodyLimit` on that route only). Bootstrap entry TTL: 15 minutes. Token: 32 random bytes, base64url, no padding. The store keeps only the SHA-256 of the token.
- Instance type families: `t3`, `t3a`, `c6a`, `m6a`, `r6a`. `CpuOptions.AmdSevSnp=enabled` only for `c6a`, `m6a`, `r6a`.
- Defaults: `AWS_REGION=us-east-2`, `AMI_NAME=ubuntu-24.04-jan3-dev-env-v3`, `INSTANCE_TYPES=t3a.medium,t3a.large,t3.medium,t3.large,m6a.large,c6a.large,r6a.large`, `DEFAULT_INSTANCE_TYPE=t3a.medium`, `SECURITY_GROUP=jan3-dev-env-ssh`, `KEY_NAME=fabrium-vms`.
- Launch: newest image owned by `self` with the name `AMI_NAME`. Root device name from the image. 20 GiB gp3, encrypted. IMDSv2 required. No instance profile. Tags on the instance and the volume: `Name`, `Project=jan3-dev-env`, `fabrium:tenant`. Tag `fabrium:setup=delivered` on redeem.
- Keep the 10 s request timeout (`503`). Logs never contain tokens, payloads, or file contents. `Payload`, `File`, and `Launch` do not derive `Debug`, so nobody can log them by accident.
- No private data in tracked files (account IDs, ARNs, emails, resource IDs, tokens, keys). Use placeholders.
- Conventional Commits, one work-unit commit per task, no trailers, no push. About 400 authored changed lines per task is a planning heuristic only.
- Checks, from the worktree root, with `PATH=$HOME/.cargo/bin:$PATH`: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.

## Decisions the spec leaves open

| Topic | Decision | Why |
|---|---|---|
| TLS for the SDK | SDK default features off. HTTP client from `aws-smithy-http-client` with `rustls-ring`, set on the config loader. | The tree already builds `rustls` with `ring`. The SDK default adds `aws-lc-sys` (a C build) and a second TLS stack. The Dockerfile then needs no new tools. |
| State in the `202` of start, stop, reboot | The state the action causes: `pending`, `stopping`, `running`. | EC2 still reports the old state for a moment. |
| `POST /v1/bootstrap` with a missing or malformed header | `404`, like an unknown token. | One answer, nothing to learn. |
| Tagging at redeem | Bounded to 5 s. A timeout counts as a failure: log it, still return the payload. | The request timeout is 10 s. A cancelled handler after the entry is gone would lose the payload. |
| Extra startup checks | Each type is `<family>.<size>`. No duplicates. An empty variable counts as unset. | Fail early on typos. |
| Image lookup | Filter `state=available`, take the newest `CreationDate`. | A failed image cannot launch. |
| Unknown instance id at EC2 | `InvalidInstanceID.NotFound` and `.Malformed` mean "no such VM" (`404`). | EC2 forgets terminated instances after about an hour. |
| `sev_snp` of a VM | From the type family, the rule of `GET /v1/instance-types`. | One rule. The code launches with SEV-SNP for exactly those families. |
| Name of a deleted VM | It stays in use while the VM is `shutting-down`, usually about a minute. A create with that name gives `409` until then. | The spec rule is "not terminated". The web track should expect it. |

## Review Focus

Inputs and conditions the spec implies and a user will meet. Each line has a test in the task that owns the code (marked RF1 to RF5).

1. **A name that another tenant, or a terminated VM, already used.** It must not give `409`. The check is per tenant and ignores terminated VMs. Task 6.
2. **A tenant API key sent as a bootstrap token, and a bootstrap token sent to a tenant route.** The first gives `404`, the second `401`. Task 7.
3. **A payload with awkward content:** quotes, newlines, non-ASCII, an empty `content`, no files. It must come back byte for byte at redeem. Task 7.
4. **An instance without the `fabrium:tenant` tag (a legacy VM), or a terminated one, asked for by id.** `404` for every tenant, even when the port returns it. Tasks 4 and 5.
5. **Expired AWS credentials, and a failed tag at redeem.** Every AWS failure gives the fixed `502` body with no AWS text. A failed tag still delivers the payload. No log line holds the token or the payload. Tasks 4, 6, and 7.

**Known ceilings** (each gets a `ponytail:` comment where it lives): one global create lock; in-memory bootstrap store (a restart makes pending VMs `expired`); the name check relies on EC2 read-after-write, so a describe right after `RunInstances` can miss the new VM; a failed redeem tag leaves a delivered VM showing `expired`; the release build of the SDK crate needs about 8.3 GiB of RAM, so the image cannot build on the 1.9 GiB `control` instance.

---

## File structure

| File | Responsibility |
|---|---|
| `src/config.rs` (edit) | `VmConfig`, its validation, `Config.vm` |
| `src/auth.rs` (edit) | Tenant name check in `ApiKeys::parse`. `bearer_token` becomes `pub(crate)` |
| `src/error.rs` (edit) | `BadRequest`, `NotFound`, `Conflict`, `Aws` |
| `src/ec2/mod.rs` | Instance type rules, port types, the `Ec2` trait |
| `src/ec2/aws.rs` | `AwsEc2` and its pure helpers |
| `src/ec2/fake.rs` | `FakeEc2`, public so integration tests can use it |
| `src/bootstrap.rs` | `Payload`, `File`, `Store`, `new_token`, `stub` |
| `src/user-data.sh` | Appendix B stub with the two placeholders |
| `src/http/vms.rs` | Instance types, list, create, start, stop, reboot, delete |
| `src/http/redeem.rs` | `POST /v1/bootstrap` |
| `src/http/mod.rs`, `src/lib.rs`, `src/main.rs` (edit) | Routes, `AppState`, wiring |
| `tests/common/mod.rs` | Harness (router, fake, state), request helpers, `create_body`, `token_in` |
| `tests/vms.rs`, `tests/logs.rs`, `tests/http.rs` (edit) | API tests with the fake; the log test (own binary); the state helper of the old tests |
| `Cargo.toml`, `Cargo.lock` (edit) | Dependencies |
| `compose.caddy.yaml`, `Caddyfile`, `.env.example`, `docs/iam-policy.json`, `README.md` | Task 8 |

---

## Task 1: Settings, tenant names, instance type rules

**Files:**
- Modify: `src/config.rs`, `src/auth.rs`, `src/lib.rs` (`pub mod ec2;`)
- Create: `src/ec2/mod.rs` (rules only. Task 2 extends it.)

**Interfaces:**
- Produces:
  - `ec2::FAMILIES: [&str; 5]`; `ec2::family(&str) -> Option<&str>` (text before the first `.`; `None` without a `.` or with an empty side); `ec2::sev_snp(&str) -> bool`.
  - `config::VmConfig { public_url, aws_region, ami_name: String, instance_types: Vec<String>, default_instance_type, security_group, key_name: String }` and `Config.vm: VmConfig`.
  - `auth::ParseError::BadName(String)`.

- [ ] **Step 1: Write the failing tests**
  - `ec2`: `family_is_the_text_before_the_first_dot` (`t3a.medium` gives `t3a`; `t3`, `t3a.`, `.large` give `None`). `sev_snp_is_true_only_for_c6a_m6a_r6a` (true: `c6a.large`, `m6a.large`, `r6a.xlarge`; false: `t3.medium`, `t3a.large`, `m6i.large`).
  - `auth`: `tenant_names_use_lowercase_letters_digits_and_hyphens` (accepts `prod-sim`, `a1`; rejects `Stage`, `prod_sim`, `a.b` with `BadName`).
  - `config` (the `load` helper adds `API_KEYS` and `PUBLIC_URL=https://cp.example.org` unless the case overrides them): `public_url_is_required`; `public_url_must_be_https_without_quotes_or_whitespace` (`http://x`, `https://`, `ftp://x`, `https://a'b`, `https://a b` fail); `public_url_loses_trailing_slashes` (`https://cp.example.org//` gives `https://cp.example.org`); `vm_defaults_apply` (all seven defaults of Global Constraints); `vm_overrides_apply` (`INSTANCE_TYPES=" t3.large , c6a.large "` gives `["t3.large","c6a.large"]`); `empty_values_use_defaults` (`AWS_REGION=""` and `BIND_ADDR=""` give the defaults); `instance_type_families_are_checked` (`t3a.medium,m5.large`, `t3`, `T3A.medium`, `""` fail); `duplicate_instance_types_fail`; `default_instance_type_must_be_in_the_list` (`DEFAULT_INSTANCE_TYPE=t3a.large` with `INSTANCE_TYPES=t3a.medium` fails).
- [ ] **Step 2:** Run `cargo test`. Expect compile errors, then failures.
- [ ] **Step 3: Implement.** `VmConfig::from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self>`, called from `Config::from_lookup`. The `var` helper treats an empty value as unset. `ApiKeys::parse` rejects a name outside `[a-z0-9-]` (byte check, no regex crate). Error messages name the variable.
- [ ] **Step 4:** Run the three checks. Expect all green. The README statement about `API_KEYS` changes in Task 8.
- [ ] **Step 5: Commit** `feat: validate VM settings and tenant names`

## Task 2: EC2 port, AWS adapter, fake

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock`, `src/ec2/mod.rs`
- Create: `src/ec2/aws.rs`, `src/ec2/fake.rs`

Dependencies, with the SDK defaults off (see Decisions):

```toml
async-trait = "0.1.92"
aws-config = { version = "1.13.0", default-features = false, features = ["behavior-version-latest", "rt-tokio"] }
aws-sdk-ec2 = { version = "1.271.0", default-features = false, features = ["behavior-version-latest", "rt-tokio"] }
aws-smithy-http-client = { version = "1.5.1", default-features = false, features = ["rustls-ring"] }
```

**Interfaces:**
- Consumes: `VmConfig`, `ec2::{family, sev_snp}` (Task 1).
- Produces, in `ec2/mod.rs`:

```rust
#[derive(Clone, Debug)]
pub struct Instance { pub id: String, pub name: String, pub tenant: Option<String>, pub state: String,
    pub instance_type: String, pub public_ip: Option<String>, pub launched_at: DateTime, pub setup_delivered: bool }
pub struct TypeSpec { pub name: String, pub vcpus: i32, pub memory_mib: i64 }
#[derive(Clone)]                       // no Debug: user_data holds the token
pub struct Launch { pub name: String, pub tenant: String, pub instance_type: String, pub user_data: String /* base64 */ }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Start, Stop, Reboot, Terminate }
impl Action {
    pub fn allowed_from(self) -> Option<&'static str>; // Start: "stopped"; Stop, Reboot: "running"; Terminate: None
    pub fn next_state(self) -> &'static str;           // "pending", "stopping", "running", "shutting-down"
}
#[async_trait]
pub trait Ec2: Send + Sync {
    async fn instance_types(&self, names: &[String]) -> anyhow::Result<Vec<TypeSpec>>;
    async fn list(&self, tenant: &str) -> anyhow::Result<Vec<Instance>>;   // not terminated; callers re-check the tag
    async fn get(&self, id: &str) -> anyhow::Result<Option<Instance>>;     // any state; None if EC2 has no such id
    async fn launch(&self, l: &Launch) -> anyhow::Result<Instance>;        // state "pending", name and tenant from `l`
    async fn act(&self, id: &str, action: Action) -> anyhow::Result<()>;
    async fn mark_setup_delivered(&self, id: &str) -> anyhow::Result<()>;
}
```

  `DateTime` is `aws_sdk_ec2::primitives::DateTime`.
- Produces: `AwsEc2::connect(vm: &VmConfig) -> AwsEc2` (async). `FakeEc2` (`Default`): `seed(Instance)`, `instance(&str) -> Option<Instance>`, `launches() -> Vec<Launch>`, `actions() -> Vec<(String, Action)>`, `type_calls() -> usize`, and the switches `fail(bool)` (every call errors), `fail_writes(bool)` (only `launch` and `act` error), `fail_tagging(bool)` (only `mark_setup_delivered` errors), `hang_tagging(bool)` (`mark_setup_delivered` never finishes; added in Task 7). `fake::instance(id, name, tenant: Option<&str>, state, instance_type) -> Instance` builds a seed. The module keeps `LAUNCHED_AT_SECS`.

**Fake rules.** `list` and `get` return every seeded instance of every tenant and state (a worst-case port, so service tests prove the service filters). Both call `tokio::task::yield_now().await`, so concurrent requests interleave. `launch` gives ids `i-fa` plus 15 hex digits (17 in all, so they never clash with seeded ids), state `pending`, `launched_at` 2026-10-26T19:00:00Z (epoch 1793041200). `act` sets `action.next_state()`. `instance_types` gives 2 vCPUs and 8192 MiB for names ending `.large`, else 4096 MiB. No lock guard lives across an await.

**Dev dependencies.** `aws-smithy-http-client` with the feature `test-util` (the replay client) and `aws-smithy-types` (for `SdkBody`). Both are already in the dependency tree.

- [ ] **Step 1: Write the failing tests** (unit tests in the modules). The SDK client is built with `Client::from_conf` and the replay client, so it sends nothing. A helper `has(body, "key=value")` compares exact form parameters, because a substring test would accept `...tenantX`.
  - `aws.rs::run_instances_request_follows_the_launch_spec`: build with `m6a.large` and read `as_input()`. Assert image id, instance type, key name, one security group id, counts 1 and 1, `user_data` unchanged, one block device mapping (device name from the image, 20 GiB, `gp3`, encrypted), `HttpTokens=required` and `HttpEndpoint=enabled`, two tag specifications (instance, volume) each with `Name`, `Project=jan3-dev-env`, `fabrium:tenant`, no instance profile.
  - `aws.rs::only_amd_families_get_sev_snp`: `cpu_options` is `None` for `t3a.medium` and `t3.large`, `Some(AmdSevSnp=enabled)` for `c6a.large`, `m6a.large`, `r6a.large`.
  - `aws.rs::newest_image_has_the_latest_creation_date`: three images, the middle one is the newest; an empty list gives `None`.
  - `aws.rs::instance_conversion_reads_tags_state_ip_and_launch_time`: tags `Name`, `fabrium:tenant`, `fabrium:setup=delivered` map to the fields; an instance without tags gives name `""`, tenant `None`, `setup_delivered` false; `launched_at` keeps whole seconds.
  - `aws.rs::unknown_instance_ids_are_not_errors`: `is_missing_id` is true for `InvalidInstanceID.NotFound` and `InvalidInstanceID.Malformed`, false for `UnauthorizedOperation` and `None`.
  - Glue tests through the real SDK pipeline, with canned XML replies (`replay(&[(status, xml)])` returns the `AwsEc2` and the recorded requests):
    - `list_asks_for_the_tenants_live_instances`: filters `tag:fabrium:tenant=<tenant>` and `instance-state-name` with the five listed states and no `terminated`; the reply maps to `Instance`s.
    - `get_returns_the_instance_none_for_a_missing_id_and_an_error_otherwise`: a found instance; `400 InvalidInstanceID.NotFound` gives `None`; `403 UnauthorizedOperation` gives an `Err` with the code.
    - `launch_resolves_the_image_and_group_then_runs_the_instance`: three requests in order (images with owner `self` and filters `name` and `state=available`; groups with filter `group-name`; `RunInstances` with the newest image, its root device, the group id, the key name, `CpuOptions.AmdSevSnp=enabled`); the result has the `name` and `tenant` of the `Launch`.
    - `launch_stops_before_run_instances_when_the_image_or_group_is_missing`: the error names the AMI name or the group name, and no `RunInstances` request goes out.
    - `actions_call_the_matching_api`, `mark_setup_delivered_sets_the_tag`, `instance_types_read_vcpus_and_memory`.
    - `sdk_errors_keep_the_aws_code_and_message_for_the_log`: the error text starts with the operation name and holds the AWS message.
  - `fake.rs`: `list_and_get_ignore_tenant_and_state`; `launch_makes_a_pending_instance_with_a_valid_id` (matches `^i-[0-9a-f]{17}$`); `actions_and_tagging_change_the_instance`; `failure_switches_make_calls_fail`; `instance_types_follow_the_size_suffix`. `ec2/mod.rs`: `actions_have_a_required_state_and_a_next_state`.
- [ ] **Step 2:** Run `cargo test`. Expect compile errors, then failures.
- [ ] **Step 3: Implement `AwsEc2`** in `ec2/aws.rs`. `connect` builds the client with the `ring` HTTP client and `Region::new(vm.aws_region)`, credentials from the default chain.
  - `list`: `describe_instances` with filters `tag:fabrium:tenant` and `instance-state-name` in `pending, running, stopping, stopped, shutting-down`; `into_paginator().items().send()`, collected.
  - `get`: `describe_instances().instance_ids(id)`. Map `is_missing_id(err.code())` to `Ok(None)`.
  - `instance_types`: `describe_instance_types().set_instance_types(...)`, vCPUs from `v_cpu_info.default_v_cpus`, memory from `memory_info.size_in_mib`.
  - `launch`: `describe_images` (owner `self`, filters `name` and `state=available`, newest by `creation_date` through `newest_image`), `describe_security_groups` (filter `group-name`, first id), then `run_instances(...)` (the pure builder in the tests). Convert `Instances[0]`, then set `name` and `tenant` from `l`.
  - `act`: `start_instances`, `stop_instances`, `reboot_instances`, `terminate_instances`. `mark_setup_delivered`: `create_tags` with `fabrium:setup=delivered`.
  - Every SDK error becomes `anyhow!("<operation>: {}", DisplayErrorContext(&e))`.
- [ ] **Step 4:** Run the three checks. Then run the dry runs of Task 9 for `t3a.medium` and `m6a.large` as an early signal. A failure changes the builder only.
- [ ] **Step 5: Commit** `feat: add the EC2 port, its AWS adapter, and an in-memory fake`

## Task 3: Bootstrap store and user data stub

**Files:**
- Modify: `Cargo.toml`, `Cargo.lock` (`base64 = "0.23.1"`, `getrandom = "0.4.3"`), `src/lib.rs` (`pub mod bootstrap;`)
- Create: `src/bootstrap.rs`, `src/user-data.sh` (Appendix B, verbatim, placeholders `<PUBLIC_URL>` and `<TOKEN>`)

**Interfaces:**
- Produces:

```rust
pub const TTL: Duration = Duration::from_secs(15 * 60);
#[derive(Clone, Deserialize, Serialize)] pub struct Payload { pub files: Vec<File>, pub script: String }
#[derive(Clone, Deserialize, Serialize)] pub struct File { pub path: String, pub mode: String, pub owner: String, pub content: String }
impl Payload { pub fn validate(&self) -> Result<(), &'static str> }   // the rules of Global Constraints
pub fn new_token() -> String;                                         // 32 bytes from getrandom, base64url, no padding
pub fn stub(public_url: &str, token: &str) -> String;                 // renders user-data.sh; replace `<TOKEN>` first, then `<PUBLIC_URL>`
pub struct Store;  // Mutex<HashMap<[u8; 32], Entry>>, never held across an await
impl Store {
    pub fn new(ttl: Duration) -> Self;
    pub fn insert(&self, token: &str, payload: Payload, instance: &str); // hashes the token; sweeps expired entries
    pub fn redeem(&self, token: &str) -> Option<(Payload, String)>;      // removes the entry; None if unknown, used, or expired
    pub fn has_live(&self, instance: &str) -> bool;
    pub fn remove_instance(&self, instance: &str);
}
```

- [ ] **Step 1: Write the failing tests** (unit tests in `bootstrap.rs`)
  - `token_is_32_random_bytes_in_base64url`: length 43; only `[A-Za-z0-9_-]`; decodes to 32 bytes; two tokens differ.
  - `redeem_is_single_use`: `redeem` returns the payload and the instance id once, then `None`.
  - `unknown_token_does_not_redeem`.
  - `expired_entry_is_not_live_and_does_not_redeem`: `Store::new(Duration::ZERO)`.
  - `has_live_follows_the_entry`: true after `insert`, false after `redeem`; true after `insert`, false after `remove_instance`, and `redeem` gives `None`.
  - `store_keeps_only_the_hash`: the keys are `Sha256(token)`; none equals the token bytes.
  - `payload_rules`: a valid payload passes; each of these fails: path `home/x`, `/a/../b`, `""`; mode `8`, `12`, `12345`, `rwx`; owner `Root`, `1abc`, 33 characters, `""`; empty `script`. `files: []` passes.
  - `stub_fills_both_placeholders`: starts with `#!/bin/bash`; contains `URL='https://cp.example.org/v1/bootstrap'`; the token occurs once, on the line that starts `TOKEN='`; no `<PUBLIC_URL>` or `<TOKEN>` is left.
  - `a_url_cannot_inject_a_second_token`: a URL that holds `<TOKEN>` stays literal, because the token is filled in first. `insert_drops_expired_entries`: with `Store::new(Duration::ZERO)` only the newest entry is left.
- [ ] **Step 2:** Run `cargo test`. Expect compile errors, then failures.
- [ ] **Step 3: Implement** the items above. Validation uses byte predicates, not the `regex` crate. `Payload` and `File` have no `Debug`. A `ponytail:` comment on `Store`: in memory; a restart loses unredeemed payloads (those VMs show `expired`); persist only if restarts during launches become frequent.
- [ ] **Step 4:** Run the three checks and `bash -n src/user-data.sh`.
- [ ] **Step 5: Commit** `feat: add the single-use bootstrap store and the user data stub`

## Task 4: AppState, errors, instance types, list

**Files:**
- Modify: `Cargo.toml` (tokio feature `sync`), `src/lib.rs`, `src/error.rs`, `src/http/mod.rs`, `src/main.rs`, `tests/http.rs`
- Create: `src/http/vms.rs`, `tests/vms.rs`

**Interfaces:**
- Consumes: `Ec2`, `Store`, `VmConfig`.
- Produces:
  - `AppState { db, api_keys, vm: Arc<VmConfig>, ec2: Arc<dyn Ec2>, bootstrap: Arc<Store>, types: Arc<tokio::sync::OnceCell<Vec<TypeSpec>>> }` and `AppState::new(db, api_keys: ApiKeys, vm: VmConfig, ec2: Arc<dyn Ec2>) -> Self` (store with `bootstrap::TTL`). The fields stay public so a test can swap `bootstrap`.
  - `AppError::{BadRequest(String) -> 400, NotFound -> 404 "not found", Conflict(String) -> 409, Aws(String) -> 502 "aws request failed"}`, all `{"error":...}`. `impl From<anyhow::Error> for AppError` builds `Aws(format!("{e:#}"))`. `into_response` logs the detail with `tracing::error!`. The body never holds it.
  - In `http/vms.rs`: `struct Vm` (the contract object, `launched_at` as RFC 3339 whole seconds, `public_ip` `null` when absent); `fn visible(&Instance, tenant: &str) -> bool` (tenant tag equals `tenant` and state is not `terminated`); `setup` is `delivered` from the tag, else `pending` if `store.has_live(id)`, else `expired`. (`find` comes in Task 5, where a test first needs it.)
  - Routes `GET /v1/instance-types` and `GET /v1/vms` behind `auth::require_key`. `main.rs` builds `AwsEc2::connect(&config.vm)` and logs the non-secret settings at `info`.

- [ ] **Step 1: Write the failing tests** (`tests/vms.rs`; two keys, `stage` and `prod-sim`; helper `call(app, method, uri, auth) -> (StatusCode, Value)`; update `state()` in `tests/http.rs` to `AppState::new`)
  - `instance_types_follow_config_order`: seven items in `INSTANCE_TYPES` order; `sev_snp` true only for `c6a.large`, `m6a.large`, `r6a.large`; `default` true only for `t3a.medium`; `t3a.medium` has 2 vCPUs and 4096 MiB, `t3a.large` has 8192 MiB.
  - `instance_types_are_cached_after_the_first_success`: `fail(true)` gives `502 {"error":"aws request failed"}`; after `fail(false)`, two calls give `200` and `type_calls() == 2`.
  - `vm_routes_need_a_tenant_key`: no header, wrong key, `Basic` scheme give `401` on both routes.
  - `list_shows_only_live_vms_of_the_caller_sorted_by_name` (RF4): seed stage `b` (running, with an IP), stage `a` (stopped), stage `gone` (terminated), prod-sim `x`, and an untagged instance. Stage sees `[a, b]` in that order. Check every field of `b`, `public_ip` is `null` for `a`, and `launched_at` is `2026-10-26T19:00:00Z`.
  - `setup_comes_from_the_tag_then_the_store_then_expiry`: one instance with the tag gives `delivered`; one with a live store entry gives `pending`; one with neither gives `expired`.
  - `sev_snp_follows_the_family`: seeded `m6a.large` gives `true`, `t3a.medium` gives `false`.
  - `list_is_502_when_aws_fails` (RF5): exact body `{"error":"aws request failed"}`.
- [ ] **Step 2:** Run `cargo test`. Expect compile errors, then failures.
- [ ] **Step 3: Implement** the items above. The type cache is `types.get_or_try_init(...)`, so a failure is not cached. The handlers read `Extension<Client>` as `ping` does.
- [ ] **Step 4:** Run the three checks.
- [ ] **Step 5: Commit** `feat: add instance types and VM list routes`

## Task 5: Start, stop, reboot, delete

**Files:**
- Modify: `src/http/vms.rs`, `src/http/mod.rs`, `tests/vms.rs`

**Interfaces:**
- Consumes: `visible`, `Vm`, `Action` (Tasks 2 and 4).
- Produces: `fn valid_id(&str) -> bool`; `async fn find(&AppState, tenant, id) -> Result<Instance, AppError>` (`valid_id`, then `ec2.get`, then `visible`, else `NotFound`); `pub fn action(Action) -> MethodRouter<AppState>` (one `POST` route builder for start, stop, and reboot); routes `POST /v1/vms/{id}/start`, `/stop`, `/reboot` (`202` with the VM) and `DELETE /v1/vms/{id}` (`202`, empty body).

- [ ] **Step 1: Write the failing tests**
  - `actions_follow_the_state_table`: for each of `start`, `stop`, `reboot` and each state in `pending, running, stopping, stopped, shutting-down`: the allowed pair gives `202` with the VM in the new state (`pending`, `stopping`, `running`) and `fake.actions()` holds it; every other pair gives `409 {"error":"vm is <state>"}` and no action.
  - `unknown_foreign_untagged_terminated_and_malformed_ids_are_404` (RF4): for all four verbs: another tenant's VM, an untagged VM, a terminated VM, a well-formed id that does not exist, and `i-xyz`, `i-123`, `i-` plus 18 hex digits, `i-ABCDEF12`, `abc`, `i-` give `404 {"error":"not found"}`. The malformed ids are seeded as VMs of the caller, so only the id rule can give the `404`. `fake.actions()` stays empty. The owner of the foreign VM can still stop it (a control that the `404` is not a general failure).
  - `delete_terminates_in_every_listed_state`: `pending, running, stopping, stopped, shutting-down` give `202` with an empty body, and `fake.actions()` holds `Terminate`.
  - `delete_drops_the_pending_payload`: after `insert` into the store, `delete` makes `has_live` false.
  - `a_failed_terminate_keeps_the_pending_payload`: with `fail_writes(true)`, `delete` gives `502` and `has_live` stays true.
  - `actions_are_502_when_aws_fails` (RF5): once with `fail(true)` (the lookup fails) and once with `fail_writes(true)` (the action fails): the fixed body, for stop, reboot, start, and delete, and no VM changes state.
- [ ] **Step 2:** Run `cargo test`. Expect failures (`404` or `405`).
- [ ] **Step 3: Implement** `act(state, tenant, id, action)`: `find`, then `allowed_from` check (`Conflict(format!("vm is {state}"))`), `ec2.act`, then set `state = action.next_state()` on the found instance. `delete` skips the state check and calls `store.remove_instance(id)` after a successful terminate. One `tracing::info!` per mutation with the tenant and the id (never payload data).
- [ ] **Step 4:** Run the three checks.
- [ ] **Step 5: Commit** `feat: add start, stop, reboot, and delete routes`

## Task 6: Create

**Files:**
- Modify: `src/http/vms.rs`, `src/http/mod.rs`, `src/lib.rs` (`create_lock: Arc<tokio::sync::Mutex<()>>` in `AppState`), `tests/vms.rs`

**Interfaces:**
- Consumes: `Payload::validate`, `new_token`, `stub`, `Store::insert`, `Ec2::{list, launch}`, `Launch`.
- Produces: `POST /v1/vms` with `DefaultBodyLimit::max(256 * 1024)` on that route only, merged with the `GET` handler. The handler takes `Bytes`, so the limit gives `413`. It parses with `serde_json::from_slice` and answers a fixed `BadRequest("invalid request body")` on any parse error (serde errors can echo values).

Flow: parse, validate (name, type in the list or the default, `Payload::validate`), take the lock, `list(tenant)` and check names among `visible` instances (`Conflict("name in use")`), `new_token`, `launch` with `STANDARD.encode(stub(public_url, token))`, `store.insert(token, payload, id)` (no await between launch and insert), answer `201` with the VM. Comment on the lock: `ponytail:` one global lock serializes every create, about 3 s each; use a lock per tenant and name if creates must run in parallel. Comment on the check: it relies on EC2 read-after-write, so a describe right after `RunInstances` can miss the new VM and two creates of one name a moment apart can both pass.

- [ ] **Step 1: Write the failing tests**
  - `create_launches_a_tagged_vm_with_the_defaults`: `201`; body has `name`, `state: "pending"`, `setup: "pending"`, `instance_type: "t3a.medium"`, `sev_snp: false`, `public_ip: null`. `fake.launches()` holds one launch with tenant `stage`, the name, and the type.
  - `create_uses_the_requested_type`: `m6a.large` gives `201` and `sev_snp: true`.
  - `create_rejects_invalid_input_with_400`: names `""`, `Lima`, `-lima`, `li_ma`, 64 characters; types `t3a.xlarge`, `T3A.MEDIUM`, `""`; a path `home/x`, `/a/../b`; modes `8`, `12345`; owner `Root`; empty `script`; a missing `script`; a missing `bootstrap`; `files` not an array; not JSON; an empty body. All give `400` with a string `error` and no launch. A 63-character name gives `201`. `"instance_type": null` gives the default.
  - `name_check_is_per_tenant_and_ignores_terminated_vms` (RF1): stage `lima` running, then `create lima` as stage gives `409 {"error":"name in use"}`; as `prod-sim` gives `201`; a terminated stage `old` does not block `create old`.
  - `concurrent_creates_of_one_name_launch_once`: two `create` calls joined with `tokio::join!` give one `201` and one `409`; `launches().len() == 1`.
  - `body_limit_is_256_kib`: a valid body of exactly 262144 bytes (pad `content`) gives `201`; one more byte gives `413`; the fake holds one launch.
  - `user_data_holds_the_stub_and_the_store_holds_the_payload`: decode `launches()[0].user_data` from base64; it starts with `#!/bin/bash` and holds the `URL='https://cp.example.org/v1/bootstrap'` line. Read the token from the `TOKEN='...'` line. `store.redeem(token)` returns the posted payload and the new instance id.
  - `create_accepts_the_edges_of_the_rules`: a 63-character name, `9`, `a-`, `0-0`; `instance_type: null`; `files: []`.
  - `create_without_a_key_is_401`.
  - `create_is_502_and_launches_nothing_when_aws_fails` (RF5): once with `fail(true)` (the lookup fails) and once with `fail_writes(true)` (the launch fails): the fixed body, no launch, and the same request works when AWS is back.
- [ ] **Step 2:** Run `cargo test`. Expect failures (`405` or `404`).
- [ ] **Step 3: Implement** the flow above.
- [ ] **Step 4:** Run the three checks.
- [ ] **Step 5: Commit** `feat: add VM create`

## Task 7: Redeem

**Files:**
- Modify: `Cargo.toml` (dev feature `test-util` of tokio), `src/auth.rs` (`bearer_token` to `pub(crate)`), `src/ec2/fake.rs` (`hang_tagging`), `src/http/mod.rs`, `tests/vms.rs`
- Create: `src/http/redeem.rs`, `tests/common/mod.rs` (the shared helpers, moved out of `tests/vms.rs`), `tests/logs.rs`

**Interfaces:**
- Consumes: `Store::redeem`, `Ec2::mark_setup_delivered`, `auth::bearer_token`.
- Produces: `POST /v1/bootstrap`, outside `require_key`. A missing or malformed header, an unknown token, a used token, and an expired token all give `404 {"error":"not found"}`. A valid token returns `200` with the payload JSON `{files, script}`.

Flow: `redeem` removes the entry first. Then `tokio::time::timeout(5 s, ec2.mark_setup_delivered(id))`. Failure or timeout logs `tracing::error!` with the instance id and the error text (never the token or the payload), and the handler still answers `200`. Comment: `ponytail:` a failed tag leaves the VM showing `expired`; retry the tag from `list` if that matters.

- [ ] **Step 1: Write the failing tests**
  - `redeem_returns_the_payload_once`: create, read the token from the user data, `POST /v1/bootstrap` with `Bearer <token>` gives `200` and the posted payload; the fake instance has `setup_delivered`; `GET /v1/vms` shows `delivered`; a second redeem gives `404 {"error":"not found"}`.
  - `redeem_rejects_missing_unknown_and_expired_tokens`: no header, `Basic x`, `Bearer nope`, and a fresh token against a store built with `Store::new(Duration::ZERO)` all give `404`.
  - `keys_and_tokens_do_not_cross` (RF2): the tenant key as a bootstrap token gives `404`; a bootstrap token on `GET /v1/vms` gives `401`; the token is still redeemable afterwards.
  - `payload_round_trips_awkward_content` (RF3): contents with `'`, `"`, `\`, `\n`, `\r\n`, a non-ASCII string with an emoji, an empty string, and a `files: []` payload come back equal after redeem.
  - `redeem_still_delivers_when_tagging_fails` (RF5): `fail_tagging(true)` gives `200` with the payload; a second redeem gives `404`; the instance stays untagged.
  - `redeem_still_delivers_when_tagging_hangs` (RF5): `hang_tagging(true)` and `tokio::time::pause()` (dev feature `test-util` of tokio). The redeem gives `200` with the payload. An outer `timeout` of 30 s makes a handler that waits for the tag fail the test instead of hanging it.
  - `delete_before_redeem_drops_the_token`: create, delete, redeem gives `404`.
  - `tests/logs.rs::logs_never_hold_a_token_a_key_or_the_payload` (RF5): install a capturing `tracing` subscriber (`tracing_subscriber::fmt` with a shared buffer writer, `set_default`, max level TRACE). Run create, a redeem with a failing tag, a failed create, and delete, with marker strings in a file `content` and in `script`. Assert that the log has the lines `vm launched`, `tagging`, `aws request failed`, and `vm deleted`, and holds neither the token, nor the tenant key, nor a marker. The test has its own test binary: a thread-scoped subscriber loses events of call sites that parallel tests of the same process hit first (seen as a test that failed in the full run and passed alone). The helpers move to `tests/common/mod.rs`.
- [ ] **Step 2:** Run `cargo test`. Expect failures (`404` for every case, including the happy path).
- [ ] **Step 3: Implement** `redeem` as above.
- [ ] **Step 4:** Run the three checks.
- [ ] **Step 5: Commit** `feat: add bootstrap redeem`

## Task 8: Deploy files and docs

A separate work unit. It becomes its own pull request.

**Files:**
- Create: `compose.caddy.yaml`, `Caddyfile`, `docs/iam-policy.json`
- Modify: `.env.example`, `README.md`. Modify `Dockerfile` only if the build below fails for a missing tool. (The build image is `rust:1.99.1-slim-bookworm`. The tree builds `ring` already, and the SDK uses `ring` too.)
- `.env.example` matches the permission rule `.env*` of some sessions, and then neither the read nor the write works. The README lists every variable, so it is complete without the file. Write the file by hand in that case.

**Interfaces:**
- `compose.caddy.yaml` is an override for `compose.yaml`. Service `caddy` (image `caddy:2`, `restart: unless-stopped`, ports `80:80` and `443:443`, `environment: CP_HOST: ${CP_HOST:?set CP_HOST in .env}`, volumes `./Caddyfile:/etc/caddy/Caddyfile:ro`, `caddy_data:/data`, `caddy_config:/config`, `depends_on: [control-plane]`). It does not read the whole `.env`, so the AWS keys stay out of the Caddy container.
- `Caddyfile`: `{$CP_HOST} { reverse_proxy control-plane:8080 }`.
- `docs/iam-policy.json`: Appendix A, verbatim, with `<ACCOUNT_ID>` and `<SECURITY_GROUP_ID>` kept.
- `.env.example`: every variable of the configuration table, no real value. Variables that have a default stay commented.
- `README.md` sections: configuration table (every variable, default, note; update the `API_KEYS` row with the tenant name rule), API summary (the eight routes, state table, error table, `413`), deploy on the control host (`git clone`, copy `.env.example` to `.env`, set `CP_HOST`, `PUBLIC_URL`, `API_KEYS`, and the AWS keys, then `docker compose -f compose.yaml -f compose.caddy.yaml up -d --build`; open ports 80 and 443; temporary AWS credentials expire, so replace them in `.env` and recreate the container), IAM (the policy file, the placeholders, validate with IAM Access Analyzer, the code enforces tenant isolation and IAM backs it up).

- [ ] **Step 1:** Write the files.
- [ ] **Step 2: Verify.** `jq . docs/iam-policy.json`. `docker compose -f compose.yaml -f compose.caddy.yaml config` with a dummy `CP_HOST` and a copy of `.env.example`. `docker run --rm` with the `Caddyfile` and `caddy validate`. To prove that the Dockerfile needs no change, run its build command in its base image: `docker run --rm -v <worktree>:/src:ro -v <scratch>:/work -e CARGO_HOME=/work/cargo -e CARGO_TARGET_DIR=/work/target rust:1.99.1-slim-bookworm cargo build --release --locked` (a `docker build` writes a BuildKit cache that other builds on the host share). Remove any container, image, or file that these commands leave.
- The release build needs about 8.3 GiB of RAM for one `rustc` (the crate `aws-sdk-ec2`). The README says so in "Build memory", with the measured numbers. A build on the `control` instance (1.9 GiB) cannot work.
- [ ] **Step 3: Commit** `chore: add the Caddy override, IAM policy, and deploy docs` (and `.env.example` when the session may write it)

## Task 9: AWS checks (no commit)

Use `aws --profile jan3 --region us-east-2` only. Read-only calls and `--dry-run` only. Never launch, start, stop, or terminate. Report outcomes only. Never paste an account ID, ARN, or resource ID into a tracked file or a report.

- [ ] **Step 1: Boot mode.** `ec2 describe-instance-types` for `t3.medium` and `t3a.medium` (also the other allowed types): `SupportedBootModes`, `NitroTpmSupport`, and `ProcessorInfo.SupportedFeatures`. `ec2 describe-images --owners self` for the newest image of that name: `BootMode`, `TpmSupport`, `RootDeviceName`. Expect the image boot mode in the supported list of every type.
- [ ] **Step 2: Dry runs.** Resolve the image id and the security group id the way `AwsEc2::launch` does. Render the stub with `sd` into the scratchpad (placeholder URL, dummy token). For `t3a.medium`, `t3.medium`, and for `m6a.large`, `c6a.large`, `r6a.large` with `--cpu-options AmdSevSnp=enabled`, run `ec2 run-instances --dry-run` with: `--key-name fabrium-vms`, `--security-group-ids`, `--metadata-options HttpTokens=required,HttpEndpoint=enabled`, `--block-device-mappings 'DeviceName=<root device of the image>,Ebs={VolumeSize=20,VolumeType=gp3,Encrypted=true}'`, `--tag-specifications` for `instance` and `volume` (`Name`, `Project=jan3-dev-env`, `fabrium:tenant`), `--user-data file://<stub>`, `--count 1` (the CLI form of `MinCount=1` and `MaxCount=1`). Expect `DryRunOperation` for each.
- [ ] **Step 2b: Negative controls**, so that the dry runs prove something: `t3a.medium` with `--cpu-options AmdSevSnp=enabled` must fail with `InvalidParameterCombination`, and a wrong `DeviceName` must fail with `InvalidBlockDeviceMapping`.
- [ ] **Step 3:** Compare the arguments with the assertions of `run_instances_request_follows_the_launch_spec`. A difference is a bug in the test or in the CLI call. Fix it and run again.

## Task 10: Local end-to-end check (no commit)

The unit tests prove each half. This check proves the contract between the service and the user data stub, which nothing else covers. It needs Docker and nothing from AWS. The scripts live in the scratchpad.

- [ ] **Step 1:** Run a fake EC2 (Python, standard library) that answers `DescribeInstances` (empty), `DescribeImages`, `DescribeSecurityGroups`, `RunInstances`, and `CreateTags` with canned XML and records each request body.
- [ ] **Step 2:** Run the real binary with `AWS_ENDPOINT_URL=http://127.0.0.1:<port>`, fake env credentials, `AWS_CONFIG_FILE=/dev/null`, `AWS_SHARED_CREDENTIALS_FILE=/dev/null`, and `AWS_EC2_METADATA_DISABLED=true`, so nothing can reach a real account. `POST /v1/vms` with files of awkward content (quotes, `$HOME`, backticks, CRLF, emoji, an empty file, no final newline) and modes `600`, `0640`, `644`.
- [ ] **Step 3:** Decode `UserData` from the recorded `RunInstances` body. Change only the URL scheme to `http`. Run it as root in a clean `ubuntu:24.04` container with `curl`, `jq`, and `sudo` installed, on the host network.
- [ ] **Step 4:** Expect: stub exit 0; every file has the expected mode, owner, size, and sha256; the new directories are `700`; the setup script ran as root and saw the files; the payload and the setup script are gone from `/run`; the recorded `CreateTags` sets `fabrium:setup=delivered` on the instance; a second redeem gives `404`; the service log holds no token, no content, and no key. Stop the processes and remove the images that the check pulled.
