//! A Brokspace shard that mirrors a remote Obsidian vault, read only, into
//! `.brokspace/shards-data/brok-obsidian/vault`, where brok can search it
//! (`memory.obsidian` with `search: vault` and `read_only: true`).
//!
//! Design and threat model: brok's ADR 0016.

mod client;
mod mirror;
mod names;
mod payload;
mod settings;
#[cfg(test)]
mod tests;

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use brok_sdk::prelude::*;
use tokio::time::MissedTickBehavior;
use tracing_subscriber::EnvFilter;

use crate::client::Vault;
use crate::payload::{SyncRequest, Synced};
use crate::settings::{API_KEY_ENV, MIRROR_DIR, Settings};

/// Must match `name` in shard.json.
const NAME: &str = env!("CARGO_PKG_NAME");

#[tokio::main]
async fn main() -> ExitCode {
    // Logs go to stderr; `brok run` captures them line by line.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let (client, mut inbox) =
        BrokClient::connect_from_env(NAME, env!("CARGO_PKG_VERSION")).await.context("cannot join the brok bus")?;
    let root = client.context().workspace_root.clone();
    let settings = Settings::from_value(&client.context().settings)?;
    client.subscribe([TopicPattern::new(SyncRequest::TOPIC)?]).await?;
    tracing::info!(mirror = MIRROR_DIR, "{NAME} ready");

    let every = settings.interval_secs;
    // The first tick is immediate: a sync at start.
    let mut timer = tokio::time::interval(Duration::from_secs(if every == 0 { 3600 } else { every.max(60) }));
    timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut first = true;
    loop {
        tokio::select! {
            event = inbox.recv() => match event {
                None => break,
                Some(event) if event.is::<Shutdown>() => break,
                Some(event) if event.is::<SyncRequest>() => report(&client, &root, &settings).await?,
                Some(event) => tracing::debug!(topic = %event.topic, "ignoring event"),
            },
            _ = timer.tick() => if first || every > 0 {
                first = false;
                report(&client, &root, &settings).await?;
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

/// Runs a sync, logs it and publishes the counts. A failed sync keeps the
/// previous copy and the shard keeps running.
async fn report(client: &BrokClient, root: &Path, settings: &Settings) -> anyhow::Result<()> {
    let outcome = Synced::from(once(root, settings).await);
    match &outcome.error {
        None => tracing::info!(
            written = outcome.written,
            unchanged = outcome.unchanged,
            removed = outcome.removed,
            skipped = outcome.skipped,
            failed = outcome.failed,
            complete = outcome.complete,
            "vault synced"
        ),
        Some(error) => tracing::warn!("sync failed, the previous copy is kept: {error}"),
    }
    client.emit(&outcome).await?;
    Ok(())
}

async fn once(root: &Path, settings: &Settings) -> anyhow::Result<mirror::Report> {
    let key = std::env::var(API_KEY_ENV)
        .ok()
        .filter(|k| !k.trim().is_empty())
        .with_context(|| format!("set {API_KEY_ENV} to the plugin's API key before `brok run`"))?;
    let ca = settings.ca_path(root)?;
    let ca = std::fs::read(&ca).with_context(|| format!("cannot read `ca_file` {}", ca.display()))?;
    let vault = Vault::new(settings, &ca, &key)?;
    let data = root.join(MIRROR_DIR);
    let data = data.parent().context("the mirror has no parent folder")?;
    mirror::sync(&vault, settings, data).await
}
