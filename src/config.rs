use std::{env, net::SocketAddr};

use anyhow::{Context, Result, ensure};

use crate::{auth::ApiKeys, ec2};

const DEFAULT_INSTANCE_TYPES: &str =
    "t3a.medium,t3a.large,t3.medium,t3.large,m6a.large,c6a.large,r6a.large";

pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub api_keys: ApiKeys,
    pub vm: VmConfig,
}

/// Where and how the service launches VMs.
pub struct VmConfig {
    /// Base URL that VMs use to redeem their token. No trailing slash.
    pub public_url: String,
    pub aws_region: String,
    pub ami_name: String,
    /// The allowlist, in the order the API lists it.
    pub instance_types: Vec<String>,
    pub default_instance_type: String,
    pub security_group: String,
    pub key_name: String,
}

/// An empty value counts as unset, so `NAME=` in an env file keeps the default.
fn env_or(get: &impl Fn(&str) -> Option<String>, name: &str, default: &str) -> String {
    get(name)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|name| env::var(name).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let api_keys = get("API_KEYS").context("API_KEYS is not set")?;
        Ok(Self {
            bind_addr: env_or(&get, "BIND_ADDR", "127.0.0.1:8080")
                .parse()
                .context("BIND_ADDR is not a valid socket address")?,
            database_url: env_or(&get, "DATABASE_URL", "sqlite://data/control-plane.db"),
            api_keys: ApiKeys::parse(&api_keys).context("invalid API_KEYS")?,
            vm: VmConfig::from_lookup(&get)?,
        })
    }
}

impl VmConfig {
    fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self> {
        let var = |name, default: &str| env_or(get, name, default);
        let public_url = get("PUBLIC_URL")
            .filter(|v| !v.is_empty())
            .context("PUBLIC_URL is not set")?;
        let instance_types = parse_instance_types(&var("INSTANCE_TYPES", DEFAULT_INSTANCE_TYPES))?;
        let default_instance_type = var("DEFAULT_INSTANCE_TYPE", "t3a.medium");
        ensure!(
            instance_types.contains(&default_instance_type),
            "DEFAULT_INSTANCE_TYPE {default_instance_type:?} is not in INSTANCE_TYPES"
        );
        Ok(Self {
            public_url: public_url_base(&public_url)?,
            aws_region: var("AWS_REGION", "us-east-2"),
            ami_name: var("AMI_NAME", "ubuntu-24.04-jan3-dev-env-v3"),
            instance_types,
            default_instance_type,
            security_group: var("SECURITY_GROUP", "jan3-dev-env-ssh"),
            key_name: var("KEY_NAME", "fabrium-vms"),
        })
    }
}

/// The URL goes inside single quotes in the user data script, so a quote would break it.
fn public_url_base(raw: &str) -> Result<String> {
    let url = raw.trim_end_matches('/');
    let has_host = url
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty());
    let clean = !url
        .chars()
        .any(|c| c == '\'' || c.is_whitespace() || c.is_control());
    ensure!(
        has_host && clean,
        "PUBLIC_URL must be an https URL without quotes or whitespace"
    );
    Ok(url.to_owned())
}

fn parse_instance_types(raw: &str) -> Result<Vec<String>> {
    let list: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    ensure!(!list.is_empty(), "INSTANCE_TYPES is empty");
    for (i, t) in list.iter().enumerate() {
        ensure!(
            ec2::family(t).is_some_and(|f| ec2::FAMILIES.contains(&f)),
            "INSTANCE_TYPES entry {t:?} must look like <family>.<size>, with a family of {:?}",
            ec2::FAMILIES
        );
        ensure!(!list[..i].contains(t), "INSTANCE_TYPES lists {t:?} twice");
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &str = "a:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";
    const URL: &str = "https://cp.example.org";

    fn load_raw(vars: &[(&str, &str)]) -> Result<Config> {
        Config::from_lookup(|k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    /// `API_KEYS` and `PUBLIC_URL` are set unless the case overrides them.
    fn load(vars: &[(&str, &str)]) -> Result<Config> {
        load_raw(&[vars, &[("API_KEYS", KEYS), ("PUBLIC_URL", URL)]].concat())
    }

    #[test]
    fn missing_or_empty_api_keys_fail() {
        assert!(load_raw(&[("PUBLIC_URL", URL)]).is_err());
        assert!(load(&[("API_KEYS", "")]).is_err());
    }

    #[test]
    fn defaults_apply() {
        let c = load(&[]).unwrap();
        assert_eq!(c.bind_addr, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(c.database_url, "sqlite://data/control-plane.db");
    }

    #[test]
    fn overrides_apply() {
        let c = load(&[
            ("BIND_ADDR", "0.0.0.0:9000"),
            ("DATABASE_URL", "sqlite::memory:"),
        ])
        .unwrap();
        assert_eq!(c.bind_addr, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(c.database_url, "sqlite::memory:");
    }

    #[test]
    fn invalid_bind_addr_fails() {
        assert!(load(&[("BIND_ADDR", "nope")]).is_err());
    }

    #[test]
    fn public_url_is_required() {
        assert!(load_raw(&[("API_KEYS", KEYS)]).is_err());
        assert!(load(&[("PUBLIC_URL", "")]).is_err());
    }

    #[test]
    fn public_url_must_be_https_without_quotes_or_whitespace() {
        for bad in [
            "http://x",
            "https://",
            "https:///",
            "ftp://x",
            "https://a'b",
            "https://a b",
            "https://a\nb",
            "cp.example.org",
            " https://x",
        ] {
            assert!(load(&[("PUBLIC_URL", bad)]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn public_url_loses_trailing_slashes() {
        for (raw, want) in [
            ("https://cp.example.org", "https://cp.example.org"),
            ("https://cp.example.org/", "https://cp.example.org"),
            ("https://cp.example.org//", "https://cp.example.org"),
            (
                "https://cp.example.org/base/",
                "https://cp.example.org/base",
            ),
        ] {
            assert_eq!(load(&[("PUBLIC_URL", raw)]).unwrap().vm.public_url, want);
        }
    }

    #[test]
    fn vm_defaults_apply() {
        let vm = load(&[]).unwrap().vm;
        assert_eq!(vm.public_url, URL);
        assert_eq!(vm.aws_region, "us-east-2");
        assert_eq!(vm.ami_name, "ubuntu-24.04-jan3-dev-env-v3");
        assert_eq!(
            vm.instance_types,
            [
                "t3a.medium",
                "t3a.large",
                "t3.medium",
                "t3.large",
                "m6a.large",
                "c6a.large",
                "r6a.large"
            ]
        );
        assert_eq!(vm.default_instance_type, "t3a.medium");
        assert_eq!(vm.security_group, "jan3-dev-env-ssh");
        assert_eq!(vm.key_name, "fabrium-vms");
    }

    #[test]
    fn vm_overrides_apply() {
        let vm = load(&[
            ("AWS_REGION", "eu-west-1"),
            ("AMI_NAME", "my-image"),
            ("INSTANCE_TYPES", " t3.large , c6a.large "),
            ("DEFAULT_INSTANCE_TYPE", "c6a.large"),
            ("SECURITY_GROUP", "my-sg"),
            ("KEY_NAME", "my-key"),
        ])
        .unwrap()
        .vm;
        assert_eq!(vm.aws_region, "eu-west-1");
        assert_eq!(vm.ami_name, "my-image");
        assert_eq!(vm.instance_types, ["t3.large", "c6a.large"]);
        assert_eq!(vm.default_instance_type, "c6a.large");
        assert_eq!(vm.security_group, "my-sg");
        assert_eq!(vm.key_name, "my-key");
    }

    #[test]
    fn empty_values_use_defaults() {
        let c = load(&[("AWS_REGION", ""), ("BIND_ADDR", "")]).unwrap();
        assert_eq!(c.vm.aws_region, "us-east-2");
        assert_eq!(c.bind_addr, "127.0.0.1:8080".parse().unwrap());
    }

    #[test]
    fn instance_type_families_are_checked() {
        // Each default is the first entry, so only the family check can fail.
        for (list, default) in [
            ("t3a.medium,m5.large", "t3a.medium"),
            ("c6i.large", "c6i.large"),
            ("T3A.medium", "T3A.medium"),
            ("t3", "t3"),
            ("t3a.", "t3a."),
            (".large", ".large"),
            (" , ", "t3a.medium"),
        ] {
            let vars = [("INSTANCE_TYPES", list), ("DEFAULT_INSTANCE_TYPE", default)];
            assert!(load(&vars).is_err(), "{list:?}");
        }
    }

    #[test]
    fn duplicate_instance_types_fail() {
        assert!(load(&[("INSTANCE_TYPES", "t3a.medium,t3a.medium")]).is_err());
    }

    #[test]
    fn default_instance_type_must_be_in_the_list() {
        let vars = [
            ("INSTANCE_TYPES", "t3a.medium"),
            ("DEFAULT_INSTANCE_TYPE", "t3a.large"),
        ];
        assert!(load(&vars).is_err());
        // A custom list that leaves out the default default.
        assert!(load(&[("INSTANCE_TYPES", "t3.large")]).is_err());
    }
}
