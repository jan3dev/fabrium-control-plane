use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{AppState, error::AppError};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("API_KEYS is empty")]
    Empty,
    #[error("API_KEYS entry {0:?} must look like <name>:<sha256-hex>")]
    Malformed(String),
    #[error("API_KEYS has a duplicate name {0:?}")]
    Duplicate(String),
}

#[derive(Debug)]
pub struct ApiKeys(Vec<(String, [u8; 32])>);

impl ApiKeys {
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let mut keys: Vec<(String, [u8; 32])> = Vec::new();
        for entry in s.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let bad = || ParseError::Malformed(entry.to_owned());
            let (name, hash) = entry.split_once(':').ok_or_else(bad)?;
            let mut digest = [0u8; 32];
            if name.is_empty() || hex::decode_to_slice(hash, &mut digest).is_err() {
                return Err(bad());
            }
            if keys.iter().any(|(n, _)| n == name) {
                return Err(ParseError::Duplicate(name.to_owned()));
            }
            keys.push((name.to_owned(), digest));
        }
        if keys.is_empty() {
            return Err(ParseError::Empty);
        }
        Ok(Self(keys))
    }

    fn find(&self, key: &str) -> Option<&str> {
        let digest = Sha256::digest(key.as_bytes());
        let mut found = None;
        // Visit every entry so the time does not reveal which one matched.
        for (name, hash) in &self.0 {
            if bool::from(digest.as_slice().ct_eq(hash)) {
                found = Some(name.as_str());
            }
        }
        found
    }
}

#[derive(Clone, Debug)]
pub struct Client {
    pub name: String,
}

pub async fn require_key(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let name = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|key| state.api_keys.find(key))
        .ok_or(AppError::Unauthorized)?
        .to_owned();
    req.extensions_mut().insert(Client { name });
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";

    #[test]
    fn parses_valid_keys() {
        let keys = ApiKeys::parse(&format!("zooid:{HASH}, other:{HASH}")).unwrap();
        assert_eq!(keys.find("foo"), Some("other"));
        assert_eq!(keys.find("bar"), None);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "",
            " , ",
            "zooid",
            ":abc",
            "zooid:abc",
            &format!("a:{HASH}x"),
        ] {
            assert!(ApiKeys::parse(bad).is_err(), "{bad:?}");
        }
        let dup = format!("a:{HASH},a:{HASH}");
        assert!(matches!(
            ApiKeys::parse(&dup),
            Err(ParseError::Duplicate(_))
        ));
    }
}
