//! [`AwsApi`] on the official AWS SDK. Every call stays inside what the
//! `rega-claude` IAM policy allows (`aws-setup/rega-claude-policy.json`).

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::ProvideCredentials;
use aws_sdk_ec2::client::Waiters;
use aws_sdk_ec2::types::{Filter, InstanceLifecycleType, InstanceStateName, LocationType, Tag};
use aws_sdk_s3::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{
    AbortIncompleteMultipartUpload, BucketLifecycleConfiguration, BucketLocationConstraint,
    BucketVersioningStatus, CreateBucketConfiguration, ExpirationStatus, LifecycleExpiration,
    LifecycleRule, LifecycleRuleFilter, NoncurrentVersionExpiration, ServerSideEncryption,
    ServerSideEncryptionByDefault, ServerSideEncryptionConfiguration, ServerSideEncryptionRule,
    Tagging, VersioningConfiguration,
};

use super::api::{AwsApi, BucketInfo, Instance};
use super::{pricing, HOME_REGION};
use crate::model::{MachineState, Orphan, ResourceKind};
use crate::provider::{ProviderError, ProviderResult};

/// Service Quotas codes for EC2's G and VT families, in vCPUs.
pub const GPU_ON_DEMAND_QUOTA: &str = "L-DB2E81BA";
pub const GPU_SPOT_QUOTA: &str = "L-3819A6DF";

/// The Price List API answers in a few regions only.
const PRICING_REGION: &str = "us-east-1";

/// How long a terminated instance may take to be gone.
const TERMINATE_WAIT: Duration = Duration::from_secs(600);

/// How long the state bucket keeps old versions of an object, such as the
/// state and key of a destroyed machine.
const OLD_VERSION_DAYS: i32 = 30;

const PROJECT_TAG: (&str, &str) = ("Project", "ReGa");
const MACHINE_TAG: &str = "ReGaMachine";

fn fail<E, R>(what: &str, err: SdkError<E, R>) -> ProviderError
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
    R: std::fmt::Debug + Send + Sync + 'static,
{
    let detail = match (err.code(), err.message()) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.to_string(),
        _ => DisplayErrorContext(&err).to_string(),
    };
    ProviderError::Cloud(format!("{what} failed: {detail}"))
}

fn tag_filter() -> Filter {
    Filter::builder()
        .name(format!("tag:{}", PROJECT_TAG.0))
        .values(PROJECT_TAG.1)
        .build()
}

fn tag_value<'a>(tags: &'a [Tag], key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|t| t.key() == Some(key))
        .and_then(Tag::value)
}

fn instance_state(name: Option<&InstanceStateName>) -> MachineState {
    match name {
        Some(InstanceStateName::Pending) => MachineState::Pending,
        Some(InstanceStateName::Running) => MachineState::Running,
        Some(InstanceStateName::Stopping) | Some(InstanceStateName::ShuttingDown) => {
            MachineState::Stopping
        }
        Some(InstanceStateName::Stopped) => MachineState::Stopped,
        _ => MachineState::Terminated,
    }
}

pub struct SdkApi {
    config: SdkConfig,
}

impl SdkApi {
    /// Loads credentials from the default chain and reads the account id.
    pub async fn connect() -> ProviderResult<(Self, String)> {
        let region = aws_config::meta::region::RegionProviderChain::default_provider()
            .or_else(Region::new(HOME_REGION));
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(region)
            .timeout_config(
                aws_config::timeout::TimeoutConfig::builder()
                    .operation_timeout(Duration::from_secs(60))
                    .connect_timeout(Duration::from_secs(10))
                    .build(),
            )
            .load()
            .await;
        let provider = config
            .credentials_provider()
            .ok_or_else(|| ProviderError::MissingCredentials("AWS".into()))?;
        match provider.provide_credentials().await {
            Ok(_) => {}
            Err(CredentialsError::CredentialsNotLoaded(_)) => {
                return Err(ProviderError::MissingCredentials("AWS".into()))
            }
            Err(e) => {
                return Err(ProviderError::Cloud(format!(
                    "AWS credentials: {}",
                    DisplayErrorContext(&e)
                )))
            }
        }
        let identity = aws_sdk_sts::Client::new(&config)
            .get_caller_identity()
            .send()
            .await
            .map_err(|e| fail("checking the AWS credentials", e))?;
        let account = identity
            .account()
            .ok_or_else(|| ProviderError::Cloud("AWS didn't say which account this is".into()))?
            .to_string();
        Ok((Self { config }, account))
    }

    fn ec2(&self, region: &str) -> aws_sdk_ec2::Client {
        aws_sdk_ec2::Client::from_conf(
            aws_sdk_ec2::config::Builder::from(&self.config)
                .region(Region::new(region.to_string()))
                .build(),
        )
    }

    fn cloudwatch(&self, region: &str) -> aws_sdk_cloudwatch::Client {
        aws_sdk_cloudwatch::Client::from_conf(
            aws_sdk_cloudwatch::config::Builder::from(&self.config)
                .region(Region::new(region.to_string()))
                .build(),
        )
    }

    fn quotas(&self, region: &str) -> aws_sdk_servicequotas::Client {
        aws_sdk_servicequotas::Client::from_conf(
            aws_sdk_servicequotas::config::Builder::from(&self.config)
                .region(Region::new(region.to_string()))
                .build(),
        )
    }

    fn pricing(&self) -> aws_sdk_pricing::Client {
        aws_sdk_pricing::Client::from_conf(
            aws_sdk_pricing::config::Builder::from(&self.config)
                .region(Region::new(PRICING_REGION))
                .build(),
        )
    }

    /// Both buckets live in the home region.
    fn s3(&self) -> aws_sdk_s3::Client {
        aws_sdk_s3::Client::from_conf(
            aws_sdk_s3::config::Builder::from(&self.config)
                .region(Region::new(HOME_REGION))
                .build(),
        )
    }

    /// Pending or running G and VT instances: (spot, vCPUs) each.
    async fn gpu_instances(&self, region: &str) -> ProviderResult<Vec<(bool, u32)>> {
        let mut pages = self
            .ec2(region)
            .describe_instances()
            .filters(
                Filter::builder()
                    .name("instance-state-name")
                    .values("pending")
                    .values("running")
                    .build(),
            )
            .filters(
                Filter::builder()
                    .name("instance-type")
                    .values("g*")
                    .values("vt*")
                    .build(),
            )
            .into_paginator()
            .send();
        let mut out = Vec::new();
        while let Some(page) = pages.next().await {
            let page = page.map_err(|e| fail("listing GPU instances", e))?;
            for i in page.reservations().iter().flat_map(|r| r.instances()) {
                let spot = i.instance_lifecycle() == Some(&InstanceLifecycleType::Spot);
                let vcpus = i
                    .cpu_options()
                    .map(|c| c.core_count().unwrap_or(0) * c.threads_per_core().unwrap_or(1))
                    .unwrap_or(0);
                out.push((spot, vcpus.max(0) as u32));
            }
        }
        Ok(out)
    }

    async fn tagged_alarms(&self, region: &str) -> ProviderResult<Vec<Orphan>> {
        let cw = self.cloudwatch(region);
        let mut pages = cw
            .describe_alarms()
            .alarm_name_prefix("rega-")
            .into_paginator()
            .send();
        let mut out = Vec::new();
        while let Some(page) = pages.next().await {
            let page = page.map_err(|e| fail("listing alarms", e))?;
            for alarm in page.metric_alarms() {
                let (Some(name), Some(arn)) = (alarm.alarm_name(), alarm.alarm_arn()) else {
                    continue;
                };
                let tags = cw
                    .list_tags_for_resource()
                    .resource_arn(arn)
                    .send()
                    .await
                    .map_err(|e| fail("reading alarm tags", e))?;
                let tag = |key: &str| {
                    tags.tags()
                        .iter()
                        .find(|t| t.key() == Some(key))
                        .and_then(|t| t.value())
                };
                if tag(PROJECT_TAG.0) != Some(PROJECT_TAG.1) {
                    continue;
                }
                out.push(Orphan {
                    kind: ResourceKind::Alarm,
                    id: name.into(),
                    region_id: region.into(),
                    machine_id: tag(MACHINE_TAG).map(String::from),
                    name: Some(name.into()),
                });
            }
        }
        Ok(out)
    }

    async fn bucket_flags(
        &self,
        s3: &aws_sdk_s3::Client,
        name: &str,
    ) -> ProviderResult<BucketInfo> {
        let versioned = s3
            .get_bucket_versioning()
            .bucket(name)
            .send()
            .await
            .map_err(|e| fail("reading bucket versioning", e))?
            .status()
            == Some(&BucketVersioningStatus::Enabled);
        let encrypted = match s3.get_bucket_encryption().bucket(name).send().await {
            Ok(out) => out
                .server_side_encryption_configuration()
                .is_some_and(|c| !c.rules().is_empty()),
            Err(e) if e.code() == Some("ServerSideEncryptionConfigurationNotFoundError") => false,
            Err(e) => return Err(fail("reading bucket encryption", e)),
        };
        let public_access_blocked = match s3.get_public_access_block().bucket(name).send().await {
            Ok(out) => out.public_access_block_configuration().is_some_and(|c| {
                c.block_public_acls() == Some(true)
                    && c.ignore_public_acls() == Some(true)
                    && c.block_public_policy() == Some(true)
                    && c.restrict_public_buckets() == Some(true)
            }),
            Err(e) if e.code() == Some("NoSuchPublicAccessBlockConfiguration") => false,
            Err(e) => return Err(fail("reading bucket public access", e)),
        };
        let tagged = self
            .bucket_tags(s3, name)
            .await?
            .iter()
            .any(|t| t.key() == PROJECT_TAG.0 && t.value() == PROJECT_TAG.1);
        let lifecycle = match s3
            .get_bucket_lifecycle_configuration()
            .bucket(name)
            .send()
            .await
        {
            Ok(out) => !out.rules().is_empty(),
            Err(e) if e.code() == Some("NoSuchLifecycleConfiguration") => false,
            Err(e) => return Err(fail("reading bucket lifecycle", e)),
        };
        Ok(BucketInfo {
            versioned,
            encrypted,
            public_access_blocked,
            tagged,
            lifecycle,
        })
    }

    async fn bucket_tags(
        &self,
        s3: &aws_sdk_s3::Client,
        name: &str,
    ) -> ProviderResult<Vec<aws_sdk_s3::types::Tag>> {
        match s3.get_bucket_tagging().bucket(name).send().await {
            Ok(out) => Ok(out.tag_set().to_vec()),
            Err(e) if e.code() == Some("NoSuchTagSet") => Ok(vec![]),
            Err(e) => Err(fail("reading bucket tags", e)),
        }
    }
}

fn is_missing_bucket(code: Option<&str>) -> bool {
    matches!(code, Some("NoSuchBucket") | Some("NotFound"))
}

#[async_trait]
impl AwsApi for SdkApi {
    async fn regions(&self) -> ProviderResult<Vec<String>> {
        let out = self
            .ec2(HOME_REGION)
            .describe_regions()
            .send()
            .await
            .map_err(|e| fail("listing regions", e))?;
        Ok(out
            .regions()
            .iter()
            .filter_map(|r| r.region_name().map(String::from))
            .collect())
    }

    async fn offered_types(&self, region: &str, types: &[String]) -> ProviderResult<Vec<String>> {
        let out = self
            .ec2(region)
            .describe_instance_type_offerings()
            .location_type(LocationType::Region)
            .filters(
                Filter::builder()
                    .name("instance-type")
                    .set_values(Some(types.to_vec()))
                    .build(),
            )
            .send()
            .await
            .map_err(|e| fail("listing instance types", e))?;
        Ok(out
            .instance_type_offerings()
            .iter()
            .filter_map(|o| o.instance_type().map(|t| t.as_str().to_string()))
            .collect())
    }

    async fn offered_zones(&self, region: &str, type_name: &str) -> ProviderResult<Vec<String>> {
        let out = self
            .ec2(region)
            .describe_instance_type_offerings()
            .location_type(LocationType::AvailabilityZone)
            .filters(
                Filter::builder()
                    .name("instance-type")
                    .values(type_name)
                    .build(),
            )
            .send()
            .await
            .map_err(|e| fail("listing availability zones", e))?;
        let mut zones: Vec<String> = out
            .instance_type_offerings()
            .iter()
            .filter_map(|o| o.location().map(String::from))
            .collect();
        zones.sort();
        Ok(zones)
    }

    async fn gpu_quota(&self, region: &str, spot: bool) -> ProviderResult<u32> {
        let code = if spot {
            GPU_SPOT_QUOTA
        } else {
            GPU_ON_DEMAND_QUOTA
        };
        match self
            .quotas(region)
            .get_service_quota()
            .service_code("ec2")
            .quota_code(code)
            .send()
            .await
        {
            Ok(out) => Ok(out
                .quota()
                .and_then(|q| q.value())
                .map_or(0, |v| v.max(0.0) as u32)),
            // A quota that was never set in a region reads as missing.
            Err(e) if e.code() == Some("NoSuchResourceException") => Ok(0),
            Err(e) => Err(fail("reading the GPU quota", e)),
        }
    }

    async fn gpu_vcpus_in_use(&self, region: &str, spot: bool) -> ProviderResult<u32> {
        Ok(self
            .gpu_instances(region)
            .await?
            .into_iter()
            .filter(|(is_spot, _)| *is_spot == spot)
            .map(|(_, vcpus)| vcpus)
            .sum())
    }

    async fn on_demand_price(&self, region: &str, type_name: &str) -> ProviderResult<Option<f64>> {
        let filters = pricing::product_filters(region, type_name)
            .into_iter()
            .map(|(field, value)| {
                aws_sdk_pricing::types::Filter::builder()
                    .r#type(aws_sdk_pricing::types::FilterType::TermMatch)
                    .field(field)
                    .value(value)
                    .build()
                    .expect("type, field and value are set")
            })
            .collect();
        let out = self
            .pricing()
            .get_products()
            .service_code("AmazonEC2")
            .set_filters(Some(filters))
            .max_results(10)
            .send()
            .await
            .map_err(|e| fail("reading prices", e))?;
        pricing::hourly_on_demand(out.price_list()).map_err(ProviderError::Cloud)
    }

    async fn spot_prices(
        &self,
        region: &str,
        type_name: &str,
    ) -> ProviderResult<Vec<(String, f64)>> {
        // With a start time of now, AWS returns the current price per zone.
        let out = self
            .ec2(region)
            .describe_spot_price_history()
            .instance_types(type_name.into())
            .product_descriptions("Windows")
            .start_time(SystemTime::now().into())
            .send()
            .await
            .map_err(|e| fail("reading spot prices", e))?;
        let mut latest: HashMap<String, f64> = HashMap::new();
        for p in out.spot_price_history() {
            let (Some(zone), Some(price)) = (p.availability_zone(), p.spot_price()) else {
                continue;
            };
            if let Ok(price) = price.parse::<f64>() {
                latest.entry(zone.into()).or_insert(price);
            }
        }
        Ok(latest.into_iter().collect())
    }

    async fn tagged_instances(&self, region: &str) -> ProviderResult<Vec<Instance>> {
        let mut pages = self
            .ec2(region)
            .describe_instances()
            .filters(tag_filter())
            .into_paginator()
            .send();
        let mut out = Vec::new();
        while let Some(page) = pages.next().await {
            let page = page.map_err(|e| fail("listing machines", e))?;
            for i in page.reservations().iter().flat_map(|r| r.instances()) {
                let state = instance_state(i.state().and_then(|s| s.name()));
                let Some(id) = i.instance_id() else { continue };
                if state == MachineState::Terminated {
                    continue;
                }
                out.push(Instance {
                    id: id.into(),
                    region_id: region.into(),
                    machine_id: tag_value(i.tags(), MACHINE_TAG).map(String::from),
                    type_name: i
                        .instance_type()
                        .map(|t| t.as_str().to_string())
                        .unwrap_or_default(),
                    state,
                    spot: i.instance_lifecycle() == Some(&InstanceLifecycleType::Spot),
                    public_ip: i.public_ip_address().map(String::from),
                });
            }
        }
        Ok(out)
    }

    async fn password_data(
        &self,
        region: &str,
        instance_id: &str,
    ) -> ProviderResult<Option<String>> {
        let out = self
            .ec2(region)
            .get_password_data()
            .instance_id(instance_id)
            .send()
            .await
            .map_err(|e| fail("reading the Windows password", e))?;
        Ok(out
            .password_data()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(String::from))
    }

    async fn tagged_resources(&self, region: &str) -> ProviderResult<Vec<Orphan>> {
        let ec2 = self.ec2(region);
        let resource = |kind, id: &str, tags: &[Tag]| Orphan {
            kind,
            id: id.into(),
            region_id: region.into(),
            machine_id: tag_value(tags, MACHINE_TAG).map(String::from),
            name: tag_value(tags, "Name").map(String::from),
        };
        let mut out: Vec<Orphan> = self
            .tagged_instances(region)
            .await?
            .into_iter()
            .map(|i| Orphan {
                kind: ResourceKind::Instance,
                id: i.id,
                region_id: i.region_id,
                machine_id: i.machine_id,
                name: None,
            })
            .collect();

        let mut volumes = ec2
            .describe_volumes()
            .filters(tag_filter())
            .into_paginator()
            .send();
        while let Some(page) = volumes.next().await {
            let page = page.map_err(|e| fail("listing volumes", e))?;
            for v in page.volumes() {
                if let Some(id) = v.volume_id() {
                    out.push(resource(ResourceKind::Volume, id, v.tags()));
                }
            }
        }

        let mut groups = ec2
            .describe_security_groups()
            .filters(tag_filter())
            .into_paginator()
            .send();
        while let Some(page) = groups.next().await {
            let page = page.map_err(|e| fail("listing security groups", e))?;
            for g in page.security_groups() {
                if let Some(id) = g.group_id() {
                    let mut o = resource(ResourceKind::SecurityGroup, id, g.tags());
                    o.name = o.name.or_else(|| g.group_name().map(String::from));
                    out.push(o);
                }
            }
        }

        let keys = ec2
            .describe_key_pairs()
            .filters(tag_filter())
            .send()
            .await
            .map_err(|e| fail("listing key pairs", e))?;
        for k in keys.key_pairs() {
            if let Some(name) = k.key_name() {
                let mut o = resource(ResourceKind::KeyPair, name, k.tags());
                o.name = Some(name.into());
                out.push(o);
            }
        }

        out.extend(self.tagged_alarms(region).await?);
        // The instance name lives in its Name tag, which tagged_instances
        // doesn't keep; fill it from the machine id for display.
        for o in out.iter_mut().filter(|o| o.name.is_none()) {
            o.name = o.machine_id.as_ref().map(|m| format!("rega-{m}"));
        }
        Ok(out)
    }

    async fn delete_resource(&self, resource: &Orphan) -> ProviderResult<()> {
        let region = resource.region_id.as_str();
        let id = resource.id.as_str();
        let ec2 = self.ec2(region);
        // Something already gone counts as deleted.
        let gone = |code: Option<&str>| code.is_some_and(|c| c.ends_with(".NotFound"));
        match resource.kind {
            ResourceKind::Instance => {
                match ec2.terminate_instances().instance_ids(id).send().await {
                    Ok(_) => {}
                    Err(e) if gone(e.code()) => return Ok(()),
                    Err(e) => return Err(fail("terminating the instance", e)),
                }
                ec2.wait_until_instance_terminated()
                    .instance_ids(id)
                    .wait(TERMINATE_WAIT)
                    .await
                    .map_err(|e| {
                        ProviderError::Cloud(format!(
                            "waiting for {id} to terminate failed: {}",
                            DisplayErrorContext(&e)
                        ))
                    })?;
            }
            ResourceKind::Volume => match ec2.delete_volume().volume_id(id).send().await {
                Err(e) if !gone(e.code()) => return Err(fail("deleting the volume", e)),
                _ => {}
            },
            ResourceKind::SecurityGroup => {
                match ec2.delete_security_group().group_id(id).send().await {
                    Err(e) if !gone(e.code()) => {
                        return Err(fail("deleting the security group", e))
                    }
                    _ => {}
                }
            }
            ResourceKind::KeyPair => {
                // DeleteKeyPair succeeds for missing keys.
                ec2.delete_key_pair()
                    .key_name(id)
                    .send()
                    .await
                    .map_err(|e| fail("deleting the key pair", e))?;
            }
            ResourceKind::Alarm => {
                self.cloudwatch(region)
                    .delete_alarms()
                    .alarm_names(id)
                    .send()
                    .await
                    .map_err(|e| fail("deleting the alarm", e))?;
            }
        }
        Ok(())
    }

    async fn bucket(&self, name: &str) -> ProviderResult<Option<BucketInfo>> {
        let s3 = self.s3();
        match s3.head_bucket().bucket(name).send().await {
            Ok(_) => Ok(Some(self.bucket_flags(&s3, name).await?)),
            Err(e) if is_missing_bucket(e.code()) => Ok(None),
            Err(e) if e.raw_response().map(|r| r.status().as_u16()) == Some(403) => {
                Err(ProviderError::Cloud(format!(
                    "bucket {name} belongs to someone else or can't be read with these credentials"
                )))
            }
            Err(e) => Err(fail("checking the bucket", e)),
        }
    }

    async fn create_bucket(&self, name: &str, region: &str) -> ProviderResult<()> {
        let mut request = self.s3().create_bucket().bucket(name);
        // us-east-1 is the one region that takes no location constraint.
        if region != "us-east-1" {
            request = request.create_bucket_configuration(
                CreateBucketConfiguration::builder()
                    .location_constraint(BucketLocationConstraint::from(region))
                    .build(),
            );
        }
        match request.send().await {
            Ok(_) => Ok(()),
            Err(e) if e.code() == Some("BucketAlreadyOwnedByYou") => Ok(()),
            Err(e) => Err(fail("creating the bucket", e)),
        }
    }

    async fn enable_versioning(&self, name: &str) -> ProviderResult<()> {
        self.s3()
            .put_bucket_versioning()
            .bucket(name)
            .versioning_configuration(
                VersioningConfiguration::builder()
                    .status(BucketVersioningStatus::Enabled)
                    .build(),
            )
            .send()
            .await
            .map_err(|e| fail("turning on bucket versioning", e))?;
        Ok(())
    }

    async fn enable_encryption(&self, name: &str) -> ProviderResult<()> {
        let rule = ServerSideEncryptionRule::builder()
            .apply_server_side_encryption_by_default(
                ServerSideEncryptionByDefault::builder()
                    .sse_algorithm(ServerSideEncryption::Aes256)
                    .build()
                    .expect("algorithm is set"),
            )
            .build();
        self.s3()
            .put_bucket_encryption()
            .bucket(name)
            .server_side_encryption_configuration(
                ServerSideEncryptionConfiguration::builder()
                    .rules(rule)
                    .build()
                    .expect("rules are set"),
            )
            .send()
            .await
            .map_err(|e| fail("turning on bucket encryption", e))?;
        Ok(())
    }

    async fn tag_bucket(&self, name: &str) -> ProviderResult<()> {
        let s3 = self.s3();
        let mut tags: Vec<_> = self
            .bucket_tags(&s3, name)
            .await?
            .into_iter()
            .filter(|t| t.key() != PROJECT_TAG.0)
            .collect();
        tags.push(
            aws_sdk_s3::types::Tag::builder()
                .key(PROJECT_TAG.0)
                .value(PROJECT_TAG.1)
                .build()
                .expect("key and value are set"),
        );
        s3.put_bucket_tagging()
            .bucket(name)
            .tagging(
                Tagging::builder()
                    .set_tag_set(Some(tags))
                    .build()
                    .expect("tags are set"),
            )
            .send()
            .await
            .map_err(|e| fail("tagging the bucket", e))?;
        Ok(())
    }

    async fn expire_old_versions(&self, name: &str) -> ProviderResult<()> {
        let rule = LifecycleRule::builder()
            .id("rega-old-versions")
            .status(ExpirationStatus::Enabled)
            .filter(LifecycleRuleFilter::builder().prefix("").build())
            .noncurrent_version_expiration(
                NoncurrentVersionExpiration::builder()
                    .noncurrent_days(OLD_VERSION_DAYS)
                    .build(),
            )
            // Drops the delete markers left once every old version is gone.
            .expiration(
                LifecycleExpiration::builder()
                    .expired_object_delete_marker(true)
                    .build(),
            )
            .abort_incomplete_multipart_upload(
                AbortIncompleteMultipartUpload::builder()
                    .days_after_initiation(7)
                    .build(),
            )
            .build()
            .expect("status is set");
        self.s3()
            .put_bucket_lifecycle_configuration()
            .bucket(name)
            .lifecycle_configuration(
                BucketLifecycleConfiguration::builder()
                    .rules(rule)
                    .build()
                    .expect("rules are set"),
            )
            .send()
            .await
            .map_err(|e| fail("setting the bucket lifecycle", e))?;
        Ok(())
    }

    async fn list_objects(&self, bucket: &str, prefix: &str) -> ProviderResult<Vec<(String, u64)>> {
        let mut pages = self
            .s3()
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .into_paginator()
            .send();
        let mut out = Vec::new();
        while let Some(page) = pages.next().await {
            let page = match page {
                Ok(page) => page,
                Err(e) if is_missing_bucket(e.code()) => return Ok(vec![]),
                Err(e) => return Err(fail("listing the bucket", e)),
            };
            for o in page.contents() {
                if let Some(key) = o.key() {
                    out.push((key.into(), o.size().unwrap_or(0).max(0) as u64));
                }
            }
        }
        Ok(out)
    }

    async fn get_object(&self, bucket: &str, key: &str) -> ProviderResult<Option<Vec<u8>>> {
        match self.s3().get_object().bucket(bucket).key(key).send().await {
            Ok(out) => {
                let body = out
                    .body
                    .collect()
                    .await
                    .map_err(|e| ProviderError::Cloud(format!("reading {key} failed: {e}")))?;
                Ok(Some(body.into_bytes().to_vec()))
            }
            Err(e) if matches!(e.code(), Some("NoSuchKey") | Some("NoSuchBucket")) => Ok(None),
            Err(e) => Err(fail(&format!("reading {key}"), e)),
        }
    }

    async fn put_object(&self, bucket: &str, key: &str, body: Vec<u8>) -> ProviderResult<()> {
        self.s3()
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|e| fail(&format!("saving {key}"), e))?;
        Ok(())
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> ProviderResult<()> {
        match self
            .s3()
            .delete_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if is_missing_bucket(e.code()) => Ok(()),
            Err(e) => Err(fail(&format!("deleting {key}"), e)),
        }
    }

    async fn credential_env(&self) -> ProviderResult<Vec<(String, String)>> {
        let creds = self
            .config
            .credentials_provider()
            .ok_or_else(|| ProviderError::MissingCredentials("AWS".into()))?
            .provide_credentials()
            .await
            .map_err(|e| {
                ProviderError::Cloud(format!("AWS credentials: {}", DisplayErrorContext(&e)))
            })?;
        let mut env = vec![
            ("AWS_ACCESS_KEY_ID".into(), creds.access_key_id().into()),
            (
                "AWS_SECRET_ACCESS_KEY".into(),
                creds.secret_access_key().into(),
            ),
        ];
        if let Some(token) = creds.session_token() {
            env.push(("AWS_SESSION_TOKEN".into(), token.into()));
        }
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_states_map_to_machine_states() {
        assert_eq!(
            instance_state(Some(&InstanceStateName::Running)),
            MachineState::Running
        );
        assert_eq!(
            instance_state(Some(&InstanceStateName::ShuttingDown)),
            MachineState::Stopping
        );
        assert_eq!(
            instance_state(Some(&InstanceStateName::Terminated)),
            MachineState::Terminated
        );
        assert_eq!(instance_state(None), MachineState::Terminated);
    }

    #[test]
    fn tags_are_found_by_key() {
        let tags = [
            Tag::builder().key("Project").value("ReGa").build(),
            Tag::builder().key("ReGaMachine").value("m-1").build(),
        ];
        assert_eq!(tag_value(&tags, MACHINE_TAG), Some("m-1"));
        assert_eq!(tag_value(&tags, "Name"), None);
    }
}
