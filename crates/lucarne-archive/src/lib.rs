//! lucarne-archive — the shared terminal-session archive store.
//!
//! When a terminal is archived its captured scrollback is written as one JSON
//! record under `~/.lucarne/term-archive/<archive_id>.json`. The gateway (web
//! archive) and the `term` CLI both use this crate, so archives created from
//! either are visible to the other.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A full archived terminal record (with preserved content).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ArchiveRecord {
    pub archive_id: String,
    pub session_id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub archived_at: u64,
    pub content: String,
}

/// Archive metadata for listings (no content).
#[derive(Serialize, Clone, Debug)]
pub struct ArchiveMeta {
    pub archive_id: String,
    pub session_id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub archived_at: u64,
}

fn dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".lucarne")
        .join("term-archive")
}

/// Persist an archive record; returns its `archive_id`.
pub fn save(
    session_id: &str,
    title: &str,
    cwd: Option<&str>,
    content: &str,
    archived_at: u64,
) -> std::io::Result<String> {
    let d = dir();
    fs::create_dir_all(&d)?;
    let archive_id = format!("{}-{}", session_id.replace([':', '/'], "_"), archived_at);
    let record = ArchiveRecord {
        archive_id: archive_id.clone(),
        session_id: session_id.to_string(),
        title: title.to_string(),
        cwd: cwd.map(str::to_string),
        archived_at,
        content: content.to_string(),
    };
    fs::write(
        d.join(format!("{archive_id}.json")),
        serde_json::to_vec(&record)?,
    )?;
    Ok(archive_id)
}

/// List archived sessions (newest first, without content).
pub fn list() -> Vec<ArchiveMeta> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir()) {
        for entry in rd.flatten() {
            if entry.path().extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else { continue };
            let Ok(rec) = serde_json::from_slice::<ArchiveRecord>(&bytes) else { continue };
            out.push(ArchiveMeta {
                archive_id: rec.archive_id,
                session_id: rec.session_id,
                title: rec.title,
                cwd: rec.cwd,
                archived_at: rec.archived_at,
            });
        }
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.archived_at));
    out
}

/// Read one archive record by id (rejects path-traversal ids).
pub fn get(archive_id: &str) -> Option<ArchiveRecord> {
    if archive_id.contains('/') || archive_id.contains("..") {
        return None;
    }
    let bytes = fs::read(dir().join(format!("{archive_id}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Current unix epoch seconds (archive timestamp helper).
pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
