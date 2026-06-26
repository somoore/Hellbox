//! `ldoom resume` — thaw the capsule (~2.6s), poll until RUNNING.
//!
//! Note: the CSPRNG-reseed-on-resume mitigation (docs/architecture.md §7) lives
//! *inside* the capsule (its `/resume` hook bounces KasmVNC's TLS listener); the
//! CLI just drives the lifecycle call.

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

pub async fn run(name: &str) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;
    let microvm_id = state
        .require(name)?
        .microvm_id
        .clone()
        .with_context(|| format!("capsule '{name}' has no microvm to resume"))?;

    let aws = Aws::new(&cfg).await?;
    aws.microvm
        .resume_microvm()
        .microvm_identifier(&microvm_id)
        .send()
        .await
        .context("resume_microvm")?;
    tracing::info!(target: "shrink::resume", "resuming {microvm_id}");

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

    // Endpoint may change across resume; refresh it.
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
        anyhow::bail!("'{name}' did not resume (state {final_state})");
    }
    println!("resumed '{name}' — RUNNING");
    Ok(())
}
