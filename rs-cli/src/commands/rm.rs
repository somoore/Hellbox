//! `ldoom rm` — full per-capsule teardown: terminate any running microvm, delete the
//! MicroVM image, and drop the capsule from state. (`down` keeps the image so you can
//! relaunch; `rm` removes it too, for a clean uninstall.)

use std::time::Duration;

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::poll::{PollOpts, poll_until};
use crate::state::State;

pub async fn run(name: &str) -> Result<()> {
    let cfg = Config::load()?;
    let mut state = State::load()?;
    let cap = state.require(name)?.clone();
    let aws = Aws::new(&cfg).await?;

    // Terminate a running microvm first — DeleteMicrovmImage fails while one is live.
    // Best-effort: it may already be gone.
    if let Some(microvm_id) = cap.microvm_id.clone() {
        let _ = aws
            .microvm
            .terminate_microvm()
            .microvm_identifier(&microvm_id)
            .send()
            .await;
        tracing::info!(target: "shrink::rm", "terminating {microvm_id}");

        // Termination is ASYNC: the VM goes TERMINATING -> TERMINATED over a few
        // seconds, and DeleteMicrovmImage rejects with "Cannot delete MicroVM image
        // with running MicroVMs" until it's fully gone. Wait for TERMINATED first.
        // A get error means the VM record is already gone, which is also fine.
        let _ = poll_until(
            &format!("microvm {name}"),
            &["TERMINATED"],
            PollOpts {
                interval: Duration::from_secs(3),
                timeout: Duration::from_secs(180),
            },
            || async {
                match aws
                    .microvm
                    .get_microvm()
                    .microvm_identifier(&microvm_id)
                    .send()
                    .await
                {
                    Ok(o) => Ok(o.state().as_str().to_string()),
                    Err(_) => Ok("TERMINATED".to_string()),
                }
            },
        )
        .await;
    }

    if let Some(image_arn) = cap.image_arn.clone() {
        delete_image_with_retry(&aws, &image_arn).await?;
        tracing::info!(target: "shrink::rm", "deleted image {image_arn}");
    }

    state.remove(name)?;
    println!("rm '{name}': image deleted, capsule removed from state");
    Ok(())
}

/// Delete the image, tolerating the brief window after a VM terminates.
///
/// Termination is async, so `DeleteMicrovmImage` can still reject with "Cannot
/// delete MicroVM image with running MicroVMs" even after `GetMicrovm` reports
/// TERMINATED — and crucially when `down` ran first and cleared the microvm id, so
/// the terminate/poll block above is skipped entirely and we'd otherwise race the
/// teardown. Retry on *that* specific message; surface any other error at once.
async fn delete_image_with_retry(aws: &Aws, image_arn: &str) -> Result<()> {
    use aws_sdk_lambdamicrovms::error::ProvideErrorMetadata;

    let deadline = Duration::from_secs(180);
    let interval = Duration::from_secs(3);
    let start = std::time::Instant::now();
    loop {
        match aws
            .microvm
            .delete_microvm_image()
            .image_identifier(image_arn)
            .send()
            .await
        {
            Ok(_) => return Ok(()),
            Err(e) => {
                let transient = e
                    .message()
                    .map(|m| m.contains("running MicroVM"))
                    .unwrap_or(false);
                if transient && start.elapsed() < deadline {
                    tracing::info!(
                        target: "shrink::rm",
                        "image still has a terminating microvm; retrying delete in {interval:?}"
                    );
                    tokio::time::sleep(interval).await;
                    continue;
                }
                return Err(e).context("delete_microvm_image");
            }
        }
    }
}
