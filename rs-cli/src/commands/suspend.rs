//! `ldoom suspend` — freeze the live capsule, poll until SUSPENDED.

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
        .with_context(|| format!("capsule '{name}' isn't running"))?;

    let aws = Aws::new(&cfg).await?;
    aws.microvm
        .suspend_microvm()
        .microvm_identifier(&microvm_id)
        .send()
        .await
        .context("suspend_microvm")?;
    tracing::info!(target: "shrink::suspend", "suspending {microvm_id}");

    let id = microvm_id.clone();
    let final_state = poll_until(
        &format!("microvm {name}"),
        &["SUSPENDED", "TERMINATED", "FAILED"],
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

    state.upsert(name, |c| c.state = Some(final_state.clone()))?;

    if final_state != "SUSPENDED" {
        anyhow::bail!("'{name}' did not suspend (state {final_state})");
    }
    println!("suspended '{name}'");
    Ok(())
}
