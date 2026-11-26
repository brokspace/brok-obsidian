//! One sync: walk the remote vault, keep the local copy equal to it within
//! the limits, and never delete anything after an incomplete walk.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Serialize;

use crate::client::{Failure, Note, Vault};
use crate::names::safe_entry;
use crate::settings::Settings;

/// Where the server's version tags are kept, next to (not inside) the mirror.
const TAGS_FILE: &str = "tags.json";

/// What a sync did. Counts only: note names and contents are not reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub written: usize,
    pub unchanged: usize,
    pub removed: usize,
    /// Names refused, over a limit, or too large.
    pub skipped: usize,
    /// Requests that failed; their notes keep the previous copy.
    pub failed: usize,
    /// The whole vault was walked, so notes gone from it were removed.
    pub complete: bool,
}

/// The `exclude` patterns. A pattern matches a note or any folder above it;
/// one without `/` matches a name at any depth.
fn excluder(patterns: &[String]) -> anyhow::Result<GlobSet> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = pattern.trim().trim_matches('/');
        if pattern.is_empty() {
            continue;
        }
        let pattern = if pattern.contains('/') { pattern.to_owned() } else { format!("**/{pattern}") };
        set.add(Glob::new(&pattern).with_context(|| format!("invalid exclude pattern `{pattern}`"))?);
    }
    set.build().context("invalid exclude patterns")
}

/// Walks the vault and updates `data_dir/vault`.
pub async fn sync(vault: &Vault, settings: &Settings, data_dir: &Path) -> anyhow::Result<Report> {
    let exclude = excluder(&settings.exclude)?;
    let root = data_dir.join("vault");
    std::fs::create_dir_all(&root).context("cannot create the mirror folder")?;
    refuse_links(data_dir, &root)?;
    let tags_file = data_dir.join(TAGS_FILE);
    let mut tags: BTreeMap<String, String> =
        std::fs::read(&tags_file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();

    let mut report = Report { complete: true, ..Report::default() };
    // Every note that exists remotely, and the lower-cased names already used.
    let mut remote: BTreeSet<String> = BTreeSet::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut total = 0u64;
    let mut folders: Vec<Vec<String>> = vec![Vec::new()];

    while let Some(folder) = folders.pop() {
        let mut entries = match vault.list(&folder).await {
            Ok(entries) => entries,
            Err(Failure::Denied(why)) => bail!(why),
            Err(Failure::Other(e)) if folder.is_empty() => return Err(e),
            Err(Failure::Other(e)) => {
                tracing::warn!("a folder could not be listed: {e:#}");
                report.failed += 1;
                report.complete = false;
                continue;
            }
        };
        entries.sort();
        for entry in entries {
            let Some((name, is_folder)) = safe_entry(&entry) else {
                // Hidden folders such as `.obsidian` are expected; they are never copied.
                let hidden = entry.starts_with('.')
                    && !entry.starts_with("..")
                    && !entry.trim_end_matches('/').contains(['/', '\\']);
                if !hidden {
                    report.skipped += 1;
                }
                continue;
            };
            let mut parts = folder.clone();
            parts.push(name.to_owned());
            let relative = parts.join("/");
            if exclude.is_match(&relative) || !taken.insert(relative.to_lowercase()) {
                if !exclude.is_match(&relative) {
                    report.skipped += 1;
                }
                continue;
            }
            if is_folder {
                if parts.len() >= settings.max_depth {
                    report.skipped += 1;
                    report.complete = false;
                } else {
                    folders.push(parts);
                }
                continue;
            }
            if !name.to_ascii_lowercase().ends_with(".md") {
                continue;
            }
            if remote.len() >= settings.max_notes || total >= settings.max_total_bytes {
                report.skipped += 1;
                report.complete = false;
                continue;
            }
            remote.insert(relative.clone());
            let target = root.join(parts.iter().collect::<PathBuf>());
            // A tag only counts while the copy it describes is still there.
            let tag = tags.get(&relative).filter(|_| target.is_file()).map(String::as_str);
            let limit = settings.max_note_bytes.min(settings.max_total_bytes - total);
            match vault.note(&parts, tag, limit).await {
                Ok(Note::Unchanged) => {
                    total += std::fs::metadata(&target).map_or(0, |m| m.len());
                    report.unchanged += 1;
                }
                Ok(Note::TooLarge) => {
                    report.skipped += 1;
                    if limit < settings.max_note_bytes {
                        // Only the total budget is used up: keep what is there.
                        report.complete = false;
                    } else {
                        // An older, smaller copy would be stale: drop it.
                        remote.remove(&relative);
                    }
                }
                Ok(Note::Content { bytes, etag }) => {
                    total += bytes.len() as u64;
                    if std::fs::read(&target).is_ok_and(|old| old == bytes) {
                        report.unchanged += 1;
                    } else if let Err(e) = write(&root, &target, &bytes) {
                        tracing::warn!("a note could not be written: {e:#}");
                        report.failed += 1;
                        continue;
                    } else {
                        report.written += 1;
                    }
                    match etag {
                        Some(etag) => tags.insert(relative, etag),
                        None => tags.remove(&relative),
                    };
                }
                Err(Failure::Denied(why)) => bail!(why),
                Err(Failure::Other(e)) => {
                    tracing::warn!("a note could not be fetched: {e:#}");
                    report.failed += 1;
                }
            }
        }
    }

    if report.complete {
        report.removed = prune(&root, &root, &remote)?;
        tags.retain(|relative, _| remote.contains(relative));
    }
    write(data_dir, &tags_file, &serde_json::to_vec(&tags)?)?;
    Ok(report)
}

/// Refuses a path with a symbolic link between `root` and `target`, so a write
/// cannot be redirected outside the mirror.
fn refuse_links(root: &Path, target: &Path) -> anyhow::Result<()> {
    let inside = target.strip_prefix(root).context("a path outside the mirror")?;
    let mut path = root.to_path_buf();
    for part in inside.components() {
        path.push(part);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("the mirror contains a symbolic link, which is never followed");
        }
    }
    Ok(())
}

/// Writes through a temporary file, so an interrupted run leaves the old copy.
fn write(root: &Path, target: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    refuse_links(root, target)?;
    let parent = target.parent().context("a note without a folder")?;
    std::fs::create_dir_all(parent)?;
    // The name starts with a dot, which no note can, and does not end in `.md`.
    let temporary = parent.join(".brok-obsidian.tmp");
    let _ = std::fs::remove_file(&temporary);
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, target)?;
    Ok(())
}

/// Removes mirrored notes that are gone from the vault, and folders left empty.
fn prune(root: &Path, dir: &Path, keep: &BTreeSet<String>) -> anyhow::Result<usize> {
    let mut removed = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            removed += prune(root, &path, keep)?;
            let _ = std::fs::remove_dir(&path); // only succeeds when empty
        } else {
            let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
            if !keep.contains(&relative) {
                std::fs::remove_file(&path)?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}
