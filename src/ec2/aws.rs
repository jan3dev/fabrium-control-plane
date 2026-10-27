//! The production `Ec2`, on the AWS SDK. The request and response logic sits in small
//! functions, so the tests need no network and no credentials.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use aws_config::{BehaviorVersion, Region};
use aws_sdk_ec2::{
    Client,
    error::{DisplayErrorContext, ProvideErrorMetadata},
    operation::run_instances::builders::RunInstancesFluentBuilder,
    primitives::DateTime,
    types::{
        self, AmdSevSnpSpecification, BlockDeviceMapping, CpuOptionsRequest, EbsBlockDevice,
        Filter, HttpTokensState, Image, InstanceMetadataEndpointState,
        InstanceMetadataOptionsRequest, InstanceType, ResourceType, Tag, TagSpecification,
        VolumeType,
    },
};
use aws_smithy_http_client::{Builder, tls};

use super::{Action, Ec2, Instance, Launch, TypeSpec, sev_snp};
use crate::config::VmConfig;

/// The states that the VM list shows. `terminated` is left out on purpose.
const LISTED_STATES: [&str; 5] = ["pending", "running", "shutting-down", "stopping", "stopped"];

pub struct AwsEc2 {
    client: Client,
    ami_name: String,
    security_group: String,
    key_name: String,
}

impl AwsEc2 {
    /// Credentials come from the default chain: `AWS_ACCESS_KEY_ID`,
    /// `AWS_SECRET_ACCESS_KEY`, and `AWS_SESSION_TOKEN` in the environment.
    pub async fn connect(vm: &VmConfig) -> Self {
        // rustls with ring, as the rest of the tree. The SDK default would add aws-lc.
        let http = Builder::new()
            .tls_provider(tls::Provider::Rustls(
                tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(vm.aws_region.clone()))
            .http_client(http)
            .load()
            .await;
        Self {
            client: Client::new(&config),
            ami_name: vm.ami_name.clone(),
            security_group: vm.security_group.clone(),
            key_name: vm.key_name.clone(),
        }
    }
}

fn filter(name: &str, values: &[&str]) -> Filter {
    let values = values.iter().map(|v| (*v).to_owned()).collect();
    Filter::builder()
        .name(name)
        .set_values(Some(values))
        .build()
}

/// An SDK error with its AWS code and message. `Display` of an `SdkError` alone is
/// only "service error", which is no use in the log.
fn aws_err<E: std::error::Error>(operation: &'static str) -> impl FnOnce(E) -> anyhow::Error {
    move |e| anyhow!("{operation}: {}", DisplayErrorContext(&e))
}

/// EC2 forgets terminated instances after about an hour, and a malformed id is no instance.
fn is_missing_id(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("InvalidInstanceID.NotFound" | "InvalidInstanceID.Malformed")
    )
}

/// `CreationDate` is ISO 8601 in UTC, so the string order is the time order.
fn newest_image(images: &[Image]) -> Option<&Image> {
    images.iter().max_by_key(|image| image.creation_date())
}

fn to_instance(i: &types::Instance) -> Instance {
    let tag = |key: &str| {
        (i.tags().iter())
            .find(|t| t.key() == Some(key))
            .and_then(|t| t.value())
    };
    Instance {
        id: i.instance_id().unwrap_or_default().to_owned(),
        name: tag("Name").unwrap_or_default().to_owned(),
        tenant: tag("fabrium:tenant").map(str::to_owned),
        state: (i.state().and_then(|s| s.name()))
            .map_or("unknown", |n| n.as_str())
            .to_owned(),
        instance_type: i.instance_type().map_or("", |t| t.as_str()).to_owned(),
        public_ip: (i.public_ip_address().filter(|ip| !ip.is_empty())).map(str::to_owned),
        // Whole seconds: EC2 sends milliseconds.
        launched_at: DateTime::from_secs(i.launch_time().map_or(0, DateTime::secs)),
        setup_delivered: tag("fabrium:setup") == Some("delivered"),
    }
}

/// The `RunInstances` request. It sets no instance profile, so the VM has no AWS role.
fn run_instances(
    client: &Client,
    launch: &Launch,
    image_id: &str,
    root_device: &str,
    security_group_id: &str,
    key_name: &str,
) -> RunInstancesFluentBuilder {
    let tags = |resource| {
        let tag = |key: &str, value: &str| Tag::builder().key(key).value(value).build();
        TagSpecification::builder()
            .resource_type(resource)
            .tags(tag("Name", &launch.name))
            .tags(tag("Project", "jan3-dev-env"))
            .tags(tag("fabrium:tenant", &launch.tenant))
            .build()
    };
    let disk = EbsBlockDevice::builder()
        .volume_size(20)
        .volume_type(VolumeType::Gp3)
        .encrypted(true)
        .build();
    let imds = InstanceMetadataOptionsRequest::builder()
        .http_tokens(HttpTokensState::Required)
        .http_endpoint(InstanceMetadataEndpointState::Enabled)
        .build();
    let request = client
        .run_instances()
        .image_id(image_id)
        .instance_type(InstanceType::from(launch.instance_type.as_str()))
        .min_count(1)
        .max_count(1)
        .key_name(key_name)
        .security_group_ids(security_group_id)
        .user_data(&launch.user_data)
        .block_device_mappings(
            BlockDeviceMapping::builder()
                .device_name(root_device)
                .ebs(disk)
                .build(),
        )
        .metadata_options(imds)
        .tag_specifications(tags(ResourceType::Instance))
        .tag_specifications(tags(ResourceType::Volume));
    if sev_snp(&launch.instance_type) {
        let sev = CpuOptionsRequest::builder().amd_sev_snp(AmdSevSnpSpecification::Enabled);
        return request.cpu_options(sev.build());
    }
    request
}

#[async_trait]
impl Ec2 for AwsEc2 {
    async fn instance_types(&self, names: &[String]) -> Result<Vec<TypeSpec>> {
        let names = names
            .iter()
            .map(|n| InstanceType::from(n.as_str()))
            .collect();
        let found = (self.client.describe_instance_types())
            .set_instance_types(Some(names))
            .into_paginator()
            .items()
            .send()
            .collect::<Result<Vec<_>, _>>()
            .await
            .map_err(aws_err("DescribeInstanceTypes"))?;
        found
            .iter()
            .map(|t| {
                Ok(TypeSpec {
                    name: (t.instance_type().context("instance type without a name"))?
                        .as_str()
                        .to_owned(),
                    vcpus: (t.v_cpu_info().and_then(|v| v.default_v_cpus()))
                        .context("instance type without vCPUs")?,
                    memory_mib: (t.memory_info().and_then(|m| m.size_in_mib()))
                        .context("instance type without memory")?,
                })
            })
            .collect()
    }

    async fn list(&self, tenant: &str) -> Result<Vec<Instance>> {
        let reservations = self
            .client
            .describe_instances()
            .filters(filter("tag:fabrium:tenant", &[tenant]))
            .filters(filter("instance-state-name", &LISTED_STATES))
            .into_paginator()
            .items()
            .send()
            .collect::<Result<Vec<_>, _>>()
            .await
            .map_err(aws_err("DescribeInstances"))?;
        Ok(reservations
            .iter()
            .flat_map(|r| r.instances())
            .map(to_instance)
            .collect())
    }

    async fn get(&self, id: &str) -> Result<Option<Instance>> {
        match self
            .client
            .describe_instances()
            .instance_ids(id)
            .send()
            .await
        {
            Ok(out) => Ok(out
                .reservations()
                .iter()
                .flat_map(|r| r.instances())
                .next()
                .map(to_instance)),
            Err(e) if is_missing_id(e.code()) => Ok(None),
            Err(e) => Err(aws_err("DescribeInstances")(e)),
        }
    }

    async fn launch(&self, launch: &Launch) -> Result<Instance> {
        let images = self
            .client
            .describe_images()
            .owners("self")
            .filters(filter("name", &[&self.ami_name]))
            .filters(filter("state", &["available"]))
            .send()
            .await
            .map_err(aws_err("DescribeImages"))?;
        let image = newest_image(images.images())
            .with_context(|| format!("no available image named {:?}", self.ami_name))?;
        let image_id = image.image_id().context("image without an id")?;
        let root_device = image
            .root_device_name()
            .context("image without a root device name")?;

        let groups = self
            .client
            .describe_security_groups()
            .filters(filter("group-name", &[&self.security_group]))
            .send()
            .await
            .map_err(aws_err("DescribeSecurityGroups"))?;
        let group_id = (groups.security_groups().first().and_then(|g| g.group_id()))
            .with_context(|| format!("no security group named {:?}", self.security_group))?;

        let out = run_instances(
            &self.client,
            launch,
            image_id,
            root_device,
            group_id,
            &self.key_name,
        )
        .send()
        .await
        .map_err(aws_err("RunInstances"))?;
        let mut instance = (out.instances().first().map(to_instance))
            .context("RunInstances returned no instance")?;
        // What we asked for is the source of truth, not the tags of the response.
        instance.name.clone_from(&launch.name);
        instance.tenant = Some(launch.tenant.clone());
        Ok(instance)
    }

    async fn act(&self, id: &str, action: Action) -> Result<()> {
        let c = &self.client;
        match action {
            Action::Start => (c.start_instances().instance_ids(id).send().await)
                .map(|_| ())
                .map_err(aws_err("StartInstances")),
            Action::Stop => (c.stop_instances().instance_ids(id).send().await)
                .map(|_| ())
                .map_err(aws_err("StopInstances")),
            Action::Reboot => (c.reboot_instances().instance_ids(id).send().await)
                .map(|_| ())
                .map_err(aws_err("RebootInstances")),
            Action::Terminate => (c.terminate_instances().instance_ids(id).send().await)
                .map(|_| ())
                .map_err(aws_err("TerminateInstances")),
        }
    }

    async fn mark_setup_delivered(&self, id: &str) -> Result<()> {
        let tag = Tag::builder()
            .key("fabrium:setup")
            .value("delivered")
            .build();
        (self
            .client
            .create_tags()
            .resources(id)
            .tags(tag)
            .send()
            .await)
            .map(|_| ())
            .map_err(aws_err("CreateTags"))
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_ec2::{
        config::Credentials,
        operation::run_instances::RunInstancesInput,
        types::{InstanceState, InstanceStateName},
    };
    use aws_smithy_http_client::test_util::{ReplayEvent, StaticReplayClient};
    use aws_smithy_types::body::SdkBody;
    use axum::http::{Request, Response};

    use super::*;

    const XMLNS: &str = "http://ec2.amazonaws.com/doc/2016-11-15/";
    const ID: &str = "i-0123456789abcdef0";

    /// An `AwsEc2` whose HTTP layer answers with `responses` in order, as (status, XML body).
    /// The second value records what the SDK sent.
    fn replay(responses: &[(u16, String)]) -> (AwsEc2, StaticReplayClient) {
        let events = responses
            .iter()
            .map(|(status, body)| {
                ReplayEvent::new(
                    Request::builder()
                        .uri("https://ec2.us-east-2.amazonaws.com/")
                        .body(SdkBody::empty())
                        .unwrap(),
                    Response::builder()
                        .status(*status)
                        .body(SdkBody::from(body.clone()))
                        .unwrap(),
                )
            })
            .collect();
        let http = StaticReplayClient::new(events);
        let client = Client::from_conf(
            aws_sdk_ec2::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .region(Region::new("us-east-2"))
                .credentials_provider(Credentials::new(
                    "AKIDEXAMPLE",
                    "secret",
                    None,
                    None,
                    "test",
                ))
                .http_client(http.clone())
                .build(),
        );
        let ec2 = AwsEc2 {
            client,
            ami_name: "ubuntu-test".into(),
            security_group: "test-sg".into(),
            key_name: "test-key".into(),
        };
        (ec2, http)
    }

    /// The request bodies (EC2 uses form encoding) that the SDK sent, in order.
    fn sent(http: &StaticReplayClient) -> Vec<String> {
        http.actual_requests()
            .map(|r| String::from_utf8(r.body().bytes().unwrap().to_vec()).unwrap())
            .collect()
    }

    /// True if the form body has exactly this `key=value` parameter. A substring test
    /// would also accept `Filter.1.Name=tag%3Afabrium%3Atenant-and-more`.
    fn has(body: &str, param: &str) -> bool {
        body.split('&').any(|p| p == param)
    }

    fn ok(root: &str, inner: &str) -> (u16, String) {
        let body = format!(r#"<{root} xmlns="{XMLNS}"><requestId>req</requestId>{inner}</{root}>"#);
        (200, body)
    }

    fn error(status: u16, code: &str) -> (u16, String) {
        let body = format!(
            "<Response><Errors><Error><Code>{code}</Code><Message>{code} happened</Message>\
             </Error></Errors><RequestID>req</RequestID></Response>"
        );
        (status, body)
    }

    fn instance_xml(id: &str, state: &str, tags: &[(&str, &str)]) -> String {
        let tags: String = tags
            .iter()
            .map(|(k, v)| format!("<item><key>{k}</key><value>{v}</value></item>"))
            .collect();
        format!(
            "<item><instanceId>{id}</instanceId><instanceState><code>0</code><name>{state}</name>\
             </instanceState><instanceType>t3a.medium</instanceType>\
             <launchTime>2026-10-26T19:00:00.000Z</launchTime><ipAddress>203.0.113.10</ipAddress>\
             <tagSet>{tags}</tagSet></item>"
        )
    }

    fn instances_response(items: &[String]) -> (u16, String) {
        let inner = format!(
            "<reservationSet><item><reservationId>r-1</reservationId><instancesSet>{}\
             </instancesSet></item></reservationSet>",
            items.concat()
        );
        ok("DescribeInstancesResponse", &inner)
    }

    fn launch(instance_type: &str) -> Launch {
        Launch {
            name: "lima".into(),
            tenant: "stage".into(),
            instance_type: instance_type.into(),
            user_data: "dXNlcg==".into(),
        }
    }

    fn request(instance_type: &str) -> RunInstancesInput {
        let (ec2, _) = replay(&[]);
        let builder = run_instances(
            &ec2.client,
            &launch(instance_type),
            "ami-1",
            "/dev/xvda",
            "sg-1",
            "fabrium-vms",
        );
        builder.as_input().clone().build().unwrap()
    }

    fn image(id: &str, created: &str) -> Image {
        Image::builder().image_id(id).creation_date(created).build()
    }

    #[test]
    fn run_instances_request_follows_the_launch_spec() {
        let r = request("t3a.medium");
        assert_eq!(r.image_id(), Some("ami-1"));
        assert_eq!(r.instance_type().map(|t| t.as_str()), Some("t3a.medium"));
        assert_eq!(r.key_name(), Some("fabrium-vms"));
        assert_eq!(r.security_group_ids(), ["sg-1"]);
        assert_eq!((r.min_count(), r.max_count()), (Some(1), Some(1)));
        assert_eq!(r.user_data(), Some("dXNlcg=="));
        // The root device name comes from the image, not from the code.
        let [disk] = r.block_device_mappings() else {
            panic!("expected one block device mapping")
        };
        assert_eq!(disk.device_name(), Some("/dev/xvda"));
        let ebs = disk.ebs().unwrap();
        assert_eq!((ebs.volume_size(), ebs.encrypted()), (Some(20), Some(true)));
        assert_eq!(ebs.volume_type(), Some(&VolumeType::Gp3));
        let imds = r.metadata_options().unwrap();
        assert_eq!(imds.http_tokens(), Some(&HttpTokensState::Required));
        assert_eq!(
            imds.http_endpoint(),
            Some(&InstanceMetadataEndpointState::Enabled)
        );
        assert!(r.iam_instance_profile().is_none());
        let specs = r.tag_specifications();
        let kinds: Vec<_> = specs.iter().map(|s| s.resource_type().cloned()).collect();
        assert_eq!(
            kinds,
            [Some(ResourceType::Instance), Some(ResourceType::Volume)]
        );
        for spec in specs {
            let tags: Vec<_> = spec
                .tags()
                .iter()
                .map(|t| (t.key().unwrap(), t.value().unwrap()))
                .collect();
            let want = [
                ("Name", "lima"),
                ("Project", "jan3-dev-env"),
                ("fabrium:tenant", "stage"),
            ];
            assert_eq!(tags, want);
        }
    }

    #[test]
    fn only_sev_snp_types_get_sev_snp() {
        let cases = [
            ("t3a.medium", false),
            ("t3.large", false),
            ("c6a.large", true),
            ("m6a.large", true),
            ("r6a.large", true),
            ("m6a.12xlarge", false),
            ("c6a.metal", false),
        ];
        for (instance_type, sev) in cases {
            let cpu = request(instance_type)
                .cpu_options()
                .and_then(|c| c.amd_sev_snp().cloned());
            assert_eq!(
                cpu,
                sev.then_some(AmdSevSnpSpecification::Enabled),
                "{instance_type}"
            );
        }
    }

    #[test]
    fn newest_image_has_the_latest_creation_date() {
        let images = [
            image("old", "2026-10-18T10:00:00.000Z"),
            image("new", "2026-10-20T17:26:52.000Z"),
            image("mid", "2026-10-19T08:00:00.000Z"),
        ];
        assert_eq!(
            newest_image(&images).and_then(|i| i.image_id()),
            Some("new")
        );
        assert!(newest_image(&[]).is_none());
    }

    #[test]
    fn instance_conversion_reads_tags_state_ip_and_launch_time() {
        let tag = |k: &str, v: &str| Tag::builder().key(k).value(v).build();
        let full = types::Instance::builder()
            .instance_id(ID)
            .state(
                InstanceState::builder()
                    .name(InstanceStateName::ShuttingDown)
                    .build(),
            )
            .instance_type(InstanceType::T3aMedium)
            .public_ip_address("203.0.113.10")
            .launch_time(DateTime::from_secs_and_nanos(1_793_041_200, 123_000_000))
            .tags(tag("Name", "lima"))
            .tags(tag("fabrium:tenant", "stage"))
            .tags(tag("fabrium:setup", "delivered"))
            .build();
        let i = to_instance(&full);
        assert_eq!(i.id, ID);
        assert_eq!(i.name, "lima");
        assert_eq!(i.state, "shutting-down");
        assert_eq!(i.instance_type, "t3a.medium");
        assert_eq!(i.tenant.as_deref(), Some("stage"));
        assert_eq!(i.public_ip.as_deref(), Some("203.0.113.10"));
        assert!(i.setup_delivered);
        // Whole seconds only.
        assert_eq!(i.launched_at, DateTime::from_secs(1_793_041_200));

        let other = tag("fabrium:setup", "other");
        let bare = to_instance(
            &types::Instance::builder()
                .instance_id("i-1")
                .tags(other)
                .build(),
        );
        assert_eq!(bare.name, "");
        assert_eq!(
            (bare.tenant, bare.public_ip, bare.setup_delivered),
            (None, None, false)
        );
    }

    #[test]
    fn unknown_instance_ids_are_not_errors() {
        assert!(is_missing_id(Some("InvalidInstanceID.NotFound")));
        assert!(is_missing_id(Some("InvalidInstanceID.Malformed")));
        assert!(!is_missing_id(Some("UnauthorizedOperation")));
        assert!(!is_missing_id(None));
    }

    #[tokio::test]
    async fn list_asks_for_the_tenants_live_instances() {
        let items = [
            instance_xml(
                ID,
                "running",
                &[("Name", "lima"), ("fabrium:tenant", "stage")],
            ),
            instance_xml("i-0123456789abcdef1", "stopped", &[]),
        ];
        let (ec2, http) = replay(&[instances_response(&items)]);
        let got = ec2.list("stage").await.unwrap();
        let got: Vec<_> = got
            .iter()
            .map(|i| (i.id.as_str(), i.name.as_str(), i.tenant.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                (ID, "lima", Some("stage")),
                ("i-0123456789abcdef1", "", None)
            ]
        );
        let bodies = sent(&http);
        let [body] = &bodies[..] else {
            panic!("expected one request")
        };
        let wants = [
            "Action=DescribeInstances",
            "Filter.1.Name=tag%3Afabrium%3Atenant",
            "Filter.1.Value.1=stage",
            "Filter.2.Name=instance-state-name",
            "Filter.2.Value.1=pending",
            "Filter.2.Value.2=running",
            "Filter.2.Value.3=shutting-down",
            "Filter.2.Value.4=stopping",
            "Filter.2.Value.5=stopped",
        ];
        for want in wants {
            assert!(has(body, want), "{want}: {body}");
        }
        assert!(!body.contains("terminated"), "{body}");
    }

    #[tokio::test]
    async fn get_returns_the_instance_none_for_a_missing_id_and_an_error_otherwise() {
        let (ec2, http) = replay(&[
            instances_response(&[instance_xml(ID, "running", &[])]),
            error(400, "InvalidInstanceID.NotFound"),
            error(403, "UnauthorizedOperation"),
        ]);
        assert_eq!(ec2.get(ID).await.unwrap().unwrap().state, "running");
        assert!(ec2.get("i-0123456789abcdef1").await.unwrap().is_none());
        let err = ec2
            .get("i-0123456789abcdef2")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("UnauthorizedOperation"), "{err}");
        assert!(has(&sent(&http)[0], &format!("InstanceId.1={ID}")));
    }

    #[tokio::test]
    async fn launch_resolves_the_image_and_group_then_runs_the_instance() {
        let images = ok(
            "DescribeImagesResponse",
            "<imagesSet>\
             <item><imageId>ami-00000000000000001</imageId><creationDate>2026-10-18T10:00:00.000Z</creationDate>\
             <rootDeviceName>/dev/sda1</rootDeviceName></item>\
             <item><imageId>ami-00000000000000002</imageId><creationDate>2026-10-20T17:26:52.000Z</creationDate>\
             <rootDeviceName>/dev/sda1</rootDeviceName></item></imagesSet>",
        );
        let groups = ok(
            "DescribeSecurityGroupsResponse",
            "<securityGroupInfo><item><groupId>sg-0123456789abcdef0</groupId></item></securityGroupInfo>",
        );
        let run = ok(
            "RunInstancesResponse",
            &format!(
                "<reservationId>r-1</reservationId><instancesSet>{}</instancesSet>",
                instance_xml("i-0aaaaaaaaaaaaaaaa", "pending", &[])
            ),
        );
        let (ec2, http) = replay(&[images, groups, run]);
        let i = ec2.launch(&launch("m6a.large")).await.unwrap();
        assert_eq!(
            (i.id.as_str(), i.state.as_str()),
            ("i-0aaaaaaaaaaaaaaaa", "pending")
        );
        // The launch request is the source of name and tenant, not the response tags.
        assert_eq!(
            (i.name.as_str(), i.tenant.as_deref()),
            ("lima", Some("stage"))
        );

        let bodies = sent(&http);
        let [images, groups, run] = &bodies[..] else {
            panic!("expected three requests")
        };
        let wants = [
            "Action=DescribeImages",
            "Owner.1=self",
            "Filter.1.Name=name",
            "Filter.1.Value.1=ubuntu-test",
            "Filter.2.Name=state",
            "Filter.2.Value.1=available",
        ];
        for want in wants {
            assert!(has(images, want), "{want}: {images}");
        }
        let wants = [
            "Action=DescribeSecurityGroups",
            "Filter.1.Name=group-name",
            "Filter.1.Value.1=test-sg",
        ];
        for want in wants {
            assert!(has(groups, want), "{want}: {groups}");
        }
        // The newest image, its root device, and the resolved group id reach RunInstances.
        let wants = [
            "Action=RunInstances",
            "ImageId=ami-00000000000000002",
            "SecurityGroupId.1=sg-0123456789abcdef0",
            "KeyName=test-key",
            "BlockDeviceMapping.1.DeviceName=%2Fdev%2Fsda1",
            "CpuOptions.AmdSevSnp=enabled",
        ];
        for want in wants {
            assert!(has(run, want), "{want}: {run}");
        }
    }

    #[tokio::test]
    async fn launch_stops_before_run_instances_when_the_image_or_group_is_missing() {
        let (ec2, http) = replay(&[ok("DescribeImagesResponse", "<imagesSet/>")]);
        let err = ec2
            .launch(&launch("t3a.medium"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ubuntu-test"), "{err}");
        assert_eq!(sent(&http).len(), 1);

        let images = ok(
            "DescribeImagesResponse",
            "<imagesSet><item><imageId>ami-1</imageId><creationDate>2026-10-20T17:26:52.000Z</creationDate>\
             <rootDeviceName>/dev/sda1</rootDeviceName></item></imagesSet>",
        );
        let none = ok("DescribeSecurityGroupsResponse", "<securityGroupInfo/>");
        let (ec2, http) = replay(&[images, none]);
        let err = ec2
            .launch(&launch("t3a.medium"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("test-sg"), "{err}");
        assert_eq!(sent(&http).len(), 2);
    }

    #[tokio::test]
    async fn actions_call_the_matching_api() {
        let table = [
            (Action::Start, "StartInstances"),
            (Action::Stop, "StopInstances"),
            (Action::Reboot, "RebootInstances"),
            (Action::Terminate, "TerminateInstances"),
        ];
        for (action, api) in table {
            let (ec2, http) = replay(&[ok(&format!("{api}Response"), "<instancesSet/>")]);
            ec2.act(ID, action).await.unwrap();
            let bodies = sent(&http);
            let [body] = &bodies[..] else {
                panic!("expected one request for {api}")
            };
            assert!(has(body, &format!("Action={api}")), "{api}: {body}");
            assert!(has(body, &format!("InstanceId.1={ID}")), "{api}: {body}");
        }
    }

    #[tokio::test]
    async fn mark_setup_delivered_sets_the_tag() {
        let (ec2, http) = replay(&[ok("CreateTagsResponse", "<return>true</return>")]);
        ec2.mark_setup_delivered(ID).await.unwrap();
        let bodies = sent(&http);
        let [body] = &bodies[..] else {
            panic!("expected one request")
        };
        for want in [
            "Action=CreateTags",
            &format!("ResourceId.1={ID}"),
            "Tag.1.Key=fabrium%3Asetup",
            "Tag.1.Value=delivered",
        ] {
            assert!(has(body, want), "{want}: {body}");
        }
    }

    #[tokio::test]
    async fn instance_types_read_vcpus_and_memory() {
        let item = |name: &str, mib: u32| {
            format!(
                "<item><instanceType>{name}</instanceType><vCpuInfo><defaultVCpus>2</defaultVCpus>\
                 </vCpuInfo><memoryInfo><sizeInMiB>{mib}</sizeInMiB></memoryInfo></item>"
            )
        };
        let set = format!(
            "<instanceTypeSet>{}{}</instanceTypeSet>",
            item("t3a.large", 8192),
            item("t3a.medium", 4096)
        );
        let (ec2, http) = replay(&[ok("DescribeInstanceTypesResponse", &set)]);
        let names = ["t3a.medium".to_owned(), "t3a.large".to_owned()];
        let specs = ec2.instance_types(&names).await.unwrap();
        let got: Vec<_> = specs
            .iter()
            .map(|s| (s.name.as_str(), s.vcpus, s.memory_mib))
            .collect();
        assert_eq!(got, [("t3a.large", 2, 8192), ("t3a.medium", 2, 4096)]);
        let body = &sent(&http)[0];
        assert!(has(body, "InstanceType.1=t3a.medium"), "{body}");
        assert!(has(body, "InstanceType.2=t3a.large"), "{body}");
    }

    #[tokio::test]
    async fn sdk_errors_keep_the_aws_code_and_message_for_the_log() {
        let (ec2, _) = replay(&[error(403, "RequestExpired")]);
        let err = ec2.list("stage").await.unwrap_err().to_string();
        assert!(err.starts_with("DescribeInstances: "), "{err}");
        assert!(err.contains("RequestExpired happened"), "{err}");
    }
}
