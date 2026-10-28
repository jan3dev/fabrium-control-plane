use std::net::IpAddr;

use axum::{
    extract::{Request, State},
    http::{HeaderMap, header::AUTHORIZATION},
    middleware::Next,
    response::Response,
};
use ipnet::IpNet;
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
        // Visit every entry (digests are unique) so the time does not reveal which one matched.
        for (name, hash) in &self.0 {
            if bool::from(digest.as_slice().ct_eq(hash)) {
                found = Some(name.as_str());
            }
        }
        found
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourcesError {
    #[error("API_SOURCES is empty")]
    Empty,
    #[error("API_SOURCES entry {0:?} must look like <tenant>:<ip-or-cidr>")]
    Malformed(String),
    #[error("API_SOURCES entry {0:?} is a range that does not start at its network address")]
    HostBits(String),
    #[error("API_SOURCES names the tenant {0:?}, which API_KEYS does not have")]
    UnknownTenant(String),
    #[error("API_KEYS has the tenant {0:?}, which has no entry in API_SOURCES")]
    NoSource(String),
}

/// The addresses that each tenant may call from.
#[derive(Debug)]
pub struct ApiSources(Vec<(String, IpNet)>);

impl ApiSources {
    /// `s` lists `<tenant>:<ip-or-cidr>` entries. Every entry needs a tenant of `keys`, and
    /// every tenant of `keys` needs an entry.
    pub fn parse(s: &str, keys: &ApiKeys) -> Result<Self, SourcesError> {
        let mut sources: Vec<(String, IpNet)> = Vec::new();
        for entry in s.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let bad = || SourcesError::Malformed(entry.to_owned());
            // The first `:` ends the tenant, so an IPv6 address keeps its own.
            let (tenant, addr) = entry.split_once(':').ok_or_else(bad)?;
            let net = parse_net(addr).ok_or_else(bad)?;
            if tenant.is_empty() {
                return Err(bad());
            }
            // Such a range allows more than its writer may mean: `192.0.2.5/24` is a whole /24.
            if net.addr() != net.network() {
                return Err(SourcesError::HostBits(entry.to_owned()));
            }
            if !keys.0.iter().any(|(name, _)| name == tenant) {
                return Err(SourcesError::UnknownTenant(tenant.to_owned()));
            }
            sources.push((tenant.to_owned(), net));
        }
        if sources.is_empty() {
            return Err(SourcesError::Empty);
        }
        for (name, _) in &keys.0 {
            if !sources.iter().any(|(tenant, _)| tenant == name) {
                return Err(SourcesError::NoSource(name.clone()));
            }
        }
        Ok(Self(sources))
    }

    /// True if `addr` is in a source of `tenant`.
    pub fn allows(&self, tenant: &str, addr: IpAddr) -> bool {
        (self.0.iter()).any(|(name, net)| name == tenant && net.contains(&addr))
    }
}

/// A CIDR range, or a bare address as a range of one host.
fn parse_net(s: &str) -> Option<IpNet> {
    (s.parse::<IpNet>().ok()).or_else(|| s.parse::<IpAddr>().ok().map(IpNet::from))
}

#[derive(Clone, Debug)]
pub struct Client {
    pub name: String,
}

/// The client: the last `X-Forwarded-For` entry, with an IPv4-mapped IPv6 address as IPv4.
/// Caddy sets this header and the app port listens on loopback only, so the last entry is the
/// real client. Entries that the caller sent come before it. `None` if the header is missing
/// or the last entry is not an IP address.
pub(crate) fn client_address(headers: &HeaderMap) -> Option<IpAddr> {
    let line = headers.get_all("x-forwarded-for").iter().next_back()?;
    let entry = line.to_str().ok()?.rsplit(',').next()?;
    Some(entry.trim().parse::<IpAddr>().ok()?.to_canonical())
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
    // The key is valid, so a refusal here may mean that the key leaked. An operator must see it.
    let from = client_address(req.headers());
    if !from.is_some_and(|addr| state.api_sources.allows(&name, addr)) {
        let from = from.map_or_else(|| "none".to_owned(), |addr| addr.to_string());
        tracing::warn!(tenant = %name, from = %from, "refused a request from an address that is not a source of the tenant");
        return Err(AppError::Forbidden);
    }
    req.extensions_mut().insert(Client { name });
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    // SHA-256 of "foo".
    const HASH: &str = "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";
    // SHA-256 of "bar".
    const HASH2: &str = "fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9";

    /// The tenants `zooid` and `other`.
    fn keys() -> ApiKeys {
        ApiKeys::parse(&format!("zooid:{HASH}, other:{HASH2}")).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// The sources of both tenants, plus the entry under test.
    fn parse_with(entry: &str) -> Result<ApiSources, SourcesError> {
        let all = format!("zooid:192.0.2.1, other:198.51.100.1, {entry}");
        ApiSources::parse(&all, &keys())
    }

    /// A request with one `X-Forwarded-For` line for each value.
    fn forwarded(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for v in values {
            headers.append("x-forwarded-for", HeaderValue::from_str(v).unwrap());
        }
        headers
    }

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

    #[test]
    fn sources_accept_hosts_ranges_and_repeated_tenants() {
        let sources = ApiSources::parse(
            "zooid:192.0.2.7, zooid:198.51.100.0/24, zooid:203.0.113.0/25, \
             other:2001:db8:a::/48, other:2001:db8:b::5",
            &keys(),
        )
        .unwrap();
        let allowed = [
            // A bare IP is one host.
            ("zooid", "192.0.2.7"),
            // A range holds its first and its last address. A tenant may have several entries.
            ("zooid", "198.51.100.0"),
            ("zooid", "198.51.100.255"),
            ("zooid", "203.0.113.127"),
            // The first `:` ends the tenant, so an IPv6 address keeps its own.
            ("other", "2001:db8:a:1::1"),
            ("other", "2001:db8:b::5"),
        ];
        for (tenant, addr) in allowed {
            assert!(sources.allows(tenant, ip(addr)), "{tenant} {addr}");
        }
        let refused = [
            ("zooid", "192.0.2.8"),
            ("zooid", "203.0.113.128"),
            ("other", "2001:db8:b::6"),
            ("other", "2001:db8:c::1"),
            // A source belongs to one tenant.
            ("other", "192.0.2.7"),
            ("zooid", "2001:db8:a:1::1"),
            ("nobody", "192.0.2.7"),
            // `allows` takes the address as it is. `client_address` makes it canonical.
            ("zooid", "::ffff:192.0.2.7"),
        ];
        for (tenant, addr) in refused {
            assert!(!sources.allows(tenant, ip(addr)), "{tenant} {addr}");
        }
    }

    #[test]
    fn sources_accept_the_edges_of_the_prefix_rules() {
        let s = parse_with("zooid:0.0.0.0/0").unwrap();
        assert!(s.allows("zooid", ip("203.0.113.9")) && !s.allows("other", ip("203.0.113.9")));
        let s = parse_with("other:::/0").unwrap();
        assert!(s.allows("other", ip("2001:db8::9")) && !s.allows("zooid", ip("2001:db8::9")));
        assert!(parse_with("zooid:192.0.2.5/32").is_ok());
        assert!(parse_with("zooid:2001:db8::5/128").is_ok());
    }

    #[test]
    fn sources_reject_bad_entries() {
        for bad in [
            "nobody",
            ":192.0.2.1",
            "zooid:",
            "zooid:nope",
            "zooid:192.0.2.256",
            "zooid:192.0.2.1/33",
            "zooid:192.0.2.0/",
            "zooid:/24",
            "zooid:192.0.2.0/-1",
            "zooid:192.0.2.0/ 24",
            "zooid:2001:db8::/129",
            "zooid:2001:db8::g",
            "zooid:192.0.2.1:8080",
            "zooid:192.0.2.1 192.0.2.2",
        ] {
            assert!(
                matches!(parse_with(bad), Err(SourcesError::Malformed(e)) if e == bad),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn sources_reject_a_range_with_host_bits() {
        // `192.0.2.5/24` would allow 256 addresses where its writer may mean one.
        for bad in [
            "zooid:192.0.2.5/24",
            "zooid:198.51.100.1/25",
            "zooid:192.0.2.1/0",
            "zooid:2001:db8::1/32",
        ] {
            assert!(
                matches!(parse_with(bad), Err(SourcesError::HostBits(e)) if e == bad),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn sources_and_keys_name_the_same_tenants() {
        for empty in ["", " , "] {
            let got = ApiSources::parse(empty, &keys());
            assert!(matches!(got, Err(SourcesError::Empty)), "{empty:?}");
        }
        // Every tenant of the keys needs a source.
        for (sources, tenant) in [("zooid:192.0.2.1", "other"), ("other:192.0.2.1", "zooid")] {
            let got = ApiSources::parse(sources, &keys());
            assert!(matches!(got, Err(SourcesError::NoSource(t)) if t == tenant));
        }
        // Every source needs a tenant of the keys, and the name is exact.
        for stranger in ["stranger", "Zooid", "zooid ", "-"] {
            let got = parse_with(&format!("{stranger}:192.0.2.1"));
            assert!(
                matches!(got, Err(SourcesError::UnknownTenant(t)) if t == stranger),
                "{stranger:?}"
            );
        }
    }

    #[test]
    fn the_client_is_the_last_forwarded_for_entry() {
        for (header, want) in [
            ("192.0.2.7", "192.0.2.7"),
            ("198.51.100.1, 192.0.2.7", "192.0.2.7"),
            ("198.51.100.1,192.0.2.7", "192.0.2.7"),
            ("198.51.100.1 ,\t192.0.2.7 ", "192.0.2.7"),
            ("  192.0.2.7  ", "192.0.2.7"),
            ("2001:db8::1", "2001:db8::1"),
            ("192.0.2.7, 2001:db8::1", "2001:db8::1"),
            // An entry before the last one can say anything.
            ("nope, 192.0.2.7", "192.0.2.7"),
            // An IPv4-mapped IPv6 address is the IPv4 address, in either notation.
            ("::ffff:192.0.2.7", "192.0.2.7"),
            ("::ffff:c000:207", "192.0.2.7"),
        ] {
            let got = client_address(&forwarded(&[header]));
            assert_eq!(got, Some(ip(want)), "{header:?}");
        }
    }

    #[test]
    fn there_is_no_client_without_a_usable_last_entry() {
        assert_eq!(client_address(&HeaderMap::new()), None);
        for bad in [
            "",
            " ",
            ",",
            "192.0.2.7,",
            "192.0.2.7, ",
            "unknown",
            "192.0.2.7, nope",
            "192.0.2.256",
            "192.0.2.7:443",
            "[2001:db8::1]",
            "192.0.2.7 198.51.100.1",
        ] {
            assert_eq!(client_address(&forwarded(&[bad])), None, "{bad:?}");
        }
        // Bytes that are not visible ASCII.
        let mut headers = HeaderMap::new();
        let value = HeaderValue::from_bytes(b"192.0.2.7\xff").unwrap();
        headers.insert("x-forwarded-for", value);
        assert_eq!(client_address(&headers), None);
    }

    #[test]
    fn with_several_header_lines_the_last_line_counts() {
        // A proxy can add a line instead of joining. Its line is the last one.
        let headers = forwarded(&["192.0.2.7", "198.51.100.1, 203.0.113.9"]);
        assert_eq!(client_address(&headers), Some(ip("203.0.113.9")));
        // An earlier line that is fine does not make up for a last line that is not.
        assert_eq!(client_address(&forwarded(&["192.0.2.7", "unknown"])), None);
    }
}
