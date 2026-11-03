//! Thin GitHub REST client used by the App and by factory actions.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Clone)]
pub struct GithubApi {
    http: Client,
    base: String,
}

/// Dispatch runs one event at a time, so a hung request would stall every run.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A failed GitHub call. Callers match on `status` instead of parsing the message.
#[derive(Debug, thiserror::Error)]
#[error("GitHub {status}: {text}")]
pub struct GithubHttpStatus {
    pub status: StatusCode,
    pub text: String,
}

impl GithubApi {
    pub fn new() -> Self {
        Self::with_base("https://api.github.com")
    }

    /// For tests against a stub server.
    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            http: Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("build GitHub HTTP client"),
            base: base.into(),
        }
    }

    pub async fn get_json_as_app<T: DeserializeOwned>(&self, path: &str, jwt: &str) -> Result<T> {
        self.request(Method::GET, path, Some(("Bearer", jwt)), None)
            .await
    }

    pub async fn post_json_as_app<T: DeserializeOwned>(
        &self,
        path: &str,
        jwt: &str,
        body: &Value,
    ) -> Result<T> {
        self.request(Method::POST, path, Some(("Bearer", jwt)), Some(body))
            .await
    }

    pub async fn request_as_install<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<&Value>,
    ) -> Result<T> {
        self.request(method, path, Some(("Bearer", token)), body)
            .await
    }

    pub async fn request_empty_as_install(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<&Value>,
    ) -> Result<()> {
        // 204 No Content and other empty bodies decode as `null`.
        let _: Value = self
            .request(method, path, Some(("Bearer", token)), body)
            .await?;
        Ok(())
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        auth: Option<(&str, &str)>,
        body: Option<&Value>,
    ) -> Result<T> {
        let url = format!("{}{path}", self.base.trim_end_matches('/'));
        let mut req = self
            .http
            .request(method, &url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "fabrium-control-plane")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some((scheme, token)) = auth {
            req = req.header("Authorization", format!("{scheme} {token}"));
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let res = req.send().await.context("GitHub HTTP request failed")?;
        let status = res.status();
        let bytes = res.bytes().await.context("read GitHub response")?;
        if !status.is_success() {
            return Err(anyhow::Error::new(GithubHttpStatus {
                status,
                text: String::from_utf8_lossy(&bytes).into(),
            }));
        }
        if bytes.is_empty() {
            // 204s and some 200s have an empty body; callers that need JSON use typed T.
            serde_json::from_value(Value::Null).context("empty GitHub body")
        } else {
            serde_json::from_slice(&bytes).context("decode GitHub JSON")
        }
    }
}

impl Default for GithubApi {
    fn default() -> Self {
        Self::new()
    }
}
