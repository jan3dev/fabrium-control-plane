//! Minimal Matrix client for the factory bot on the Fabrium homeserver.

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};

/// Dispatch runs one event at a time, so a hung request would stall every run.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone)]
pub struct MatrixClient {
    http: Client,
    homeserver: String,
    access_token: String,
    pub implementer: String,
    pub reviewer: String,
}

/// Body of `createRoom` and of an alias lookup.
#[derive(Debug, Deserialize)]
struct CreateRoomResponse {
    room_id: String,
}

#[derive(Debug, Deserialize)]
struct MatrixError {
    errcode: String,
}

#[derive(Debug, Deserialize)]
struct Whoami {
    user_id: String,
}

impl MatrixClient {
    pub fn new(
        homeserver: impl Into<String>,
        access_token: impl Into<String>,
        implementer: impl Into<String>,
        reviewer: impl Into<String>,
    ) -> Self {
        Self {
            http: Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("build Matrix HTTP client"),
            homeserver: homeserver.into().trim_end_matches('/').to_owned(),
            access_token: access_token.into(),
            implementer: implementer.into(),
            reviewer: reviewer.into(),
        }
    }

    pub fn with_base(
        homeserver: impl Into<String>,
        access_token: impl Into<String>,
        implementer: impl Into<String>,
        reviewer: impl Into<String>,
    ) -> Self {
        Self::new(homeserver, access_token, implementer, reviewer)
    }

    /// Create a private room named `#alias_localpart`, invite `user_id`, return the room id.
    /// If the alias exists (an earlier attempt created the room), return that room.
    pub async fn create_task_room(
        &self,
        alias_localpart: &str,
        name: &str,
        invite: &str,
    ) -> Result<String> {
        let body = json!({
            "name": name,
            "preset": "private_chat",
            "room_alias_name": alias_localpart,
            "invite": [invite],
            "is_direct": false,
        });
        let (status, bytes) = self
            .send(
                reqwest::Method::POST,
                "/_matrix/client/v3/createRoom",
                Some(&body),
            )
            .await
            .context("create Matrix room")?;
        if status.is_success() {
            let res: CreateRoomResponse =
                serde_json::from_slice(&bytes).context("decode Matrix JSON")?;
            return Ok(res.room_id);
        }
        let err: Option<MatrixError> = serde_json::from_slice(&bytes).ok();
        if err.is_some_and(|e| e.errcode == "M_ROOM_IN_USE") {
            return self
                .resolve_alias(alias_localpart)
                .await
                .with_context(|| format!("reuse room #{alias_localpart}"));
        }
        bail!("Matrix {status}: {}", String::from_utf8_lossy(&bytes))
    }

    async fn resolve_alias(&self, alias_localpart: &str) -> Result<String> {
        let me: Whoami = self
            .request(
                reqwest::Method::GET,
                "/_matrix/client/v3/account/whoami",
                None,
            )
            .await?;
        let (_, server) = me
            .user_id
            .split_once(':')
            .context("bot user id has no server name")?;
        let path = format!(
            "/_matrix/client/v3/directory/room/{}",
            urlencoding_path(&format!("#{alias_localpart}:{server}"))
        );
        let res: CreateRoomResponse = self.request(reqwest::Method::GET, &path, None).await?;
        Ok(res.room_id)
    }

    pub async fn invite(&self, room_id: &str, user_id: &str) -> Result<()> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/invite",
            urlencoding_path(room_id)
        );
        let _: Value = self
            .post(&path, &json!({ "user_id": user_id }))
            .await
            .with_context(|| format!("invite {user_id} to {room_id}"))?;
        Ok(())
    }

    pub async fn send_notice(&self, room_id: &str, body: &str) -> Result<()> {
        let txn = format!(
            "cp-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let path = format!(
            "/_matrix/client/v3/rooms/{}/send/m.room.message/{txn}",
            urlencoding_path(room_id)
        );
        let content = json!({
            "msgtype": "m.notice",
            "body": body,
            "format": "org.matrix.custom.html",
            "formatted_body": markdownish_to_html(body),
        });
        let _: Value = self
            .put(&path, &content)
            .await
            .with_context(|| format!("send notice to {room_id}"))?;
        Ok(())
    }

    /// Mention `user_id` so zooid starts a session in that room.
    pub async fn wake(&self, room_id: &str, user_id: &str, prompt: &str) -> Result<()> {
        let mention = format!("{user_id} {prompt}");
        let txn = format!(
            "wake-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let path = format!(
            "/_matrix/client/v3/rooms/{}/send/m.room.message/{txn}",
            urlencoding_path(room_id)
        );
        let html = format!(
            "<a href=\"https://matrix.to/#/{}\">{}</a> {}",
            user_id,
            user_id,
            html_escape(prompt)
        );
        let content = json!({
            "msgtype": "m.text",
            "body": mention,
            "format": "org.matrix.custom.html",
            "formatted_body": html,
            "m.mentions": { "user_ids": [user_id] },
        });
        let _: Value = self
            .put(&path, &content)
            .await
            .with_context(|| format!("wake {user_id} in {room_id}"))?;
        Ok(())
    }

    pub async fn archive(&self, room_id: &str) -> Result<()> {
        // Soft archive: post a notice. Full tombstone needs power levels; keep it simple for v1.
        self.send_notice(room_id, "factory: run complete; archiving this room.")
            .await
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        self.request(reqwest::Method::POST, path, Some(body)).await
    }

    async fn put<T: serde::de::DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        self.request(reqwest::Method::PUT, path, Some(body)).await
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<T> {
        let (status, bytes) = self.send(method, path, body).await?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            bail!("Matrix {status}: {text}");
        }
        serde_json::from_slice(&bytes).context("decode Matrix JSON")
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(reqwest::StatusCode, Vec<u8>)> {
        let url = format!("{}{path}", self.homeserver);
        let mut req = self
            .http
            .request(method, &url)
            .bearer_auth(&self.access_token);
        if let Some(body) = body {
            req = req.json(body);
        }
        let res = req.send().await.context("Matrix HTTP request failed")?;
        let status = res.status();
        let bytes = res.bytes().await.context("read Matrix response")?;
        Ok((status, bytes.to_vec()))
    }
}

fn urlencoding_path(id: &str) -> String {
    // Room ids are `!opaque:server` and aliases `#local:server`.
    id.replace('!', "%21")
        .replace('#', "%23")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn markdownish_to_html(s: &str) -> String {
    html_escape(s).replace('\n', "<br/>")
}
