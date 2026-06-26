//! AWS wiring: build one `SdkConfig` (region from our config, default credential
//! chain — picks up `AWS_PROFILE` / the shared credentials file) and hand out the
//! two clients we need: the official `aws-sdk-lambdamicrovms` `Client` and an S3
//! client for artifact upload.

use anyhow::Result;
use aws_config::BehaviorVersion;
use aws_sdk_lambdamicrovms::Client as MicrovmClient;

use crate::config::Config;

/// The set of AWS clients the commands share.
pub struct Aws {
    pub microvm: MicrovmClient,
    pub s3: aws_sdk_s3::Client,
    /// Resolved region (kept for log/diagnostic use by callers).
    #[allow(dead_code)]
    pub region: String,
}

impl Aws {
    /// Resolve credentials + region and build both clients.
    pub async fn new(cfg: &Config) -> Result<Self> {
        let region = aws_config::Region::new(cfg.region.clone());
        // Default credential chain: env, profile (AWS_PROFILE), shared file, SSO,
        // container/instance metadata. We only pin the region from our config.
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .region(region)
            .load()
            .await;

        let microvm = MicrovmClient::new(&sdk_config);
        let s3 = aws_sdk_s3::Client::new(&sdk_config);

        Ok(Self {
            microvm,
            s3,
            region: cfg.region.clone(),
        })
    }
}
