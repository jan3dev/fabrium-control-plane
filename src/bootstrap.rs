//! Single-use bootstrap payloads. A new VM redeems its token once, over HTTPS, and
//! gets the files and the setup script that the tenant sent at create time.
//!
//! `Payload` and `File` have no `Debug`, so a `{:?}` in a log line cannot leak them.

use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long a VM has to redeem its token.
pub const TTL: Duration = Duration::from_secs(15 * 60);

/// The user data stub. It has two placeholders: `<PUBLIC_URL>` and `<TOKEN>`.
const STUB: &str = include_str!("user-data.sh");

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Payload {
    pub files: Vec<File>,
    pub script: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct File {
    pub path: String,
    pub mode: String,
    pub owner: String,
    pub content: String,
}

impl Payload {
    /// The rules of the create request. The error text never holds a value of the payload.
    pub fn validate(&self) -> Result<(), &'static str> {
        for f in &self.files {
            if !valid_path(&f.path) {
                return Err("file path must start with / and have no .. segment");
            }
            if !valid_mode(&f.mode) {
                return Err("file mode must be 3 or 4 octal digits");
            }
            if !valid_owner(&f.owner) {
                return Err("file owner is not a valid user name");
            }
        }
        if self.script.is_empty() {
            return Err("script must not be empty");
        }
        Ok(())
    }
}

fn valid_path(path: &str) -> bool {
    path.starts_with('/') && !path.split('/').any(|segment| segment == "..")
}

/// `^[0-7]{3,4}$`
fn valid_mode(mode: &str) -> bool {
    matches!(mode.len(), 3 | 4) && mode.bytes().all(|b| (b'0'..=b'7').contains(&b))
}

/// `^[a-z_][a-z0-9_-]{0,31}$`
fn valid_owner(owner: &str) -> bool {
    let b = owner.as_bytes();
    (1..=32).contains(&b.len())
        && matches!(b[0], b'a'..=b'z' | b'_')
        && b.iter()
            .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// 32 random bytes, base64url without padding.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS has a random source");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The user data script for one VM.
pub fn stub(public_url: &str, token: &str) -> String {
    // The token goes first, so a URL that holds `<TOKEN>` cannot get a copy of it.
    STUB.replace("<TOKEN>", token)
        .replace("<PUBLIC_URL>", public_url)
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Payloads by the SHA-256 of their token. The store never keeps a token.
///
/// ponytail: in memory. A restart loses the payloads that no VM has redeemed yet, and
/// those VMs show `expired`. Persist the store only if restarts during launches become
/// frequent.
pub struct Store {
    ttl: Duration,
    entries: Mutex<HashMap<[u8; 32], Entry>>,
}

struct Entry {
    payload: Payload,
    instance: String,
    expires: Instant,
}

impl Store {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::default(),
        }
    }

    // The lock is never held across an await, and nothing in a critical section panics.
    fn entries(&self) -> MutexGuard<'_, HashMap<[u8; 32], Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps the payload for `ttl`. Also drops the entries that have expired.
    pub fn insert(&self, token: &str, payload: Payload, instance: &str) {
        let now = Instant::now();
        let mut entries = self.entries();
        entries.retain(|_, e| e.expires > now);
        let entry = Entry {
            payload,
            instance: instance.to_owned(),
            expires: now + self.ttl,
        };
        entries.insert(hash(token), entry);
    }

    /// Removes the entry. `None` if the token is unknown, used, or expired.
    pub fn redeem(&self, token: &str) -> Option<(Payload, String)> {
        let entry = self.entries().remove(&hash(token))?;
        (entry.expires > Instant::now()).then_some((entry.payload, entry.instance))
    }

    /// True if the instance has an entry that has not expired.
    pub fn has_live(&self, instance: &str) -> bool {
        let now = Instant::now();
        (self.entries().values()).any(|e| e.instance == instance && e.expires > now)
    }

    pub fn remove_instance(&self, instance: &str) {
        self.entries().retain(|_, e| e.instance != instance);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, mode: &str, owner: &str) -> File {
        File {
            path: path.to_owned(),
            mode: mode.to_owned(),
            owner: owner.to_owned(),
            content: "secret".to_owned(),
        }
    }

    fn payload() -> Payload {
        Payload {
            files: vec![file("/home/ubuntu/zooid/.env", "600", "ubuntu")],
            script: "#!/bin/bash\ntrue\n".to_owned(),
        }
    }

    fn with_file(f: File) -> Payload {
        Payload {
            files: vec![f],
            ..payload()
        }
    }

    #[test]
    fn token_is_32_random_bytes_in_base64url() {
        let token = new_token();
        assert_eq!(token.len(), 43);
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(URL_SAFE_NO_PAD.decode(&token).unwrap().len(), 32);
        assert_ne!(token, new_token());
    }

    #[test]
    fn redeem_is_single_use() {
        let store = Store::new(TTL);
        store.insert("tok", payload(), "i-1");
        let (got, instance) = store.redeem("tok").unwrap();
        assert!(got == payload());
        assert_eq!(instance, "i-1");
        assert!(store.redeem("tok").is_none());
    }

    #[test]
    fn unknown_token_does_not_redeem() {
        let store = Store::new(TTL);
        store.insert("tok", payload(), "i-1");
        assert!(store.redeem("other").is_none());
        assert!(store.redeem("").is_none());
        // A wrong guess does not burn the real token.
        assert!(store.redeem("tok").is_some());
    }

    #[test]
    fn expired_entry_is_not_live_and_does_not_redeem() {
        let store = Store::new(Duration::ZERO);
        store.insert("tok", payload(), "i-1");
        assert!(!store.has_live("i-1"));
        assert!(store.redeem("tok").is_none());
    }

    #[test]
    fn has_live_follows_the_entry() {
        let store = Store::new(TTL);
        assert!(!store.has_live("i-1"));
        store.insert("tok", payload(), "i-1");
        assert!(store.has_live("i-1"));
        assert!(!store.has_live("i-2"));
        store.redeem("tok").unwrap();
        assert!(!store.has_live("i-1"));

        store.insert("tok2", payload(), "i-1");
        store.insert("tok3", payload(), "i-2");
        store.remove_instance("i-1");
        assert!(!store.has_live("i-1"));
        assert!(store.redeem("tok2").is_none());
        assert!(store.has_live("i-2"));
    }

    #[test]
    fn insert_drops_expired_entries() {
        let store = Store::new(Duration::ZERO);
        store.insert("a", payload(), "i-1");
        store.insert("b", payload(), "i-2");
        // Only the entry that was just inserted can be left.
        assert_eq!(store.entries().len(), 1);
    }

    #[test]
    fn store_keeps_only_the_hash() {
        let store = Store::new(TTL);
        store.insert("tok", payload(), "i-1");
        let keys: Vec<[u8; 32]> = store.entries().keys().copied().collect();
        let want: [u8; 32] = Sha256::digest(b"tok").into();
        assert_eq!(keys, [want]);
    }

    #[test]
    fn payload_rules() {
        assert!(payload().validate().is_ok());
        let no_files = Payload {
            files: vec![],
            ..payload()
        };
        assert!(no_files.validate().is_ok());
        for path in ["/a", "/a/b.c", "/a/..b/c", "/"] {
            assert!(
                with_file(file(path, "600", "ubuntu")).validate().is_ok(),
                "{path}"
            );
        }
        for path in ["home/x", "", "/a/../b", "/..", "../a"] {
            assert!(
                with_file(file(path, "600", "ubuntu")).validate().is_err(),
                "{path:?}"
            );
        }
        for mode in ["600", "0600", "755", "000"] {
            assert!(
                with_file(file("/a", mode, "ubuntu")).validate().is_ok(),
                "{mode}"
            );
        }
        for mode in ["8", "12", "12345", "rwx", "", "60 ", "6o0", "-600"] {
            assert!(
                with_file(file("/a", mode, "ubuntu")).validate().is_err(),
                "{mode:?}"
            );
        }
        for owner in ["ubuntu", "root", "_apt", "a-b_c9"] {
            assert!(
                with_file(file("/a", "600", owner)).validate().is_ok(),
                "{owner}"
            );
        }
        let long = "a".repeat(33);
        for owner in ["Root", "1abc", "", "a b", "a.b", "-a", long.as_str()] {
            assert!(
                with_file(file("/a", "600", owner)).validate().is_err(),
                "{owner:?}"
            );
        }
        assert!(
            Payload {
                script: String::new(),
                ..payload()
            }
            .validate()
            .is_err()
        );
        assert!(
            Payload {
                script: "x".to_owned(),
                ..payload()
            }
            .validate()
            .is_ok()
        );
        let max_owner = "a".repeat(32);
        assert!(with_file(file("/a", "600", &max_owner)).validate().is_ok());
    }

    #[test]
    fn stub_fills_both_placeholders() {
        let token = new_token();
        let script = stub("https://cp.example.org", &token);
        assert!(script.starts_with("#!/bin/bash\n"));
        assert!(script.contains("\nURL='https://cp.example.org/v1/bootstrap'\n"));
        assert!(!script.contains("<PUBLIC_URL>") && !script.contains("<TOKEN>"));
        // The token appears once, in the TOKEN= line.
        assert_eq!(script.matches(&token).count(), 1);
        let line = script.lines().find(|l| l.contains(&token)).unwrap();
        assert_eq!(line, format!("TOKEN='{token}'"));
    }

    #[test]
    fn a_url_cannot_inject_a_second_token() {
        // `<TOKEN>` in the URL stays literal: the token is filled in first.
        let script = stub("https://x.example/<TOKEN>", "abc");
        assert!(script.contains("URL='https://x.example/<TOKEN>/v1/bootstrap'"));
        assert_eq!(script.matches("abc").count(), 1);
    }
}
