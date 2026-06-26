//! `ldoom up` — `run_microvm` from the built image → poll until RUNNING →
//! persist microvm_id + endpoint.

use anyhow::{Context, Result};
use aws_sdk_lambdamicrovms::types::IdlePolicy;

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

/// Hard cap on a single run (8h) — keeps a forgotten capsule from running forever.
const MAX_DURATION_SECS: i32 = 8 * 60 * 60;
/// Idle policy: auto-resume on traffic, suspend after a few idle minutes.
const MAX_IDLE_SECS: i32 = 5 * 60;
const SUSPENDED_DURATION_SECS: i32 = 24 * 60 * 60;

pub async fn run(name: &str) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;

    let image_id = {
        let capsule = state.require(name)?;
        capsule
            .image_arn
            .clone()
            .or_else(|| capsule.image_version.clone())
            .with_context(|| {
                format!("capsule '{name}' has no image yet — run `ldoom build` first")
            })?
    };

    let aws = Aws::new(&cfg).await?;

    let idle_policy = IdlePolicy::builder()
        .auto_resume_enabled(true)
        .max_idle_duration_seconds(MAX_IDLE_SECS)
        .suspended_duration_seconds(SUSPENDED_DURATION_SECS)
        .build()
        .context("building idle policy")?;

    // Build the request; only attach network connectors / exec role when configured —
    // empty values would fail validation, so omit them and get Lambda-managed defaults.
    let mut req = aws
        .microvm
        .run_microvm()
        .image_identifier(image_id)
        .idle_policy(idle_policy)
        .maximum_duration_in_seconds(MAX_DURATION_SECS)
        // Unique per invocation: a deterministic token would idempotently return a
        // previously-terminated MicroVM on a re-`up`.
        .client_token(format!("shrink-up-{name}-{}", now_secs()));
    if !cfg.ingress_connector_arn.trim().is_empty() {
        req = req.ingress_network_connectors(cfg.ingress_connector_arn.clone());
    }
    if !cfg.egress_connector_arn.trim().is_empty() {
        req = req.egress_network_connectors(cfg.egress_connector_arn.clone());
    }
    if let Some(role) = cfg.execution_role_arn.as_deref() {
        req = req.execution_role_arn(role);
    }

    let run = req.send().await.context("run_microvm")?;
    let microvm_id = run.microvm_id().to_string();
    tracing::info!(target: "shrink::up", "launched {microvm_id} (state {})", run.state().as_str());

    state.upsert(name, |c| {
        c.microvm_id = Some(microvm_id.clone());
        c.endpoint = Some(run.endpoint().to_string());
        c.state = Some(run.state().as_str().to_string());
    })?;

    // Poll until RUNNING (TERMINATED is the failure terminal we bail on).
    let id = microvm_id.clone();
    let final_state = poll_until(
        &format!("microvm {name}"),
        &["RUNNING", "TERMINATED", "FAILED"],
        PollOpts::default(),
        || {
            let aws = &aws;
            let id = id.clone();
            async move {
                let out = aws
                    .microvm
                    .get_microvm()
                    .microvm_identifier(&id)
                    .send()
                    .await
                    .context("get_microvm")?;
                Ok(out.state().as_str().to_string())
            }
        },
    )
    .await?;

    // Refresh endpoint (it may only be populated once RUNNING).
    let endpoint = aws
        .microvm
        .get_microvm()
        .microvm_identifier(&microvm_id)
        .send()
        .await
        .ok()
        .map(|o| o.endpoint().to_string());

    state.upsert(name, |c| {
        c.state = Some(final_state.clone());
        if endpoint.is_some() {
            c.endpoint = endpoint.clone();
        }
    })?;

    if final_state != "RUNNING" {
        anyhow::bail!("microvm '{name}' did not reach RUNNING (state {final_state})");
    }

    println!("up '{name}': {microvm_id} RUNNING");
    Ok(())
}

/// Coarse wall-clock seconds for a unique-enough client token.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
