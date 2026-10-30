//! Thin GitHub REST client used by the App and by factory actions.

use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Clone)]
pub struct GithubApi {
    http: Client,
    base: String,
}

impl GithubApi {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
            base: "https://api.github.com".into(),
        }
    }

    /// For tests against a stub server.
    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
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
        let _: Value = self
            .request(method, path, Some(("Bearer", token)), body)
            .await
            .or_else(|e| {
                // 204 No Content has an empty body.
                if e.to_string().contains("status 204") {
                    Ok(Value::Null)
                } else {
                    Err(e)
                }
            })?;
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
        if status == StatusCode::NO_CONTENT {
            bail!("status 204");
        }
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            bail!("GitHub {status}: {text}");
        }
        if bytes.is_empty() {
            // Some endpoints return 200 with an empty body; callers that need JSON use typed T.
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
