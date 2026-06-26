//! `ldoom config` — view and change persistent settings in `~/.lambdadoom/config.toml`.
//!
//! Settings are the "just works, or doesn't, based on config" knobs — no flags to
//! remember. The CloudFormation-emitted fields (bucket, role ARNs, …) are shown but
//! not meant to be hand-edited here.

use anyhow::{Result, bail};

use crate::config::Config;

/// The user-tunable setting keys (everything else in config.toml is CloudFormation output).
const TUNABLE_KEYS: &[&str] = &["idle_suspend_minutes", "display"];

/// `ldoom config show` — print the resolved settings.
pub fn show() -> Result<()> {
    let cfg = Config::load()?;
    println!("settings ({}):", Config::path()?.display());
    println!("  region            = {}", cfg.region);
    println!("  port              = {}", cfg.port);
    println!("  audio_port        = {}", cfg.audio_port);
    match cfg.idle_suspend_minutes {
        Some(m) if m > 0 => println!("  idle_suspend_minutes  = {m}  (auto-suspend ON)"),
        _ => println!(
            "  idle_suspend_minutes  = <unset>  (auto-suspend off; platform IdlePolicy ~5 min still applies)"
        ),
    }
    match cfg.display.as_deref() {
        Some("h264") => println!("  display               = h264 (H.264/WebCodecs)"),
        _ => println!("  display               = vnc (noVNC, default)"),
    }
    Ok(())
}

/// `ldoom config set <key> <value>`.
pub fn set(key: &str, value: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    match key {
        "idle_suspend_minutes" => {
            let mins: u64 = value.trim().parse().map_err(|_| {
                anyhow::anyhow!("idle_suspend_minutes must be a whole number of minutes")
            })?;
            cfg.idle_suspend_minutes = Some(mins);
            cfg.save()?;
            if mins == 0 {
                println!(
                    "set idle_suspend_minutes = 0  (auto-suspend off; platform IdlePolicy still applies)"
                );
            } else {
                println!(
                    "set idle_suspend_minutes = {mins}  (`ldoom open` auto-suspends after {mins} idle min)"
                );
            }
        }
        "display" => {
            let v = value.trim().to_ascii_lowercase();
            match v.as_str() {
                "vnc" => {
                    cfg.display = None;
                    cfg.save()?;
                    println!(
                        "set display = vnc  (noVNC, default — `ldoom open` opens the plain URL)"
                    );
                }
                "h264" => {
                    cfg.display = Some("h264".to_string());
                    cfg.save()?;
                    println!(
                        "set display = h264  (H.264/WebCodecs — `ldoom open` opens with ?display=h264)"
                    );
                }
                _ => bail!(
                    "display must be 'vnc' (noVNC, default) or 'h264' (H.264/WebCodecs); got '{value}'"
                ),
            }
        }
        other => unknown_key(other)?,
    }
    Ok(())
}

/// `ldoom config unset <key>` — clear an optional setting.
pub fn unset(key: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    match key {
        "idle_suspend_minutes" => {
            cfg.idle_suspend_minutes = None;
            cfg.save()?;
            println!(
                "unset idle_suspend_minutes  (auto-suspend off; platform IdlePolicy ~5 min still applies)"
            );
        }
        "display" => {
            cfg.display = None;
            cfg.save()?;
            println!(
                "unset display  (back to vnc/noVNC default — `ldoom open` opens the plain URL)"
            );
        }
        other => unknown_key(other)?,
    }
    Ok(())
}

fn unknown_key(key: &str) -> Result<()> {
    bail!(
        "unknown setting '{key}'. Tunable keys: {}",
        TUNABLE_KEYS.join(", ")
    )
}
