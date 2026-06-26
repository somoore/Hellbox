//! `~/.lambdadoom/config.toml` — the static, per-user settings that don't change
//! between capsules: region, the S3 artifact bucket, and the role ARNs that the
//! CloudFormation stack (`deploy/doom.yaml`) emits. Populated once from the stack
//! Outputs.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default streaming/display port exposed by the capsule (noVNC over WSS).
pub const DEFAULT_PORT: i32 = 6901;
/// Default audio port (PCM-over-WebSocket). `open` routes the `/shrinkaudio`
/// path here; the endpoint multiplexes both internal ports via X-aws-proxy-port.
pub const DEFAULT_AUDIO_PORT: i32 = 6902;
/// Default video port (H.264/WebCodecs). `open` routes the `/shrinkvideo` path here.
pub const DEFAULT_VIDEO_PORT: i32 = 6903;
/// Default input port. `open` routes the `/shrinkinput` path here.
pub const DEFAULT_INPUT_PORT: i32 = 6904;
/// Region this demo defaults to (matches the CloudFormation template + README).
pub const DEFAULT_REGION: &str = "us-east-1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// AWS region. Defaults to us-east-2.
    #[serde(default = "default_region")]
    pub region: String,
    /// S3 bucket for the build context zip.
    pub artifact_bucket: String,
    /// IAM role the MicroVMs build assumes to read the artifact / write the image.
    pub build_role_arn: String,
    /// IAM role the running MicroVM assumes (optional — egress, etc.).
    #[serde(default)]
    pub execution_role_arn: Option<String>,
    /// Ingress network connector ARN (HTTPS/WSS in). Empty/omitted → Lambda-managed
    /// default (JWE-auth ingress), which is what the demo uses.
    #[serde(default)]
    pub ingress_connector_arn: String,
    /// Egress network connector ARN (out). Empty/omitted → Lambda-managed default
    /// (internet egress).
    #[serde(default)]
    pub egress_connector_arn: String,
    /// Base image ARN the capsule layers on top of (ARM64 Firecracker base).
    pub base_image_arn: String,
    /// Exposed display/stream port. Defaults to 6901.
    #[serde(default = "default_port")]
    pub port: i32,
    /// Audio port (PCM-over-WebSocket). Defaults to 6902.
    #[serde(default = "default_audio_port")]
    pub audio_port: i32,
    /// Video port (H.264/WebCodecs). Defaults to 6903. `open` routes the
    /// `/shrinkvideo` path here.
    #[serde(default = "default_video_port")]
    pub video_port: i32,
    /// Input port. Defaults to 6904. `open` routes the `/shrinkinput` path here.
    #[serde(default = "default_input_port")]
    pub input_port: i32,
    /// Display backend: `"vnc"` (noVNC, the default) or `"h264"` (H.264/WebCodecs).
    /// `None` means vnc. When `"h264"`, `ldoom open` appends `?display=h264` to the
    /// opened URL. Manage with `ldoom config set display …`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Client-side auto-suspend: if set and > 0, `ldoom open` suspends
    /// the MicroVM after this many minutes with no connected viewer (no active WS
    /// session through the proxy). Unset/0 → rely only on the platform `IdlePolicy`
    /// (≈5 min). Manage with `ldoom config set idle_suspend_minutes …`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_suspend_minutes: Option<u64>,
}

fn default_region() -> String {
    DEFAULT_REGION.to_string()
}
fn default_port() -> i32 {
    DEFAULT_PORT
}
fn default_audio_port() -> i32 {
    DEFAULT_AUDIO_PORT
}
fn default_video_port() -> i32 {
    DEFAULT_VIDEO_PORT
}
fn default_input_port() -> i32 {
    DEFAULT_INPUT_PORT
}

impl Config {
    /// `~/.lambdadoom/config.toml` (or the platform equivalent via `directories`).
    pub fn path() -> Result<PathBuf> {
        Ok(shrink_dir()?.join("config.toml"))
    }

    /// Load and parse the config, with a friendly error if it's missing.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no config at {} — deploy `deploy/doom.yaml` (the Launch Stack button \
                 or `aws cloudformation deploy`) and copy the stack Outputs there \
                 (region, artifact_bucket, build_role_arn, execution_role_arn). See README.",
                path.display()
            )
        })?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    /// Persist the config (used by tooling/tests; the CLI mostly reads).
    #[allow(dead_code)]
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// `~/.lambdadoom` — shared by config.toml and state.json. `directories` maps this
/// to `%USERPROFILE%\.lambdadoom` on Windows automatically. (The function name is
/// historical; the directory is `.lambdadoom`.)
pub fn shrink_dir() -> Result<PathBuf> {
    // Honor LAMBDADOOM_HOME so deploy.sh's override (and isolated test runs) actually
    // redirect config + state; otherwise default to ~/.lambdadoom.
    if let Ok(dir) = std::env::var("LAMBDADOOM_HOME")
        && !dir.trim().is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    // ponytail: ProjectDirs would put us under AppData on Windows; the docs all
    // say `~/.lambdadoom`, so anchor on the home dir directly and keep it identical
    // across platforms. directories::BaseDirs gives us the home dir portably.
    let dirs = directories::BaseDirs::new()
        .context("could not resolve home directory for ~/.lambdadoom")?;
    Ok(dirs.home_dir().join(".lambdadoom"))
}
