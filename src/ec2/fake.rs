//! An in-memory `Ec2` for tests.
//!
//! It is a worst-case port: `list` and `get` return every instance of every tenant
//! and state, so a test that passes proves that the handlers do the filtering.

use std::{
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use aws_sdk_ec2::primitives::DateTime;

use super::{Action, Ec2, Instance, Launch, TypeSpec};

/// 2026-10-26T19:00:00Z, the launch time of every instance that the fake makes.
pub const LAUNCHED_AT_SECS: i64 = 1_793_041_200;

/// An instance for `FakeEc2::seed`, with no public IP and no `fabrium:setup` tag.
pub fn instance(
    id: &str,
    name: &str,
    tenant: Option<&str>,
    state: &str,
    instance_type: &str,
) -> Instance {
    Instance {
        id: id.to_owned(),
        name: name.to_owned(),
        tenant: tenant.map(str::to_owned),
        state: state.to_owned(),
        instance_type: instance_type.to_owned(),
        public_ip: None,
        launched_at: DateTime::from_secs(LAUNCHED_AT_SECS),
        setup_delivered: false,
    }
}

#[derive(Default)]
pub struct FakeEc2 {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    instances: Vec<Instance>,
    launches: Vec<Launch>,
    actions: Vec<(String, Action)>,
    type_calls: usize,
    fail: bool,
    fail_writes: bool,
    fail_tagging: bool,
    hang_tagging: bool,
    launch_delay: Duration,
    hide_launches: bool,
    launched_ids: Vec<String>,
}

impl FakeEc2 {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn check(&self) -> Result<()> {
        ensure!(!self.inner().fail, "fake EC2 failure");
        Ok(())
    }

    pub fn seed(&self, instance: Instance) {
        self.inner().instances.push(instance);
    }

    pub fn instance(&self, id: &str) -> Option<Instance> {
        self.inner().instances.iter().find(|i| i.id == id).cloned()
    }

    /// Every instance that EC2 holds, seeded or launched.
    pub fn instances(&self) -> Vec<Instance> {
        self.inner().instances.clone()
    }

    pub fn launches(&self) -> Vec<Launch> {
        self.inner().launches.clone()
    }

    /// The instances that `launch` made, in order.
    pub fn launched(&self) -> Vec<Instance> {
        let inner = self.inner();
        let find = |id: &String| inner.instances.iter().find(|i| i.id == *id);
        inner
            .launched_ids
            .iter()
            .filter_map(find)
            .cloned()
            .collect()
    }

    /// Sets or removes the public IP of an instance, as EC2 does at a stop and a start.
    pub fn set_public_ip(&self, id: &str, ip: Option<&str>) {
        let mut inner = self.inner();
        let found = (inner.instances.iter_mut().find(|i| i.id == id))
            .expect("a seeded or launched instance");
        found.public_ip = ip.map(str::to_owned);
    }

    pub fn actions(&self) -> Vec<(String, Action)> {
        self.inner().actions.clone()
    }

    /// How many times `instance_types` was called, failed calls included.
    pub fn type_calls(&self) -> usize {
        self.inner().type_calls
    }

    /// While on, every call returns an error.
    pub fn fail(&self, on: bool) {
        self.inner().fail = on;
    }

    /// While on, `launch` and `act` return an error. Lookups still work.
    pub fn fail_writes(&self, on: bool) {
        self.inner().fail_writes = on;
    }

    /// While on, `mark_setup_delivered` returns an error.
    pub fn fail_tagging(&self, on: bool) {
        self.inner().fail_tagging = on;
    }

    /// While on, `mark_setup_delivered` never finishes.
    pub fn hang_tagging(&self, on: bool) {
        self.inner().hang_tagging = on;
    }

    /// While on, `list` leaves out the instances that `launch` made, as EC2 can right after
    /// `RunInstances`. `get` still finds them.
    pub fn hide_launches(&self, on: bool) {
        self.inner().hide_launches = on;
    }

    /// `launch` holds the VM at once, as EC2 does when it accepts `RunInstances`, and
    /// answers after `delay`. A caller that gives up in between still has a launched VM.
    pub fn delay_launch(&self, delay: Duration) {
        self.inner().launch_delay = delay;
    }
}

// No lock guard lives across an await: the futures must be `Send`.
#[async_trait]
impl Ec2 for FakeEc2 {
    async fn instance_types(&self, names: &[String]) -> Result<Vec<TypeSpec>> {
        self.inner().type_calls += 1;
        self.check()?;
        Ok(names
            .iter()
            .map(|name| TypeSpec {
                name: name.clone(),
                vcpus: 2,
                memory_mib: if name.ends_with(".large") { 8192 } else { 4096 },
            })
            .collect())
    }

    async fn list(&self, _tenant: &str) -> Result<Vec<Instance>> {
        tokio::task::yield_now().await;
        self.check()?;
        let inner = self.inner();
        let hidden = |i: &Instance| inner.hide_launches && inner.launched_ids.contains(&i.id);
        Ok(inner
            .instances
            .iter()
            .filter(|i| !hidden(i))
            .cloned()
            .collect())
    }

    async fn get(&self, id: &str) -> Result<Option<Instance>> {
        tokio::task::yield_now().await;
        self.check()?;
        Ok(self.instance(id))
    }

    async fn launch(&self, launch: &Launch) -> Result<Instance> {
        tokio::task::yield_now().await;
        self.check()?;
        let (launched, delay) = {
            let mut inner = self.inner();
            ensure!(!inner.fail_writes, "fake EC2 write failure");
            let n = inner.launches.len() + 1;
            // The 254 addresses of 203.0.113.0/24 (RFC 5737), one for each launch: a test can tell
            // the VMs apart. A launch after the last would repeat one.
            ensure!(n < 255, "fake EC2 has no public IP left");
            // 17 hex digits, like EC2. The `fa` prefix keeps these apart from seeded ids.
            let id = format!("i-fa{n:015x}");
            let launched = Instance {
                public_ip: Some(format!("203.0.113.{n}")),
                ..instance(
                    &id,
                    &launch.name,
                    Some(&launch.tenant),
                    "pending",
                    &launch.instance_type,
                )
            };
            inner.instances.push(launched.clone());
            inner.launched_ids.push(id);
            inner.launches.push(launch.clone());
            (launched, inner.launch_delay)
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        Ok(launched)
    }

    async fn act(&self, id: &str, action: Action) -> Result<()> {
        self.check()?;
        let mut inner = self.inner();
        ensure!(!inner.fail_writes, "fake EC2 write failure");
        let found = (inner.instances.iter_mut().find(|i| i.id == id))
            .with_context(|| format!("no instance {id}"))?;
        found.state = action.next_state().to_owned();
        inner.actions.push((id.to_owned(), action));
        Ok(())
    }

    async fn mark_setup_delivered(&self, id: &str) -> Result<()> {
        self.check()?;
        let hang = self.inner().hang_tagging;
        if hang {
            std::future::pending::<()>().await;
        }
        let mut inner = self.inner();
        ensure!(!inner.fail_tagging, "fake EC2 tagging failure");
        let found = (inner.instances.iter_mut().find(|i| i.id == id))
            .with_context(|| format!("no instance {id}"))?;
        found.setup_delivered = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(name: &str) -> Launch {
        Launch {
            name: name.to_owned(),
            tenant: "stage".to_owned(),
            instance_type: "t3a.medium".to_owned(),
            user_data: "dXNlcg==".to_owned(),
        }
    }

    #[tokio::test]
    async fn list_and_get_ignore_tenant_and_state() {
        let fake = FakeEc2::default();
        fake.seed(instance(
            "i-00000001",
            "a",
            Some("prod"),
            "running",
            "t3.medium",
        ));
        fake.seed(instance("i-00000002", "b", None, "terminated", "t3.medium"));
        assert_eq!(fake.list("stage").await.unwrap().len(), 2);
        let got = fake.get("i-00000002").await.unwrap().unwrap();
        assert_eq!(got.state, "terminated");
        assert!(fake.get("i-00000003").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn launch_makes_a_pending_instance_with_a_valid_id() {
        let fake = FakeEc2::default();
        let a = fake.launch(&launch("a")).await.unwrap();
        let b = fake.launch(&launch("b")).await.unwrap();
        for i in [&a, &b] {
            let hex = i.id.strip_prefix("i-").unwrap();
            assert_eq!(hex.len(), 17, "{}", i.id);
            assert!(hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
            assert_eq!(i.state, "pending");
            assert_eq!(i.tenant.as_deref(), Some("stage"));
            assert!(!i.setup_delivered);
        }
        assert_ne!(a.id, b.id);
        assert_eq!(a.name, "a");
        assert_eq!(fake.launches().len(), 2);
        assert_eq!(fake.instance(&a.id).unwrap().name, "a");
    }

    #[tokio::test]
    async fn launch_gives_each_instance_its_own_public_ip() {
        let fake = FakeEc2::default();
        let a = fake.launch(&launch("a")).await.unwrap();
        let b = fake.launch(&launch("b")).await.unwrap();
        // A documentation address (RFC 5737) for each launch. `get` and `list` show it too.
        assert_eq!(a.public_ip.as_deref(), Some("203.0.113.1"));
        assert_eq!(b.public_ip.as_deref(), Some("203.0.113.2"));
        let got = fake.get(&b.id).await.unwrap().unwrap();
        assert_eq!(got.public_ip.as_deref(), Some("203.0.113.2"));
        let listed = fake.list("stage").await.unwrap();
        assert_eq!(listed[0].public_ip.as_deref(), Some("203.0.113.1"));
        // A seeded instance has the address that the test gives it: none.
        fake.seed(instance(
            "i-00000001",
            "c",
            Some("stage"),
            "running",
            "t3.medium",
        ));
        assert_eq!(fake.instance("i-00000001").unwrap().public_ip, None);
    }

    #[tokio::test]
    async fn the_fake_has_254_public_ips_to_give() {
        let fake = FakeEc2::default();
        for _ in 0..254 {
            fake.launch(&launch("a")).await.unwrap();
        }
        let last = fake.instances().pop().unwrap();
        assert_eq!(last.public_ip.as_deref(), Some("203.0.113.254"));
        // Another launch would repeat an address, so it fails.
        assert!(fake.launch(&launch("b")).await.is_err());
    }

    #[tokio::test]
    async fn set_public_ip_changes_one_instance() {
        let fake = FakeEc2::default();
        let a = fake.launch(&launch("a")).await.unwrap();
        let b = fake.launch(&launch("b")).await.unwrap();
        fake.set_public_ip(&a.id, None);
        assert_eq!(fake.get(&a.id).await.unwrap().unwrap().public_ip, None);
        fake.set_public_ip(&b.id, Some("198.51.100.9"));
        let got = fake.get(&b.id).await.unwrap().unwrap();
        assert_eq!(got.public_ip.as_deref(), Some("198.51.100.9"));
        assert_eq!(fake.get(&a.id).await.unwrap().unwrap().public_ip, None);
    }

    #[tokio::test]
    async fn launched_lists_the_instances_that_launch_made() {
        let fake = FakeEc2::default();
        fake.seed(instance(
            "i-00000001",
            "seeded",
            Some("stage"),
            "running",
            "t3.medium",
        ));
        let a = fake.launch(&launch("a")).await.unwrap();
        let b = fake.launch(&launch("b")).await.unwrap();
        let ids: Vec<_> = fake.launched().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, [a.id, b.id]);
    }

    #[tokio::test]
    async fn actions_and_tagging_change_the_instance() {
        let fake = FakeEc2::default();
        let i = fake.launch(&launch("a")).await.unwrap();
        fake.act(&i.id, Action::Stop).await.unwrap();
        assert_eq!(fake.instance(&i.id).unwrap().state, "stopping");
        assert_eq!(fake.actions(), [(i.id.clone(), Action::Stop)]);
        fake.mark_setup_delivered(&i.id).await.unwrap();
        assert!(fake.instance(&i.id).unwrap().setup_delivered);
        assert!(fake.act("i-00000009", Action::Stop).await.is_err());
    }

    #[tokio::test]
    async fn failure_switches_make_calls_fail() {
        let fake = FakeEc2::default();
        let i = fake.launch(&launch("a")).await.unwrap();
        fake.fail_tagging(true);
        assert!(fake.mark_setup_delivered(&i.id).await.is_err());
        assert!(fake.list("stage").await.is_ok());
        fake.fail_tagging(false);
        fake.fail_writes(true);
        assert!(fake.act(&i.id, Action::Stop).await.is_err());
        assert!(fake.launch(&launch("b")).await.is_err());
        assert!(fake.get(&i.id).await.is_ok());
        assert!(fake.actions().is_empty());
        assert_eq!(fake.launches().len(), 1);
        fake.fail_writes(false);
        fake.fail(true);
        assert!(fake.list("stage").await.is_err());
        assert!(fake.get(&i.id).await.is_err());
        assert!(fake.launch(&launch("b")).await.is_err());
        assert!(fake.act(&i.id, Action::Stop).await.is_err());
        assert!(
            fake.instance_types(&["t3.medium".to_owned()])
                .await
                .is_err()
        );
        assert_eq!(fake.type_calls(), 1);
    }

    #[tokio::test]
    async fn tagging_can_hang() {
        let fake = FakeEc2::default();
        let i = fake.launch(&launch("a")).await.unwrap();
        fake.hang_tagging(true);
        tokio::time::pause();
        let tagging = fake.mark_setup_delivered(&i.id);
        let hung = tokio::time::timeout(std::time::Duration::from_secs(60), tagging).await;
        assert!(hung.is_err());
        assert!(!fake.instance(&i.id).unwrap().setup_delivered);
    }

    #[tokio::test]
    async fn launch_can_be_accepted_at_once_and_answer_late() {
        let fake = FakeEc2::default();
        fake.delay_launch(Duration::from_secs(6));
        tokio::time::pause();
        let a = launch("a");
        let gave_up = tokio::time::timeout(Duration::from_secs(1), fake.launch(&a)).await;
        // The caller gave up, but EC2 had accepted the launch.
        assert!(gave_up.is_err());
        let seen: Vec<_> = fake.instances().into_iter().map(|i| i.name).collect();
        assert_eq!(seen, ["a"]);
        // Without the delay, the same call answers.
        fake.delay_launch(Duration::ZERO);
        assert!(fake.launch(&launch("b")).await.is_ok());
    }

    #[tokio::test]
    async fn list_can_miss_the_instances_that_launch_made() {
        let fake = FakeEc2::default();
        fake.seed(instance(
            "i-00000001",
            "old",
            Some("stage"),
            "running",
            "t3.medium",
        ));
        let launched = fake.launch(&launch("new")).await.unwrap();
        let names = |list: Vec<Instance>| list.into_iter().map(|i| i.name).collect::<Vec<_>>();
        assert_eq!(names(fake.list("stage").await.unwrap()), ["old", "new"]);
        // EC2 can lag right after RunInstances. A lookup by id still finds the VM.
        fake.hide_launches(true);
        assert_eq!(names(fake.list("stage").await.unwrap()), ["old"]);
        assert!(fake.get(&launched.id).await.unwrap().is_some());
        fake.hide_launches(false);
        assert_eq!(fake.list("stage").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn instance_types_follow_the_size_suffix() {
        let fake = FakeEc2::default();
        let names = ["t3a.medium".to_owned(), "t3a.large".to_owned()];
        let specs = fake.instance_types(&names).await.unwrap();
        let got: Vec<_> = specs
            .iter()
            .map(|s| (s.name.as_str(), s.vcpus, s.memory_mib))
            .collect();
        assert_eq!(got, [("t3a.medium", 2, 4096), ("t3a.large", 2, 8192)]);
    }
}
