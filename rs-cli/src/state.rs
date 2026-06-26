//! `~/.lambdadoom/state.json` — the mutable, per-capsule ledger keyed by name.
//! There is no server-side list API for *our* known capsules, so this file is
//! the source of truth `ps` reads and every command updates.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::shrink_dir;

/// One capsule's record. Every field is optional past the name because a capsule
/// exists in stages: built (image only) → up (microvm) → torn down.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Capsule {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image_arn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub microvm_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub endpoint: Option<String>,
    /// Last known lifecycle state string (e.g. CREATED, RUNNING, SUSPENDED).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub state: Option<String>,
}

/// The whole ledger: name -> capsule. BTreeMap keeps `ps` output stable/sorted.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub capsules: BTreeMap<String, Capsule>,
}

impl State {
    /// `~/.lambdadoom/state.json`.
    pub fn path() -> Result<PathBuf> {
        Ok(shrink_dir()?.join("state.json"))
    }

    /// Load the ledger, returning an empty one if the file doesn't exist yet.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Persist the ledger (pretty JSON so it's hand-inspectable).
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("serializing state")?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Borrow a capsule by name.
    pub fn get(&self, name: &str) -> Option<&Capsule> {
        self.capsules.get(name)
    }

    /// Borrow a capsule by name, erroring with a hint if unknown.
    pub fn require(&self, name: &str) -> Result<&Capsule> {
        self.get(name).with_context(|| {
            format!("no capsule named '{name}' in state — run `ldoom build --name {name}` first")
        })
    }

    /// Apply a mutation to `name`'s record (creating it if absent) and save.
    pub fn upsert(&mut self, name: &str, f: impl FnOnce(&mut Capsule)) -> Result<()> {
        let entry = self.capsules.entry(name.to_string()).or_default();
        f(entry);
        self.save()
    }

    /// Drop a capsule from the ledger entirely (used by `rm` after the image is deleted).
    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.capsules.remove(name);
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Runnable check: upsert is additive — later writes don't clobber unrelated
    // fields. (`cargo test` covers the merge semantics `up`/`build` rely on.)
    #[test]
    fn upsert_merges_fields() {
        let mut s = State::default();
        s.capsules.entry("demo".into()).or_default().image_arn = Some("arn:image".into());
        {
            let c = s.capsules.entry("demo".into()).or_default();
            c.microvm_id = Some("mvm-1".into());
        }
        let c = s.get("demo").unwrap();
        assert_eq!(c.image_arn.as_deref(), Some("arn:image"));
        assert_eq!(c.microvm_id.as_deref(), Some("mvm-1"));
    }
}
