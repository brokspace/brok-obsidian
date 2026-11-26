//! The shard's events.

use brok_sdk::Payload;
use serde::{Deserialize, Serialize};

use crate::mirror::Report;

/// Asks for a sync now.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncRequest {}

impl Payload for SyncRequest {
    const TOPIC: &'static str = "obsidian:sync";
}

/// The result of a sync: counts, and why it failed if it did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Synced {
    pub ok: bool,
    pub written: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Payload for Synced {
    const TOPIC: &'static str = "obsidian:synced";
}

impl From<anyhow::Result<Report>> for Synced {
    fn from(outcome: anyhow::Result<Report>) -> Self {
        let (report, error) = match outcome {
            Ok(report) => (report, None),
            Err(e) => (Report::default(), Some(format!("{e:#}"))),
        };
        Self {
            ok: error.is_none(),
            written: report.written,
            unchanged: report.unchanged,
            removed: report.removed,
            skipped: report.skipped,
            failed: report.failed,
            complete: report.complete,
            error,
        }
    }
}
