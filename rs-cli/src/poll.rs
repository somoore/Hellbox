//! Generic "poll an async getter until it reaches a terminal state" helper.
//! Used by `build` (image CREATED/CREATE_FAILED), `up`/`resume` (RUNNING),
//! and `suspend` (SUSPENDED).

use std::collections::HashSet;
use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// How often to poll, and how long before we give up.
#[derive(Clone, Copy, Debug)]
pub struct PollOpts {
    pub interval: Duration,
    pub timeout: Duration,
}

impl Default for PollOpts {
    fn default() -> Self {
        // ~10s cadence (docs/architecture.md), generous ceiling for cold image builds.
        Self {
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(15 * 60),
        }
    }
}

/// Poll `getter` until it returns a state in `terminal`, then return that state.
///
/// `getter` is an async closure returning the *current* state string. We don't
/// interpret which terminal states are "good" vs "bad" — the caller checks the
/// returned string (e.g. CREATED vs CREATE_FAILED). Logs each transition.
pub async fn poll_until<F, Fut>(
    label: &str,
    terminal: &[&str],
    opts: PollOpts,
    mut getter: F,
) -> Result<String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let terminal: HashSet<&str> = terminal.iter().copied().collect();
    let start = Instant::now();
    let mut last: Option<String> = None;

    loop {
        let state = getter().await?;
        if last.as_deref() != Some(state.as_str()) {
            tracing::info!(target: "shrink::poll", "{label}: {state}");
            last = Some(state.clone());
        }
        if terminal.contains(state.as_str()) {
            return Ok(state);
        }
        if start.elapsed() >= opts.timeout {
            bail!(
                "timed out after {:?} waiting for {label} to reach one of {:?} (last: {state})",
                opts.timeout,
                terminal
            );
        }
        tokio::time::sleep(opts.interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    // Runnable check: poll_until returns once a terminal state appears, and
    // surfaces the exact terminal string the caller must branch on.
    #[tokio::test]
    async fn stops_at_terminal_state() {
        let n = Cell::new(0u8);
        let opts = PollOpts {
            interval: Duration::from_millis(1),
            timeout: Duration::from_secs(5),
        };
        let got = poll_until("test", &["CREATED", "CREATE_FAILED"], opts, || async {
            let v = n.get();
            n.set(v + 1);
            Ok(if v < 2 {
                "CREATING".into()
            } else {
                "CREATED".into()
            })
        })
        .await
        .unwrap();
        assert_eq!(got, "CREATED");
    }
}
