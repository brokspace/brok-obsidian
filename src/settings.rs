//! `shards.brok-obsidian.settings` in `brok.yml`. The API key is never here:
//! it is read from the environment variable `OBSIDIAN_API_KEY`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use reqwest::Url;
use serde::Deserialize;

/// The one environment variable the key is read from (granted in `shard.json`).
pub const API_KEY_ENV: &str = "OBSIDIAN_API_KEY";
/// The mirror, relative to the workspace root.
pub const MIRROR_DIR: &str = ".brokspace/shards-data/brok-obsidian/vault";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// The plugin's HTTPS address.
    pub url: String,
    /// The plugin's certificate authority (PEM), relative to the workspace or absolute.
    pub ca_file: Option<String>,
    /// Folders, files or globs of the vault that are never downloaded.
    pub exclude: Vec<String>,
    /// Seconds between syncs; 0 syncs only at start and on request.
    pub interval_secs: u64,
    /// Seconds one request may take.
    pub timeout_secs: u64,
    pub max_notes: usize,
    pub max_note_bytes: u64,
    pub max_total_bytes: u64,
    /// How deep folders are followed.
    pub max_depth: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            url: "https://127.0.0.1:27124".into(),
            ca_file: None,
            exclude: Vec::new(),
            interval_secs: 900,
            timeout_secs: 20,
            max_notes: 20_000,
            max_note_bytes: 512 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
            max_depth: 16,
        }
    }
}

impl Settings {
    pub fn from_value(value: &serde_json::Value) -> anyhow::Result<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone()).context("invalid shards.brok-obsidian.settings")
    }

    /// The server address: HTTPS only, and nothing but a host and a port, so
    /// the key is only ever sent where the user wrote.
    pub fn base_url(&self) -> anyhow::Result<Url> {
        let url = Url::parse(self.url.trim()).context("`url` is not a valid address")?;
        if url.scheme() != "https" {
            bail!("`url` must start with https:// (the key is never sent over plain HTTP)");
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("`url` must not contain a user or a password: set {API_KEY_ENV} instead");
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() || url.host_str().is_none() {
            bail!("`url` must be only the host and port, such as https://127.0.0.1:27124");
        }
        Ok(url)
    }

    pub fn ca_path(&self, root: &Path) -> anyhow::Result<PathBuf> {
        let file = self.ca_file.as_deref().map(str::trim).filter(|f| !f.is_empty()).context(
            "set `ca_file` to the plugin's certificate (download it from <url>/obsidian-local-rest-api.crt)",
        )?;
        Ok(root.join(file))
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_https_host_is_accepted() {
        let with = |url: &str| Settings { url: url.into(), ..Settings::default() }.base_url();
        assert!(with("https://127.0.0.1:27124").is_ok());
        assert!(with("https://vault.tailnet.ts.net:27124/").is_ok());
        for bad in [
            "http://127.0.0.1:27123",
            "https://u:p@host",
            "https://host/vault",
            "https://host?x=1",
            "ftp://host",
            "host",
        ] {
            assert!(with(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn unknown_settings_are_refused_and_a_certificate_is_required() {
        assert!(Settings::from_value(&serde_json::json!({ "insecure": true })).is_err());
        assert!(Settings::default().ca_path(Path::new("/w")).is_err());
    }
}
