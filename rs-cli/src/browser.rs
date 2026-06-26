//! Open a URL in the user's default browser. Thin wrapper over `webbrowser` so
//! `open` (and Fork B) have a single call site.

use anyhow::{Context, Result};

/// Open `url` in the default browser.
pub fn open(url: &str) -> Result<()> {
    tracing::info!(target: "shrink::browser", "opening {url}");
    webbrowser::open(url).with_context(|| format!("failed to open browser at {url}"))?;
    Ok(())
}
