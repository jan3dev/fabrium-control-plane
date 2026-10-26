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
    #[error("API_KEYS name {0:?} may use only a-z, 0-9, and -")]
    BadName(String),
    #[error("API_KEYS has a duplicate name {0:?}")]
    Duplicate(String),
    #[error("API_KEYS entry {0:?} repeats the hash of an earlier entry")]
    DuplicateKey(String),
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
            // The name becomes the value of the `fabrium:tenant` tag.
            if !name
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
            {
                return Err(ParseError::BadName(name.to_owned()));
            }
            if keys.iter().any(|(n, _)| n == name) {
                return Err(ParseError::Duplicate(name.to_owned()));
            }
            if keys.iter().any(|(_, d)| *d == digest) {
                return Err(ParseError::DuplicateKey(name.to_owned()));
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
        // `parse` keeps digests unique, so at most one entry matches.
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

/// Scheme is case-insensitive and whitespace around the token is ignored (RFC 9110).
pub(crate) fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(char::is_whitespace)?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
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
        .and_then(bearer_token)
        .and_then(|key| state.api_keys.find(key))
        .ok_or(AppError::Unauthorized)?
        .to_owned();
    req.extensions_mut().insert(Client { name });
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    // SHA-256 of "foo".
    const HASH: &str = "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";
    // SHA-256 of "bar".
    const HASH2: &str = "fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9";

    #[test]
    fn parses_valid_keys() {
        let keys = ApiKeys::parse(&format!("zooid:{HASH}, other:{HASH2}")).unwrap();
        assert_eq!(keys.find("foo"), Some("zooid"));
        assert_eq!(keys.find("bar"), Some("other"));
        assert_eq!(keys.find("baz"), None);
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
        let dup = format!("a:{HASH},a:{HASH2}");
        assert!(matches!(
            ApiKeys::parse(&dup),
            Err(ParseError::Duplicate(_))
        ));
    }

    #[test]
    fn tenant_names_use_lowercase_letters_digits_and_hyphens() {
        for ok in ["prod-sim", "a1", "stage", "-"] {
            assert!(ApiKeys::parse(&format!("{ok}:{HASH}")).is_ok(), "{ok}");
        }
        for bad in ["Stage", "prod_sim", "a.b", "é", "a b"] {
            assert!(
                matches!(
                    ApiKeys::parse(&format!("{bad}:{HASH}")),
                    Err(ParseError::BadName(n)) if n == bad
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn rejects_same_hash_under_two_names() {
        let dup = format!("a:{HASH},b:{HASH}");
        assert!(matches!(
            ApiKeys::parse(&dup),
            Err(ParseError::DuplicateKey(n)) if n == "b"
        ));
    }

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        for ok in [
            "Bearer k",
            "bearer k",
            "BEARER k",
            "Bearer   k",
            "Bearer \tk ",
        ] {
            assert_eq!(bearer_token(ok), Some("k"), "{ok:?}");
        }
        for bad in ["Bearer", "Bearer ", "Bearer   ", "Basic k", "k", ""] {
            assert_eq!(bearer_token(bad), None, "{bad:?}");
        }
    }
}
