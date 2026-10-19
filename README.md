# Control Plane Backend for Fabrium

The control plane API for Fabrium. See the [system design](https://github.com/jan3dev/fabrium) for the full picture.

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

4. Put `zooid:<hash>` in `API_KEYS` in `.env`. Use only the 64-character hash, not the key.
5. Start the service.

   ```sh
   docker compose up --build
   ```

6. Call the API from another terminal.

   ```sh
   curl -i localhost:8080/health
   # 200 {"status":"ok"}

   curl -i -H "Authorization: Bearer $KEY" localhost:8080/v1/ping
   # 200 {"pong":true,"client":"zooid"}
   ```

The database lives in the `data` Docker volume. It survives restarts.

## Configuration

| Variable | Default | Docker value | Notes |
|----------|---------|--------------|-------|
| `API_KEYS` | none (required) | from `.env` | Comma-separated `<name>:<sha256-hex>` pairs. The service refuses to start if it is missing, empty, or malformed. |
| `BIND_ADDR` | `127.0.0.1:8080` | `0.0.0.0:8080` | Set by the image. |
| `DATABASE_URL` | `sqlite://data/control-plane.db` | `sqlite:///data/control-plane.db` | Set by the image. The service creates the file and its directory. |
| `RUST_LOG` | `info` | from `.env` | Log filter. |

A `.env` file in the working directory is loaded automatically for local runs.

## Local development without Docker

You need Rust 1.99 or newer.

```sh
cargo run
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

`cargo run` needs `API_KEYS` in your environment or in `.env`.

## Migrations

The service runs pending migrations at startup. Use the CLI to run them by hand.

```sh
cargo run -p migration -- up
cargo run -p migration -- down
cargo run -p migration -- status
```

The CLI reads `DATABASE_URL`. There are no migrations yet. Generate entities later with `sea-orm-cli`.

## API

| Method | Path | Auth | Response |
|--------|------|------|----------|
| GET | `/health` | none | `200 {"status":"ok"}`, or `503 {"status":"unavailable"}` if the database does not answer |
| GET | `/v1/ping` | Bearer | `200 {"pong":true,"client":"<name>"}` |

Send the key in the `Authorization: Bearer <key>` header. The server hashes the key with SHA-256 and compares it with the stored hashes.

A missing header, a wrong scheme, or an unknown key returns `401` with `WWW-Authenticate: Bearer` and a JSON body such as `{"error":"missing or invalid API key"}`.
