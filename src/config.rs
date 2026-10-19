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
        let var = |name, default: &str| env::var(name).unwrap_or_else(|_| default.to_owned());
        let api_keys = env::var("API_KEYS").context("API_KEYS is not set")?;
        Ok(Self {
            bind_addr: var("BIND_ADDR", "127.0.0.1:8080")
                .parse()
                .context("BIND_ADDR is not a valid socket address")?,
            database_url: var("DATABASE_URL", "sqlite://data/control-plane.db"),
            api_keys: ApiKeys::parse(&api_keys).context("invalid API_KEYS")?,
        })
    }
}
