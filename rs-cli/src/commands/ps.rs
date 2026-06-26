//! `ldoom ps` — print the capsule table from state.json. With `--refresh`,
//! call `list_microvms` and reconcile the live state back into the ledger.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::aws::Aws;
use crate::config::Config;
use crate::state::State;

pub async fn run(refresh: bool) -> Result<()> {
    let mut state = State::load()?;

    if refresh {
        let cfg = Config::load()?;
        let aws = Aws::new(&cfg).await?;
        let live = aws
            .microvm
            .list_microvms()
            .send()
            .await
            .context("list_microvms")?;
        // Index live records by microvm_id (the list carries id + state, not endpoint).
        let by_id: HashMap<&str, &_> = live.items().iter().map(|m| (m.microvm_id(), m)).collect();

        let names: Vec<String> = state.capsules.keys().cloned().collect();
        for name in names {
            let id = state.get(&name).and_then(|c| c.microvm_id.clone());
            if let Some(id) = id {
                match by_id.get(id.as_str()) {
                    Some(item) => {
                        let st = item.state().as_str().to_string();
                        state.upsert(&name, |c| {
                            c.state = Some(st.clone());
                        })?;
                    }
                    None => {
                        // Live API doesn't know it anymore → gone.
                        state.upsert(&name, |c| {
                            c.microvm_id = None;
                            c.endpoint = None;
                            c.state = Some("TERMINATED".to_string());
                        })?;
                    }
                }
            }
        }
    }

    print_table(&state);
    Ok(())
}

fn print_table(state: &State) {
    if state.capsules.is_empty() {
        println!("no capsules yet — `ldoom build --name <name>`");
        return;
    }
    println!("{:<16} {:<12} {:<22} ENDPOINT", "NAME", "STATE", "MICROVM");
    for (name, c) in &state.capsules {
        println!(
            "{:<16} {:<12} {:<22} {}",
            name,
            c.state.as_deref().unwrap_or("-"),
            c.microvm_id.as_deref().unwrap_or("-"),
            c.endpoint.as_deref().unwrap_or("-"),
        );
    }
}
