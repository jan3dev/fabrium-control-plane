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

/// An absolute path to a file: no trailing `/`, no control character (the stub would drop a
/// NUL byte and read a newline as the end of the path), and no `..` segment.
fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.ends_with('/')
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|segment| segment == "..")
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
    grace: Duration,
    entries: Mutex<HashMap<[u8; 32], Entry>>,
}

/// How long a redeemed payload stays, so that a VM whose first answer got lost can ask
/// again from the same address. The TTL still ends it.
pub const GRACE: Duration = Duration::from_secs(60);

/// What a redeem of a live token gets. No `Debug`: the payload holds secrets.
pub enum Redeemed {
    Payload(Payload, String),
    /// A repeat from another address than the first redeem: the instance and that first
    /// address. Only the caller of the first redeem, the VM, gets the payload again.
    Refused {
        instance: String,
        first: String,
    },
}

struct Entry {
    payload: Payload,
    instance: String,
    /// The tenant and the name of the VM, so that a VM that EC2 does not list yet still
    /// holds its name.
    tenant: String,
    name: String,
    expires: Instant,
    /// The time and the caller address of the first redeem.
    redeemed: Option<(Instant, String)>,
}

impl Entry {
    /// The TTL, or the end of the grace time after the first redeem if that comes first.
    fn end(&self, grace: Duration) -> Instant {
        (self.redeemed.as_ref()).map_or(self.expires, |(at, _)| self.expires.min(*at + grace))
    }
}

impl Store {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            grace: GRACE,
            entries: Mutex::default(),
        }
    }

    /// Replaces the grace time of `GRACE`.
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    // The lock is never held across an await, and nothing in a critical section panics.
    fn entries(&self) -> MutexGuard<'_, HashMap<[u8; 32], Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The entries, after the ones that have ended are dropped. Every read goes through
    /// here, so a payload, which holds secrets, does not stay in memory after its time.
    fn live(&self) -> MutexGuard<'_, HashMap<[u8; 32], Entry>> {
        let now = Instant::now();
        let mut entries = self.entries();
        entries.retain(|_, e| e.end(self.grace) > now);
        entries
    }

    pub fn insert(&self, token: &str, payload: Payload, instance: &str, tenant: &str, name: &str) {
        let entry = Entry {
            payload,
            instance: instance.to_owned(),
            tenant: tenant.to_owned(),
            name: name.to_owned(),
            expires: Instant::now() + self.ttl,
            redeemed: None,
        };
        self.live().insert(hash(token), entry);
    }

    /// Redeems `token` for the caller at `from`. In the grace time, a repeat from the same address
    /// gets the same payload, and a repeat from another address is refused (README, "Bootstrap").
    /// `None` if the token is unknown or its entry has ended.
    pub fn redeem(&self, token: &str, from: &str) -> Option<Redeemed> {
        // `live` has dropped an entry whose TTL or grace time is over.
        let mut entries = self.live();
        let entry = entries.get_mut(&hash(token))?;
        if let Some((_, first)) = &entry.redeemed
            && first != from
        {
            let (instance, first) = (entry.instance.clone(), first.clone());
            return Some(Redeemed::Refused { instance, first });
        }
        (entry.redeemed).get_or_insert_with(|| (Instant::now(), from.to_owned()));
        Some(Redeemed::Payload(
            entry.payload.clone(),
            entry.instance.clone(),
        ))
    }

    /// The instance of the live entry for `token`. This reads the entry and changes nothing: it
    /// is not a redeem. `None` if the token is unknown or its entry has ended.
    pub fn instance_of(&self, token: &str) -> Option<String> {
        self.live().get(&hash(token)).map(|e| e.instance.clone())
    }

    /// True if the instance has a live entry: a token that no VM has redeemed, or one in its
    /// grace time. In the grace time, the tag `fabrium:setup=delivered` may not show yet.
    pub fn has_live(&self, instance: &str) -> bool {
        (self.live().values()).any(|e| e.instance == instance)
    }

    /// True if a VM of the tenant with this name is still waiting for its token.
    pub fn name_in_use(&self, tenant: &str, name: &str) -> bool {
        (self.live().values()).any(|e| e.tenant == tenant && e.name == name && e.redeemed.is_none())
    }

    pub fn remove_instance(&self, instance: &str) {
        self.entries().retain(|_, e| e.instance != instance);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VM: &str = "203.0.113.10";

    /// A redeem from the VM: the payload and the instance, or `None`.
    fn get(store: &Store, token: &str) -> Option<(Payload, String)> {
        match store.redeem(token, VM)? {
            Redeemed::Payload(payload, instance) => Some((payload, instance)),
            Redeemed::Refused { .. } => None,
        }
    }

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
    fn a_repeat_within_the_grace_time_gets_the_same_payload() {
        let store = Store::new(TTL);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        let (got, instance) = get(&store, "tok").unwrap();
        assert!(got == payload());
        assert_eq!(instance, "i-1");
        // The first answer got lost on its way: the VM asks again.
        let (again, instance) = get(&store, "tok").unwrap();
        assert!(again == payload());
        assert_eq!(instance, "i-1");
    }

    #[test]
    fn a_repeat_from_another_address_is_refused() {
        let store = Store::new(TTL);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(get(&store, "tok").is_some());
        // The token leaked: someone else asks within the grace time.
        let Some(Redeemed::Refused { instance, first }) = store.redeem("tok", "198.51.100.7")
        else {
            panic!("a repeat from another address must be refused");
        };
        assert_eq!((instance.as_str(), first.as_str()), ("i-1", VM));
        // The VM itself can still ask again.
        assert!(get(&store, "tok").is_some());
    }

    #[test]
    fn a_repeat_after_the_grace_time_is_refused() {
        let store = Store::new(TTL).with_grace(Duration::ZERO);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(get(&store, "tok").is_some());
        assert!(get(&store, "tok").is_none());
        // The entry is gone, not only refused.
        assert!(store.entries().is_empty());
    }

    #[test]
    fn the_grace_time_ends_with_the_ttl() {
        let store = Store::new(Duration::from_millis(200));
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(get(&store, "tok").is_some());
        std::thread::sleep(Duration::from_millis(250));
        assert!(get(&store, "tok").is_none());
    }

    #[test]
    fn every_read_drops_the_entries_past_their_grace_time() {
        // The payload holds secrets: it must not stay after its grace time.
        let store = Store::new(TTL).with_grace(Duration::ZERO);
        store.insert("a", payload(), "i-1", "stage", "lima");
        get(&store, "a").unwrap();
        assert!(!store.has_live("i-1"));
        assert_eq!(store.entries().len(), 0);
    }

    #[test]
    fn unknown_token_does_not_redeem() {
        let store = Store::new(TTL);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(get(&store, "other").is_none());
        assert!(get(&store, "").is_none());
        // A wrong guess does not burn the real token.
        assert!(get(&store, "tok").is_some());
    }

    #[test]
    fn expired_entry_is_not_live_and_does_not_redeem() {
        let store = Store::new(Duration::ZERO);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(!store.has_live("i-1"));
        assert!(get(&store, "tok").is_none());
    }

    #[test]
    fn instance_of_reads_a_live_entry_without_redeeming_it() {
        // Without a grace time, a redeem would end the entry at once.
        let store = Store::new(TTL).with_grace(Duration::ZERO);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert_eq!(store.instance_of("tok").as_deref(), Some("i-1"));
        assert_eq!(store.instance_of("tok").as_deref(), Some("i-1"));
        assert_eq!(store.instance_of("other"), None);
        assert_eq!(store.instance_of(""), None);
        // The reads did not redeem: the first redeem still gets the payload.
        assert!(get(&store, "tok").is_some());
        assert_eq!(store.instance_of("tok"), None);
    }

    #[test]
    fn instance_of_does_not_set_the_address_of_the_first_redeem() {
        let store = Store::new(TTL);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(store.instance_of("tok").is_some());
        // The first redeem sets the address, whoever it is. It is not a repeat.
        let first = store.redeem("tok", "198.51.100.7");
        assert!(matches!(first, Some(Redeemed::Payload(..))));
        // Now another address is a repeat.
        let repeat = store.redeem("tok", VM);
        assert!(matches!(repeat, Some(Redeemed::Refused { .. })));
    }

    #[test]
    fn instance_of_is_none_for_an_ended_entry() {
        let store = Store::new(Duration::ZERO);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert_eq!(store.instance_of("tok"), None);
        // Like every read, it drops the entries that have ended.
        assert_eq!(store.entries().len(), 0);
    }

    #[test]
    fn has_live_follows_the_entry() {
        let store = Store::new(TTL);
        assert!(!store.has_live("i-1"));
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(store.has_live("i-1"));
        assert!(!store.has_live("i-2"));
        get(&store, "tok").unwrap();
        // In the grace time, the tag may not show yet: the VM is still `pending`.
        assert!(store.has_live("i-1"));

        store.insert("tok2", payload(), "i-1", "stage", "lima");
        store.insert("tok3", payload(), "i-2", "stage", "other");
        store.remove_instance("i-1");
        assert!(!store.has_live("i-1"));
        assert!(get(&store, "tok2").is_none());
        assert!(store.has_live("i-2"));
    }

    #[test]
    fn a_live_entry_holds_the_name_of_its_vm() {
        let store = Store::new(TTL);
        assert!(!store.name_in_use("stage", "lima"));
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(store.name_in_use("stage", "lima"));
        // The name is per tenant.
        assert!(!store.name_in_use("prod", "lima"));
        assert!(!store.name_in_use("stage", "other"));
        // A VM that redeemed its token, or was deleted, is known to EC2 or gone.
        get(&store, "tok").unwrap();
        assert!(!store.name_in_use("stage", "lima"));
        store.insert("tok2", payload(), "i-2", "stage", "other");
        store.remove_instance("i-2");
        assert!(!store.name_in_use("stage", "other"));
    }

    #[test]
    fn an_expired_entry_does_not_hold_a_name() {
        let store = Store::new(Duration::ZERO);
        store.insert("tok", payload(), "i-1", "stage", "lima");
        assert!(!store.name_in_use("stage", "lima"));
    }

    #[test]
    fn every_read_drops_the_expired_entries() {
        // A zero TTL expires the entry at once: its secrets must not wait for the next insert.
        let store = Store::new(Duration::ZERO);
        store.insert("a", payload(), "i-1", "stage", "lima");
        assert_eq!(store.entries().len(), 1);
        assert!(!store.has_live("i-1"));
        assert_eq!(store.entries().len(), 0, "has_live");

        store.insert("b", payload(), "i-2", "stage", "other");
        assert!(get(&store, "unknown").is_none());
        assert_eq!(store.entries().len(), 0, "redeem of another token");

        store.insert("c", payload(), "i-3", "stage", "third");
        assert!(!store.name_in_use("stage", "third"));
        assert_eq!(store.entries().len(), 0, "name_in_use");
    }

    #[test]
    fn insert_drops_expired_entries() {
        let store = Store::new(Duration::ZERO);
        store.insert("a", payload(), "i-1", "stage", "lima");
        store.insert("b", payload(), "i-2", "stage", "other");
        // Only the entry that was just inserted can be left.
        assert_eq!(store.entries().len(), 1);
    }

    #[test]
    fn store_keeps_only_the_hash() {
        let store = Store::new(TTL);
        store.insert("tok", payload(), "i-1", "stage", "lima");
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
        for path in ["/a", "/a/b.c", "/a/..b/c", "/a b", "/ä/日本"] {
            assert!(
                with_file(file(path, "600", "ubuntu")).validate().is_ok(),
                "{path}"
            );
        }
        let bad_paths = [
            "home/x",
            "",
            "/a/../b",
            "/..",
            "../a",
            // A path names a file: no trailing slash.
            "/",
            "/a/",
            // No control character, so no NUL, newline, or tab.
            "/a\n",
            "/a\r",
            "/a\0b",
            "/a\tb",
            "/a\u{7f}",
            "/\u{0}../etc/x",
        ];
        for path in bad_paths {
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
