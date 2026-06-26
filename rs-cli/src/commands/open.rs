//! `ldoom open` — the magic. Mint a short-lived auth token for the capsule and
//! open it in a browser tab.
//!
//! ## Auth fork (docs/architecture.md §5) — Fork B is THE path now.
//! The MicroVM endpoint authenticates with a JWE in the `X-aws-proxy-auth`
//! header (port via `X-aws-proxy-port`). Browsers can't set that header on
//! navigation or the `WebSocket` constructor, and — verified live — the same
//! JWE carried in a query string returns **403**. So the only workable browser
//! path is **Fork B**: a `127.0.0.1` loopback reverse-proxy that injects the
//! headers and forwards to the MicroVM over TLS (HTTP + WSS). We start it, then
//! open `http://127.0.0.1:<port>/`. (Fork A — header-free query string — is dead.)

use anyhow::{Context, Result};
use aws_sdk_lambdamicrovms::types::PortSpecification;

use crate::aws::Aws;
use crate::browser;
use crate::config::Config;
use crate::state::State;

/// The header/key the auth token is delivered under (per the SDK contract).
const AUTH_HEADER: &str = "X-aws-proxy-auth";
/// Token validity (minutes; AWS caps this at 60).
const TOKEN_TTL_MINUTES: i32 = 30;

pub async fn run(name: &str, no_open: bool) -> Result<()> {
    let cfg = Config::load()?;
    let state = State::load()?;
    let capsule = state.require(name)?;

    let microvm_id = capsule.microvm_id.clone().with_context(|| {
        format!("capsule '{name}' isn't running — `ldoom up --name {name}` first")
    })?;
    let endpoint = capsule
        .endpoint
        .clone()
        .with_context(|| format!("capsule '{name}' has no endpoint yet — is it RUNNING?"))?;

    let port = cfg.port;
    let audio_port = cfg.audio_port;
    let video_port = cfg.video_port;
    let input_port = cfg.input_port;
    // H.264 display backend ("h264") appends ?display=h264 to the opened URL; the
    // default (vnc/None) opens the plain URL.
    let h264 = cfg.display.as_deref() == Some("h264");
    let aws = Aws::new(&cfg).await?;

    // Mint the token for all four internal capsule ports (display, audio, video, input).
    let mut tok_req = aws
        .microvm
        .create_microvm_auth_token()
        .microvm_identifier(&microvm_id)
        .expiration_in_minutes(TOKEN_TTL_MINUTES);
    for p in [port, audio_port, video_port, input_port] {
        tok_req = tok_req.allowed_ports(PortSpecification::Port(p));
    }
    // Token minting is best-effort: a SUSPENDED capsule can't mint, but we still want
    // to start the local proxy so its control bar (Resume) is reachable. If it fails,
    // start with an empty token — the data path is dead while suspended anyway, and
    // the browser's Resume re-mints a fresh token once the VM is RUNNING again.
    let jwe = match tok_req.send().await {
        Ok(out) => out
            .auth_token()
            .get(AUTH_HEADER)
            .cloned()
            .unwrap_or_default(),
        Err(e) => {
            tracing::warn!(
                target: "shrink::open",
                "could not mint auth token (capsule may be suspended): {e:#} — \
                 starting control-only proxy; click Resume in the tab to thaw"
            );
            String::new()
        }
    };

    // Client-side auto-suspend after N idle minutes (0/unset = off).
    let idle_minutes = cfg.idle_suspend_minutes.unwrap_or(0);
    open_fork_b(
        &endpoint,
        port,
        audio_port,
        video_port,
        input_port,
        h264,
        &jwe,
        no_open,
        &aws,
        &microvm_id,
        name,
        idle_minutes,
    )
    .await
}

/// Strip any scheme/trailing slash from the stored endpoint, leaving the bare
/// host the proxy dials over TLS.
fn host_of(endpoint: &str) -> String {
    endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("wss://")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(feature = "proxy")]
#[allow(clippy::too_many_arguments)]
async fn open_fork_b(
    endpoint: &str,
    port: i32,
    audio_port: i32,
    video_port: i32,
    input_port: i32,
    h264: bool,
    jwe: &str,
    no_open: bool,
    aws: &Aws,
    microvm_id: &str,
    name: &str,
    idle_minutes: u64,
) -> Result<()> {
    use std::sync::Arc;

    use crate::proxy::{self, ProxyActivity, ProxyConfig, ProxyControl, Upstream};

    let activity = Arc::new(ProxyActivity::default());
    // Live upstream cells (host + token) the proxy reads per-request and the
    // browser-driven resume rewrites in place after a thaw.
    let upstream = Upstream::new(host_of(endpoint), jwe.to_string());
    // Control wiring for the injected Suspend/Resume buttons: the proxy can drive
    // suspend/resume/state and re-mint the token for the same four ports we opened.
    let control = Arc::new(ProxyControl {
        microvm: aws.microvm.clone(),
        microvm_id: microvm_id.to_string(),
        name: name.to_string(),
        token_ports: vec![port, audio_port, video_port, input_port],
        upstream: upstream.clone(),
    });
    let base = ProxyConfig {
        upstream,
        upstream_port: port,
        local_port: 6080, // conventional noVNC port.
        routes: vec![
            ("/shrinkaudio".to_string(), audio_port),
            ("/shrinkvideo".to_string(), video_port),
            ("/shrinkinput".to_string(), input_port),
        ],
        activity: Some(activity.clone()),
        control: Some(control),
    };
    // ponytail: try 6080, else fall back to an ephemeral port — no pre-probe.
    let base_url = match proxy::start(base.clone()).await {
        Ok(u) => u,
        Err(_) => {
            proxy::start(ProxyConfig {
                local_port: 0,
                ..base
            })
            .await?
        }
    };
    // H.264 display backend: tell the page to use WebCodecs via ?display=h264.
    let url = if h264 {
        format!("{base_url}/?display=h264")
    } else {
        base_url
    };

    tracing::info!(target: "shrink::open", "Fork B proxy serving {url} (auth header injected; JWE <redacted>)");
    if no_open {
        println!("Fork B proxy ready: {url}  (--no-open: not launching browser)");
    } else {
        browser::open(&url)?;
        println!("opened (Fork B loopback proxy): {url}");
    }

    // Keep the process alive so the proxy keeps serving the tab. With auto-suspend
    // enabled (Lever 4), also race an idle monitor that suspends the MicroVM once no
    // viewer has been connected for `idle_minutes`; otherwise just wait for Ctrl-C.
    if idle_minutes > 0 {
        println!("auto-suspend: will freeze '{name}' after {idle_minutes} idle min with no viewer");
        tracing::info!(target: "shrink::open", "Fork B proxy running; Ctrl-C to stop (auto-suspend after {idle_minutes} idle min)");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = idle_monitor(activity, idle_minutes) => {
                suspend_idle(aws, microvm_id, name).await?;
            }
        }
    } else {
        tracing::info!(target: "shrink::open", "Fork B proxy running; Ctrl-C to stop");
        tokio::signal::ctrl_c().await.ok();
    }
    Ok(())
}

/// Resolve once no WebSocket session has been active for `idle_minutes`. Only starts
/// counting after at least one viewer has connected, so it never suspends a capsule
/// the user just opened but hasn't loaded yet.
#[cfg(feature = "proxy")]
async fn idle_monitor(activity: std::sync::Arc<crate::proxy::ProxyActivity>, idle_minutes: u64) {
    use std::time::Duration;
    let idle_secs = idle_minutes * 60;
    let tick = 15u64;
    let mut idle_for = 0u64;
    let mut ever_connected = false;
    loop {
        tokio::time::sleep(Duration::from_secs(tick)).await;
        if activity.active() > 0 {
            ever_connected = true;
            idle_for = 0;
        } else if ever_connected {
            idle_for += tick;
            if idle_for >= idle_secs {
                return;
            }
        }
    }
}

/// Suspend the MicroVM when the idle monitor fires, and note it in state.
#[cfg(feature = "proxy")]
async fn suspend_idle(aws: &Aws, microvm_id: &str, name: &str) -> Result<()> {
    use anyhow::Context;
    tracing::info!(target: "shrink::open", "idle timeout reached — auto-suspending {microvm_id}");
    aws.microvm
        .suspend_microvm()
        .microvm_identifier(microvm_id)
        .send()
        .await
        .context("auto-suspend (idle)")?;
    if let Ok(mut st) = State::load() {
        let _ = st.upsert(name, |c| c.state = Some("SUSPENDING".to_string()));
    }
    println!(
        "auto-suspended '{name}' after idle timeout — cost saved. `ldoom resume --name {name}` to thaw."
    );
    Ok(())
}

#[cfg(not(feature = "proxy"))]
#[allow(clippy::too_many_arguments)]
async fn open_fork_b(
    _endpoint: &str,
    _port: i32,
    _audio_port: i32,
    _video_port: i32,
    _input_port: i32,
    _h264: bool,
    _jwe: &str,
    _no_open: bool,
    _aws: &Aws,
    _microvm_id: &str,
    _name: &str,
    _idle_minutes: u64,
) -> Result<()> {
    anyhow::bail!(
        "ldoom open requires the `proxy` feature (Fork B loopback proxy), which is \
         on by default. Rebuild without `--no-default-features`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_normalizes_scheme_and_slash() {
        assert_eq!(
            host_of("https://abc.lambda-microvm.us-east-2.on.aws/"),
            "abc.lambda-microvm.us-east-2.on.aws"
        );
        assert_eq!(host_of("abc.example.com"), "abc.example.com");
        assert_eq!(host_of("wss://h/"), "h");
    }
}
