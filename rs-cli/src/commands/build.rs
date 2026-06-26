//! `ldoom build` — zip `capsule/` → upload to S3 → `create_microvm_image` →
//! poll until CREATED → persist the image ARN/version to state.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use aws_sdk_lambdamicrovms::types::{
    Capability, CodeArtifact, HookState, Hooks, MicrovmHooks, MicrovmImageHooks,
};
use walkdir::WalkDir;
use zip::write::FileOptions;

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

/// The platform probes lifecycle hooks (ready/run/resume) by POSTing to
/// `/aws/lambda-microvms/runtime/v1/<hook>` on this port. It is NOT the app/
/// stream port (6901) — AWS empirically requires the hook listener on 9000
/// (verified: an 8080 listener never receives the probe → "Ready hook timed out").
const HOOK_PORT: i32 = 9000;

pub async fn run(name: &str, app: Option<&str>, capsule_dir_override: Option<&str>) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;

    // capsule/ lives at the repo root next to src/. Resolve relative to CWD; the
    // CLI is expected to run from the repo. `--capsule-dir` overrides it to build
    // an alternate capsule.
    let capsule_dir = capsule_dir(capsule_dir_override)?;
    if let Some(app) = app {
        // ponytail: caller-supplied --app just gets noted; the capsule build
        // expects the .exe already staged under capsule/app/. Surfacing a hint
        // beats silently ignoring it.
        tracing::info!(target: "shrink::build", "note: --app {app} — ensure it's staged under capsule/app/");
    }

    // 1. zip the build context.
    let zip_path = zip_context(&capsule_dir)
        .with_context(|| format!("zipping build context at {}", capsule_dir.display()))?;
    tracing::info!(target: "shrink::build", "built context zip at {}", zip_path.display());

    // 2. upload to S3.
    let key = format!("contexts/{name}.zip");
    let bytes = std::fs::read(&zip_path).context("reading context zip")?;
    let aws = Aws::new(&cfg).await?;
    aws.s3
        .put_object()
        .bucket(&cfg.artifact_bucket)
        .key(&key)
        .body(bytes.into())
        .send()
        .await
        .with_context(|| format!("uploading s3://{}/{key}", cfg.artifact_bucket))?;
    let code_artifact_uri = format!("s3://{}/{}", cfg.artifact_bucket, key);
    tracing::info!(target: "shrink::build", "uploaded {code_artifact_uri}");

    // 3. create the image. Hooks are ENABLED/DISABLED enums; the readiness probe
    // listener must be on port 9000; ready timeout is generous (Wine cold-start > 60s).
    let hooks = Hooks::builder()
        .port(HOOK_PORT)
        .microvm_image_hooks(
            MicrovmImageHooks::builder()
                .ready(HookState::Enabled)
                .ready_timeout_in_seconds(600)
                .validate(HookState::Disabled)
                .validate_timeout_in_seconds(60)
                .build(),
        )
        .microvm_hooks(
            MicrovmHooks::builder()
                .run(HookState::Enabled)
                .run_timeout_in_seconds(60)
                .resume(HookState::Enabled)
                .resume_timeout_in_seconds(60)
                .suspend(HookState::Disabled)
                .suspend_timeout_in_seconds(60)
                .terminate(HookState::Disabled)
                .terminate_timeout_in_seconds(60)
                .build(),
        )
        .build();
    let created = aws
        .microvm
        .create_microvm_image()
        .name(name)
        .base_image_arn(&cfg.base_image_arn)
        .build_role_arn(&cfg.build_role_arn)
        .code_artifact(CodeArtifact::Uri(code_artifact_uri))
        // Privileged-like caps so FEX/Box64 (Hangover) can run x86 code without
        // a SIGBUS under the MicroVM runtime sandbox.
        .additional_os_capabilities(Capability::All)
        .hooks(hooks)
        .client_token(client_token(name))
        .send()
        .await
        .context("create_microvm_image")?;
    let image_arn = created.image_arn().to_string();
    tracing::info!(target: "shrink::build", "image creating: {image_arn} (state {})", created.state().as_str());

    state.upsert(name, |c| {
        c.image_arn = Some(image_arn.clone());
        c.image_version = created.latest_active_image_version().map(str::to_string);
        c.state = Some(created.state().as_str().to_string());
    })?;

    // 4. poll until terminal.
    let image_id = image_arn.clone();
    let final_state = poll_until(
        &format!("image {name}"),
        &["CREATED", "CREATE_FAILED"],
        PollOpts::default(),
        || {
            let aws = &aws;
            let image_id = image_id.clone();
            async move {
                let out = aws
                    .microvm
                    .get_microvm_image()
                    .image_identifier(&image_id)
                    .send()
                    .await
                    .context("get_microvm_image")?;
                Ok(out.state().as_str().to_string())
            }
        },
    )
    .await?;

    // Capture the active version on success.
    let active_version = aws
        .microvm
        .get_microvm_image()
        .image_identifier(&image_arn)
        .send()
        .await
        .ok()
        .and_then(|o| o.latest_active_image_version().map(str::to_string));

    state.upsert(name, |c| {
        c.state = Some(final_state.clone());
        if active_version.is_some() {
            c.image_version = active_version.clone();
        }
    })?;

    if final_state == "CREATE_FAILED" {
        bail!("image build for '{name}' failed (state CREATE_FAILED)");
    }

    println!("built '{name}': image {image_arn} CREATED");
    Ok(())
}

/// Locate the capsule build context: `--capsule-dir` if given, else `./capsule`.
fn capsule_dir(override_path: Option<&str>) -> Result<PathBuf> {
    let dir = match override_path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir()?.join("capsule"),
    };
    if !dir.is_dir() {
        bail!(
            "no capsule dir at {} — run `ldoom build` from the LambdaDoom repo root, \
             or pass --capsule-dir <PATH>",
            dir.display()
        );
    }
    Ok(dir)
}

/// Deterministic-ish client token so retried builds dedupe. ponytail: name +
/// coarse timestamp is plenty for the spike (no UUID dep).
fn client_token(name: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("shrink-build-{name}-{secs}")
}

/// Zip `dir` recursively into a temp file, preserving relative paths. Returns the
/// zip path.
fn zip_context(dir: &Path) -> Result<PathBuf> {
    let out_path = std::env::temp_dir().join(format!("shrink-context-{}.zip", std::process::id()));
    let file = std::fs::File::create(&out_path)
        .with_context(|| format!("creating {}", out_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: FileOptions = FileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let rel = path.strip_prefix(dir).context("relativizing zip entry")?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        // Forward-slash paths inside the archive regardless of host OS.
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");

        if path.is_dir() {
            zip.add_directory(format!("{name}/"), opts)
                .with_context(|| format!("adding dir {name}"))?;
        } else {
            zip.start_file(&name, opts)
                .with_context(|| format!("adding file {name}"))?;
            let data =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            zip.write_all(&data)
                .with_context(|| format!("writing {name} into zip"))?;
        }
    }
    zip.finish().context("finalizing zip")?;
    Ok(out_path)
}
