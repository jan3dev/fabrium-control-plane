//! The EC2 port. `AwsEc2` implements it for production and `FakeEc2` for tests.
//!
//! An implementation may return instances of any tenant. The handlers check the
//! `fabrium:tenant` tag of every instance, so a wrong filter here cannot leak a VM.

pub mod aws;
pub mod fake;

use async_trait::async_trait;
use aws_sdk_ec2::primitives::DateTime;

pub use aws::AwsEc2;
pub use fake::FakeEc2;

/// Families a tenant may launch.
pub const FAMILIES: [&str; 5] = ["t3", "t3a", "c6a", "m6a", "r6a"];

/// The text before the first `.`. `None` if there is no `.` or a side is empty.
pub fn family(instance_type: &str) -> Option<&str> {
    let (family, size) = instance_type.split_once('.')?;
    (!family.is_empty() && !size.is_empty()).then_some(family)
}

/// True for the families that have AMD SEV-SNP.
pub fn sev_snp(instance_type: &str) -> bool {
    matches!(family(instance_type), Some("c6a" | "m6a" | "r6a"))
}

/// An EC2 instance, reduced to what the API shows.
#[derive(Clone, Debug)]
pub struct Instance {
    pub id: String,
    /// Tag `Name`, or an empty string.
    pub name: String,
    /// Tag `fabrium:tenant`. `None` for an instance that Fabrium did not launch or adopt.
    pub tenant: Option<String>,
    /// The EC2 state name, such as `running` or `shutting-down`.
    pub state: String,
    pub instance_type: String,
    pub public_ip: Option<String>,
    pub launched_at: DateTime,
    /// Tag `fabrium:setup` is `delivered`.
    pub setup_delivered: bool,
}

pub struct TypeSpec {
    pub name: String,
    pub vcpus: i32,
    pub memory_mib: i64,
}

/// What to launch. No `Debug`: `user_data` holds the bootstrap token.
#[derive(Clone)]
pub struct Launch {
    pub name: String,
    pub tenant: String,
    pub instance_type: String,
    /// The user data script, base64 encoded.
    pub user_data: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Reboot,
    Terminate,
}

impl Action {
    /// The only state this action accepts. `None` means any state.
    pub fn allowed_from(self) -> Option<&'static str> {
        match self {
            Self::Start => Some("stopped"),
            Self::Stop | Self::Reboot => Some("running"),
            Self::Terminate => None,
        }
    }

    /// The state that EC2 moves the instance to.
    pub fn next_state(self) -> &'static str {
        match self {
            Self::Start => "pending",
            Self::Stop => "stopping",
            Self::Reboot => "running",
            Self::Terminate => "shutting-down",
        }
    }
}

#[async_trait]
pub trait Ec2: Send + Sync {
    /// vCPUs and memory of each type, in any order. Fails if EC2 does not know a name.
    async fn instance_types(&self, names: &[String]) -> anyhow::Result<Vec<TypeSpec>>;
    /// The instances of a tenant that are not terminated.
    async fn list(&self, tenant: &str) -> anyhow::Result<Vec<Instance>>;
    /// One instance in any state, or `None` if EC2 has no such id.
    async fn get(&self, id: &str) -> anyhow::Result<Option<Instance>>;
    /// Launches the VM. The result has state `pending`, and `name` and `tenant` of `launch`.
    async fn launch(&self, launch: &Launch) -> anyhow::Result<Instance>;
    async fn act(&self, id: &str, action: Action) -> anyhow::Result<()>;
    /// Sets the tag `fabrium:setup=delivered`.
    async fn mark_setup_delivered(&self, id: &str) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_is_the_text_before_the_first_dot() {
        assert_eq!(family("t3a.medium"), Some("t3a"));
        assert_eq!(family("m6a.large.extra"), Some("m6a"));
        for bad in ["t3", "t3a.", ".large", "", "."] {
            assert_eq!(family(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn sev_snp_is_true_only_for_c6a_m6a_r6a() {
        for yes in ["c6a.large", "m6a.large", "r6a.xlarge"] {
            assert!(sev_snp(yes), "{yes}");
        }
        for no in ["t3.medium", "t3a.large", "m6i.large", "c6a", ""] {
            assert!(!sev_snp(no), "{no}");
        }
    }

    #[test]
    fn actions_have_a_required_state_and_a_next_state() {
        let table = [
            (Action::Start, Some("stopped"), "pending"),
            (Action::Stop, Some("running"), "stopping"),
            (Action::Reboot, Some("running"), "running"),
            (Action::Terminate, None, "shutting-down"),
        ];
        for (action, from, next) in table {
            assert_eq!(action.allowed_from(), from, "{action:?}");
            assert_eq!(action.next_state(), next, "{action:?}");
        }
    }
}
