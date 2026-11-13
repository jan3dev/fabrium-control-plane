# GitHub App Setup for Fabrium Agent Factory

This guide walks through creating and configuring the GitHub App required by the agent factory dispatcher.

> [!WARNING]
> The manifest contains the placeholder `YOUR_CP_HOST`. Replace it with your real control plane host **before** you create the app. An app made from the unchanged manifest gets a broken webhook URL.

## Prerequisites

- Admin access to the GitHub organization or user account where the app will be installed
- The control plane host URL (e.g., `https://cp.fabrium.net`)

## Option 1: Create from Manifest (Recommended)

GitHub supports creating apps from a manifest file for quick setup.

### Steps

1. Copy the manifest and replace `YOUR_CP_HOST` with your actual control plane hostname:

```bash
cp docs/tech/agent-factory/github-app-manifest.json /tmp/github-app-manifest.json
# Edit the webhook URL in the copy
```

2. Host the edited copy at a temporary URL that GitHub can fetch (for example, a secret gist). Delete the host after the app is created.

3. Open the manifest flow with the URL-encoded address of your copy:

```
https://github.com/settings/apps/new?manifest=<URL-encoded-manifest-URL>
```

For an organization app, use `https://github.com/organizations/<org>/settings/apps/new?manifest=<URL-encoded-manifest-URL>`.

4. GitHub shows the app settings from the manifest. Check them and click **Create GitHub App**.

5. After creation, note the **App ID** and download the **Private Key** (PEM file).

6. Configure a **Webhook Secret** in the app settings.

## Option 2: Manual Creation

### Step 1: Create the GitHub App

1. Go to **Settings → Developer settings → GitHub Apps → New GitHub App**

2. Fill in the basic information:
   - **GitHub App name**: `Fabrium Factory` (or your preferred name)
   - **Description**: Agent factory dispatcher for issue-to-PR automation
   - **Homepage URL**: Your control plane URL

### Step 2: Configure Webhook

- **Webhook URL**: `https://<CP_HOST>/v1/github/webhook`
- **Webhook secret**: Generate a strong random secret (save this for later)
  
  ```bash
  openssl rand -hex 32
  ```
- **Active**: ✓ (checked)
- **SSL verification**: Enable (recommended)

### Step 3: Set Permissions

Under **Repository permissions**:

| Permission     | Access Level | Reason                                    |
|----------------|--------------|-------------------------------------------|
| **Issues**     | Read & Write | Add/remove labels, post comments          |
| **Checks**     | Read         | Monitor CI status on factory branches     |
| **Pull requests** | Read      | Track PR state and merge status           |
| **Metadata**   | Read         | Required for basic repository access      |

Leave all other permissions at "No access".

### Step 4: Subscribe to Events

Check these webhook events:

- [x] **Issues** — `labeled`, `opened`, `edited` trigger factory runs
- [x] **Issue comment** — Future: human commands in issues
- [x] **Check suite** — CI completion on `factory/*` branches
- [x] **Pull request** — Merged/closed PR detection

Do not subscribe to **Pull request review** yet. The listener does not handle it. **Issue comment** stays subscribed for the planned human-command flow.

### Step 5: Installation Settings

- **Where can this GitHub App be installed?**: Only on this account (recommended for private use) or Any account
- Click **Create GitHub App**

### Step 6: Generate Private Key

After creating the app:

1. Scroll to **Private keys** section
2. Click **Generate a private key**
3. Save the downloaded `.pem` file securely

## Installation

After creating the app, install it on repositories that should use the factory:

1. Go to the GitHub App settings
2. Click **Install App** in the sidebar
3. Select the account/organization
4. Choose **Only select repositories** and pick the sandbox/factory repos
5. Click **Install**

## Control Plane Configuration

Add these environment variables to your control plane deployment:

```bash
# GitHub App Configuration
GITHUB_APP_ID=<your-app-id>
GITHUB_APP_PRIVATE_KEY_FILE=/path/to/app.pem
# Or inline (replace newlines with \n):
# GITHUB_APP_PRIVATE_KEY=-----BEGIN RSA PRIVATE KEY-----\n...\n-----END RSA PRIVATE KEY-----
GITHUB_WEBHOOK_SECRET=<the-webhook-secret-you-generated>

# Allowlist of repositories (comma-separated owner/name)
FACTORY_REPOS=acme/sandbox,acme/other-repo
```

## Verification

### Test Webhook Delivery

1. Go to your GitHub App settings → Advanced → Recent Deliveries
2. Trigger a test event (e.g., add a label to an issue in an installed repo)
3. Check for successful delivery (2xx response)

### Test Signature Verification

The control plane validates `X-Hub-Signature-256` using HMAC-SHA256:

```bash
# Send a test ping
curl -X POST https://<CP_HOST>/v1/github/webhook \
  -H "Content-Type: application/json" \
  -H "X-GitHub-Event: ping" \
  -H "X-GitHub-Delivery: test-$(date +%s)" \
  -H "X-Hub-Signature-256: sha256=$(echo -n '{"zen":"test"}' | openssl dgst -sha256 -hmac 'YOUR_WEBHOOK_SECRET' | cut -d' ' -f2)" \
  -d '{"zen":"test"}'
```

Expected response: `{"ok":true}` or `{"ok":true,"ignored":"event"}` for ping.

### Verify App Authentication

Check the control plane logs for successful installation token minting when processing events from an installed repository.

## Security Notes

1. **Private key**: Store securely, never commit to git. Use secrets management (Vault, AWS Secrets Manager, etc.) in production.

2. **Webhook secret**: Use a cryptographically random value of at least 32 bytes.

3. **Repository allowlist**: `FACTORY_REPOS` restricts which repositories the factory processes. Events from other repos are ignored with `{"ok":true,"ignored":"repo"}`.

4. **SSL verification**: Keep enabled in production. Only disable for local testing.

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| 403 Forbidden | Invalid or missing signature | Check `GITHUB_WEBHOOK_SECRET` matches the app config |
| 404 Not Found | Factory not configured | Set all required `GITHUB_*` and `MATRIX_*` env vars |
| `{"ok":true,"ignored":"repo"}` | Repo not in allowlist | Add repo to `FACTORY_REPOS` |
| JWT errors in logs | Invalid private key | Re-download PEM from GitHub App settings |
| Installation token fails | App not installed on repo | Install the app on the target repository |

## Event Flow Reference

```
GitHub Issue labeled "factory:ready"
         │
         ▼
POST /v1/github/webhook
         │
         ├── Verify X-Hub-Signature-256
         ├── Dedupe X-GitHub-Delivery
         ├── Check repo in FACTORY_REPOS
         │
         ▼
FactoryDispatcher → factory_runs row → Matrix room → wake implementer
```

See [listener.md](./listener.md) for the full state machine documentation. That file lands with the factory listener PR.
