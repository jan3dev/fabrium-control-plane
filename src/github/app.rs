//! GitHub App JWT and installation access tokens.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};

use super::client::GithubApi;

/// Installation tokens live one hour; refresh early to dodge clock skew.
const TOKEN_TTL_SECS: u64 = 50 * 60;

struct CachedToken {
    token: String,
    expires_at: u64,
}

#[derive(Clone)]
pub struct GithubApp {
    pub app_id: u64,
    encoding_key: EncodingKey,
    api: GithubApi,
    installations: Arc<Mutex<HashMap<String, u64>>>,
    tokens: Arc<Mutex<HashMap<String, CachedToken>>>,
}

#[derive(Debug, Serialize)]
struct Claims {
    iat: u64,
    exp: u64,
    iss: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: String,
    expires_at: String,
}

#[derive(Debug, Deserialize)]
struct InstallationRepos {
    id: u64,
}

impl GithubApp {
    pub fn new(app_id: u64, private_key_pem: &str, api: GithubApi) -> Result<Self> {
        let encoding_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .context("GITHUB_APP_PRIVATE_KEY is not a valid RSA PEM")?;
        Ok(Self {
            app_id,
            encoding_key,
            api,
            installations: Arc::new(Mutex::new(HashMap::new())),
            tokens: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// A JWT that lasts under ten minutes (GitHub's limit).
    pub fn app_jwt(&self) -> Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_secs();
        // Skew the issued-at slightly so a fast clock does not reject the token.
        let iat = now.saturating_sub(60);
        let exp = now + 9 * 60;
        let claims = Claims {
            iat,
            exp,
            iss: self.app_id.to_string(),
        };
        let mut header = Header::new(Algorithm::RS256);
        header.typ = Some("JWT".into());
        encode(&header, &claims, &self.encoding_key).context("sign GitHub App JWT")
    }

    pub async fn installation_id_for_repo(&self, owner: &str, repo: &str) -> Result<u64> {
        let jwt = self.app_jwt()?;
        let path = format!("/repos/{owner}/{repo}/installation");
        let body: InstallationRepos = self
            .api
            .get_json_as_app(&path, &jwt)
            .await
            .with_context(|| format!("lookup installation for {owner}/{repo}"))?;
        Ok(body.id)
    }

    pub async fn installation_token(&self, installation_id: u64) -> Result<(String, String)> {
        let jwt = self.app_jwt()?;
        let path = format!("/app/installations/{installation_id}/access_tokens");
        let body: TokenResponse = self
            .api
            .post_json_as_app(&path, &jwt, &serde_json::json!({}))
            .await
            .with_context(|| format!("mint installation token {installation_id}"))?;
        if body.token.is_empty() {
            bail!("GitHub returned an empty installation token");
        }
        Ok((body.token, body.expires_at))
    }

    pub async fn token_for_repo(&self, full_name: &str) -> Result<String> {
        if let Some(token) = self.cached_token(full_name) {
            return Ok(token);
        }
        let (owner, repo) = full_name
            .split_once('/')
            .context("repository full_name must be owner/name")?;
        let id = match self.installation_id(full_name) {
            Some(id) => id,
            None => {
                let id = self.installation_id_for_repo(owner, repo).await?;
                self.remember_installation(full_name, id);
                id
            }
        };
        let (token, _) = self.installation_token(id).await?;
        self.remember_token(full_name, token.clone(), now_secs() + TOKEN_TTL_SECS);
        Ok(token)
    }

    fn cached_token(&self, full_name: &str) -> Option<String> {
        let tokens = self.tokens.lock().expect("token cache lock");
        let cached = tokens.get(full_name)?;
        if now_secs() < cached.expires_at {
            Some(cached.token.clone())
        } else {
            None
        }
    }

    fn remember_token(&self, full_name: &str, token: String, expires_at: u64) {
        self.tokens
            .lock()
            .expect("token cache lock")
            .insert(full_name.to_owned(), CachedToken { token, expires_at });
    }

    fn installation_id(&self, full_name: &str) -> Option<u64> {
        self.installations
            .lock()
            .expect("installation cache lock")
            .get(full_name)
            .copied()
    }

    fn remember_installation(&self, full_name: &str, id: u64) {
        self.installations
            .lock()
            .expect("installation cache lock")
            .insert(full_name.to_owned(), id);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_secs()
}
