use std::{env, net::SocketAddr};

use anyhow::{Context, Result};

use crate::auth::ApiKeys;

pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub api_keys: ApiKeys,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let var = |name, default: &str| get(name).unwrap_or_else(|| default.to_owned());
        let api_keys = get("API_KEYS").context("API_KEYS is not set")?;
        Ok(Self {
            bind_addr: var("BIND_ADDR", "127.0.0.1:8080")
                .parse()
                .context("BIND_ADDR is not a valid socket address")?,
            database_url: var("DATABASE_URL", "sqlite://data/control-plane.db"),
            api_keys: ApiKeys::parse(&api_keys).context("invalid API_KEYS")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &str = "a:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";

    fn load(vars: &[(&str, &str)]) -> Result<Config> {
        Config::from_lookup(|k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    #[test]
    fn missing_or_empty_api_keys_fail() {
        assert!(load(&[]).is_err());
        assert!(load(&[("API_KEYS", "")]).is_err());
    }

    #[test]
    fn defaults_apply() {
        let c = load(&[("API_KEYS", KEYS)]).unwrap();
        assert_eq!(c.bind_addr, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(c.database_url, "sqlite://data/control-plane.db");
    }

    #[test]
    fn overrides_apply() {
        let c = load(&[
            ("API_KEYS", KEYS),
            ("BIND_ADDR", "0.0.0.0:9000"),
            ("DATABASE_URL", "sqlite::memory:"),
        ])
        .unwrap();
        assert_eq!(c.bind_addr, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(c.database_url, "sqlite::memory:");
    }

    #[test]
    fn invalid_bind_addr_fails() {
        assert!(load(&[("API_KEYS", KEYS), ("BIND_ADDR", "nope")]).is_err());
    }
}
