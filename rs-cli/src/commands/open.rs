//! Open a capsule through the loopback auth proxy.

use anyhow::{Context, Result};
use aws_sdk_lambdamicrovms::types::PortSpecification;

use crate::aws::Aws;
#[cfg(feature = "proxy")]
use crate::browser;
use crate::config::Config;
#[cfg(any(feature = "proxy", test))]
use crate::lifecycle::host_of;
use crate::state::State;

const AUTH_HEADER: &str = "X-aws-proxy-auth";
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
    let h264 = cfg.display.as_deref() == Some("h264");
    let aws = Aws::new(&cfg).await?;

    let mut tok_req = aws
        .microvm
        .create_microvm_auth_token()
        .microvm_identifier(&microvm_id)
        .expiration_in_minutes(TOKEN_TTL_MINUTES);
    // SECURITY: the token's allowedPorts scoping is the load-bearing control that
    // keeps the in-VM services (which bind 0.0.0.0 and self-authenticate nothing)
    // off the public internet. Scope it to the display+stream ports ONLY. Never add
    // the internal readiness hook (9000) or the raw VNC port (5901) here.
    for p in [port, audio_port, video_port, input_port] {
        tok_req = tok_req.allowed_ports(PortSpecification::Port(p));
    }
    // Suspended capsules cannot mint; the local Resume UI can still start.
    let jwe = match tok_req.send().await {
        Ok(out) => out
            .auth_token()
            .get(AUTH_HEADER)
            .cloned()
            .unwrap_or_default(),
        Err(e) => {
            tracing::warn!(
                target: "ldoom::open",
                "could not mint auth token (capsule may be suspended): {e:#} — \
                 starting control-only proxy; click Resume in the tab to thaw"
            );
            String::new()
        }
    };

    let idle_minutes = cfg.idle_suspend_minutes.unwrap_or(0);
    open_fork_b(OpenForkBArgs {
        endpoint: &endpoint,
        port,
        audio_port,
        video_port,
        input_port,
        h264,
        jwe: &jwe,
        no_open,
        aws: &aws,
        microvm_id: &microvm_id,
        name,
        idle_minutes,
    })
    .await
}

struct OpenForkBArgs<'a> {
    endpoint: &'a str,
    port: i32,
    audio_port: i32,
    video_port: i32,
    input_port: i32,
    h264: bool,
    jwe: &'a str,
    no_open: bool,
    aws: &'a Aws,
    microvm_id: &'a str,
    name: &'a str,
    idle_minutes: u64,
}

#[cfg(feature = "proxy")]
async fn open_fork_b(args: OpenForkBArgs<'_>) -> Result<()> {
    use std::sync::Arc;

    use crate::proxy::{self, ProxyActivity, ProxyConfig, ProxyControl, Upstream};

    let OpenForkBArgs {
        endpoint,
        port,
        audio_port,
        video_port,
        input_port,
        h264,
        jwe,
        no_open,
        aws,
        microvm_id,
        name,
        idle_minutes,
    } = args;

    let activity = Arc::new(ProxyActivity::default());
    let upstream = Upstream::new(host_of(endpoint), jwe.to_string());
    let control_secret = generate_control_secret()?;
    let control = Arc::new(ProxyControl {
        microvm: aws.microvm.clone(),
        microvm_id: microvm_id.to_string(),
        name: name.to_string(),
        token_ports: vec![port, audio_port, video_port, input_port],
        upstream: upstream.clone(),
        control_secret,
    });
    let base = ProxyConfig {
        upstream,
        upstream_port: port,
        local_port: 6080,
        routes: vec![
            ("/ldoom/audio".to_string(), audio_port),
            ("/ldoom/video".to_string(), video_port),
            ("/ldoom/input".to_string(), input_port),
        ],
        activity: Some(activity.clone()),
        control: Some(control),
    };
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
    let url = if h264 {
        format!("{base_url}/?display=h264")
    } else {
        base_url
    };

    tracing::info!(target: "ldoom::open", "Fork B proxy serving {url} (auth header injected; JWE <redacted>)");
    if no_open {
        println!("Fork B proxy ready: {url}  (--no-open: not launching browser)");
    } else {
        browser::open(&url)?;
        println!("opened (Fork B loopback proxy): {url}");
    }

    if idle_minutes > 0 {
        println!("auto-suspend: will freeze '{name}' after {idle_minutes} idle min with no viewer");
        tracing::info!(target: "ldoom::open", "Fork B proxy running; Ctrl-C to stop (auto-suspend after {idle_minutes} idle min)");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = idle_monitor(activity, idle_minutes) => {
                suspend_idle(aws, microvm_id, name).await?;
            }
        }
    } else {
        tracing::info!(target: "ldoom::open", "Fork B proxy running; Ctrl-C to stop");
        tokio::signal::ctrl_c().await.ok();
    }
    Ok(())
}

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

#[cfg(feature = "proxy")]
async fn suspend_idle(aws: &Aws, microvm_id: &str, name: &str) -> Result<()> {
    use anyhow::Context;
    tracing::info!(target: "ldoom::open", "idle timeout reached — auto-suspending {microvm_id}");
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

#[cfg(feature = "proxy")]
fn generate_control_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).context("generating loopback control secret")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(not(feature = "proxy"))]
async fn open_fork_b(args: OpenForkBArgs<'_>) -> Result<()> {
    let _ = (
        args.endpoint,
        args.port,
        args.audio_port,
        args.video_port,
        args.input_port,
        args.h264,
        args.jwe,
        args.no_open,
        args.aws,
        args.microvm_id,
        args.name,
        args.idle_minutes,
    );
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
