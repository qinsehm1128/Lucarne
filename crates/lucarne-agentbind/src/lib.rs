//! lucarne-agentbind — bind an rmux pane (by cwd) to the agent session running
//! inside it, and read that session's transcript as chat messages.
//!
//! An agent launched interactively in a pane (`claude`, `codex`) gives us no
//! structured stream, but it writes a transcript file under a cwd-keyed folder:
//! `~/.claude/projects/<sanitized-cwd>/<session-uuid>.jsonl`. Given the pane's
//! cwd we resolve that folder, pick the most-recently-active transcript, and
//! parse it into `{role, text}` bubbles. This proves the binding the user asked
//! about without any FS tracing (cwd is enough; `lsof` is an optional precision
//! upgrade). The robust upstream form would reuse Lucarne's `agent-sessions`
//! parser instead of this minimal claude reader.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The agent session a pane is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundAgent {
    /// Agent family, e.g. "claude".
    pub kind: String,
    /// The agent's own session id (transcript file stem).
    pub session_id: String,
    /// Path to the transcript file.
    pub transcript: PathBuf,
}

/// One parsed conversation turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Msg {
    /// "user" or "assistant".
    pub role: String,
    pub text: String,
}

/// Claude's project-folder name for a cwd: every non-alphanumeric byte → '-'
/// (matches the on-disk `~/.claude/projects/<name>` layout).
pub fn sanitize_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Resolve the agent session bound to a pane with the given cwd, if any.
/// (Claude today — cwd-keyed and deterministic. Codex is date-keyed and would
/// require scanning meta.cwd; left for the agent-sessions integration.)
pub fn bind(cwd: &str) -> Option<BoundAgent> {
    let dir = dirs::home_dir()?
        .join(".claude")
        .join("projects")
        .join(sanitize_cwd(cwd));
    if !dir.is_dir() {
        return None;
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().map_or(true, |(best_m, _)| modified > *best_m) {
            best = Some((modified, path));
        }
    }
    let (_, transcript) = best?;
    let session_id = transcript.file_stem()?.to_string_lossy().into_owned();
    Some(BoundAgent {
        kind: "claude".to_string(),
        session_id,
        transcript,
    })
}

/// Read transcript messages appended since byte `from`, returning the new
/// messages and the new byte offset. An incomplete trailing line (no `\n` yet)
/// is left unconsumed so the next poll re-reads it whole. Pass `from = 0` to
/// read the whole transcript.
pub fn read_messages(path: &Path, from: u64) -> (Vec<Msg>, u64) {
    let Ok(mut file) = File::open(path) else {
        return (Vec::new(), from);
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= from {
        return (Vec::new(), from);
    }
    if file.seek(SeekFrom::Start(from)).is_err() {
        return (Vec::new(), from);
    }
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return (Vec::new(), from);
    }

    let mut msgs = Vec::new();
    let mut consumed = from;
    for line in buf.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break; // incomplete tail — re-read next time
        }
        consumed += line.len() as u64;
        if let Some(m) = parse_line(line.trim_end()) {
            msgs.push(m);
        }
    }
    (msgs, consumed)
}

/// Parse one claude transcript jsonl line into a `Msg` (user/assistant text only).
fn parse_line(line: &str) -> Option<Msg> {
    let v: Value = serde_json::from_str(line).ok()?;
    let kind = v.get("type")?.as_str()?;
    if kind != "user" && kind != "assistant" {
        return None;
    }
    let message = v.get("message")?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or(kind)
        .to_string();
    let text = match message.get("content")? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(Value::as_str) == Some("text") {
                    b.get("text").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(Msg { role, text })
}

/// A recently-active claude transcript across all projects (for chat history).
#[derive(Clone, Debug)]
pub struct RecentSession {
    pub kind: String,
    pub session_id: String,
    /// The `~/.claude/projects/<project>` directory name.
    pub project: String,
    /// Last-modified time (epoch seconds).
    pub modified: u64,
}

/// List the most-recently-active claude transcripts across all projects.
pub fn recent_sessions(limit: usize) -> Vec<RecentSession> {
    let Some(root) = dirs::home_dir().map(|h| h.join(".claude").join("projects")) else {
        return Vec::new();
    };
    let mut all = Vec::new();
    if let Ok(projects) = std::fs::read_dir(&root) {
        for proj in projects.flatten() {
            if !proj.path().is_dir() {
                continue;
            }
            let project = proj.file_name().to_string_lossy().into_owned();
            if let Ok(files) = std::fs::read_dir(proj.path()) {
                for f in files.flatten() {
                    let p = f.path();
                    if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let modified = f
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let session_id = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    all.push(RecentSession {
                        kind: "claude".to_string(),
                        session_id,
                        project: project.clone(),
                        modified,
                    });
                }
            }
        }
    }
    all.sort_by(|a, b| b.modified.cmp(&a.modified));
    all.truncate(limit);
    all
}

/// Resolve a transcript path from a (project, session) pair, validated to stay
/// inside `~/.claude/projects` (single path components, no traversal).
pub fn history_transcript(project: &str, session: &str) -> Option<PathBuf> {
    if [project, session]
        .iter()
        .any(|c| c.contains('/') || c.contains("..") || c.is_empty())
    {
        return None;
    }
    let path = dirs::home_dir()?
        .join(".claude")
        .join("projects")
        .join(project)
        .join(format!("{session}.jsonl"));
    path.is_file().then_some(path)
}

/// A short title for a transcript: the first non-empty line of its first user
/// message (bounded head read). Used to make bound/history sessions
/// distinguishable in the chat picker.
pub fn first_user_message(path: &Path) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let mut buf = vec![0u8; 16 * 1024];
    let n = f.read(&mut buf).ok()?;
    for line in String::from_utf8_lossy(&buf[..n]).split('\n') {
        if let Some(m) = parse_line(line) {
            if m.role == "user" {
                let first = m.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                let s: String = first.chars().take(60).collect();
                return (!s.trim().is_empty()).then_some(s);
            }
        }
    }
    None
}

/// SQLite-backed registry of agent sessions that have been observed bound to an
/// rmux pane. Chat history reads ONLY from here — it never scans the whole
/// `~/.claude` tree — so unrelated local agent sessions are never exposed.
pub mod db {
    use std::path::PathBuf;

    use rusqlite::Connection;

    fn path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".lucarne")
            .join("agents.db")
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn open() -> Option<Connection> {
        let p = path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(p).ok()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS rmux_agent_sessions (
                agent_session_id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                cwd TEXT,
                rmux_session TEXT,
                title TEXT,
                transcript TEXT,
                first_seen INTEGER NOT NULL,
                last_seen INTEGER NOT NULL
            );",
        )
        .ok()?;
        // Additive migration: older DBs may lack `summary`.
        let _ = conn.execute("ALTER TABLE rmux_agent_sessions ADD COLUMN summary TEXT", []);
        Some(conn)
    }

    /// Record (upsert) that `session_id` was seen bound to an rmux pane.
    pub fn record(
        kind: &str,
        session_id: &str,
        cwd: &str,
        rmux_session: &str,
        title: &str,
        transcript: &str,
    ) {
        let Some(conn) = open() else { return };
        let ts = now();
        let summary = super::first_user_message(std::path::Path::new(transcript));
        let _ = conn.execute(
            "INSERT INTO rmux_agent_sessions
                (agent_session_id, kind, cwd, rmux_session, title, transcript, summary, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(agent_session_id) DO UPDATE SET
                cwd = ?3, rmux_session = ?4, title = ?5, transcript = ?6, summary = ?7, last_seen = ?8",
            rusqlite::params![session_id, kind, cwd, rmux_session, title, transcript, summary, ts],
        );
    }

    /// One row of rmux-related agent-session history.
    #[derive(Clone, Debug)]
    pub struct HistRow {
        pub kind: String,
        pub session_id: String,
        pub cwd: Option<String>,
        pub rmux_session: Option<String>,
        pub title: Option<String>,
        pub summary: Option<String>,
        pub last_seen: i64,
    }

    /// Most-recently-seen rmux-related agent sessions (newest first).
    pub fn history(limit: usize) -> Vec<HistRow> {
        let Some(conn) = open() else { return Vec::new() };
        let mut rows = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT kind, agent_session_id, cwd, rmux_session, title, summary, last_seen
             FROM rmux_agent_sessions ORDER BY last_seen DESC LIMIT ?1",
        ) {
            if let Ok(mapped) = stmt.query_map([limit as i64], |r| {
                Ok(HistRow {
                    kind: r.get(0)?,
                    session_id: r.get(1)?,
                    cwd: r.get(2)?,
                    rmux_session: r.get(3)?,
                    title: r.get(4)?,
                    summary: r.get(5)?,
                    last_seen: r.get(6)?,
                })
            }) {
                rows.extend(mapped.flatten());
            }
        }
        rows
    }

    /// The recorded transcript path for a session (for read-only history view).
    pub fn transcript_path(session_id: &str) -> Option<String> {
        let conn = open()?;
        conn.query_row(
            "SELECT transcript FROM rmux_agent_sessions WHERE agent_session_id = ?1",
            [session_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sanitize_matches_claude_layout() {
        assert_eq!(
            sanitize_cwd("/Users/qs/project/me/rmux_remote_control"),
            "-Users-qs-project-me-rmux-remote-control"
        );
    }

    #[test]
    fn parses_user_and_assistant_text_blocks() {
        let user = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi?"}]}}"#;
        let asst = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi!"}]}}"#;
        let other = r#"{"type":"queue-operation"}"#;
        assert_eq!(parse_line(user), Some(Msg { role: "user".into(), text: "hi?".into() }));
        assert_eq!(parse_line(asst), Some(Msg { role: "assistant".into(), text: "Hi!".into() }));
        assert_eq!(parse_line(other), None);
    }

    #[test]
    fn read_messages_is_incremental_and_skips_partial_tail() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let l1 = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"a\"}]}}\n";
        f.write_all(l1.as_bytes()).unwrap();
        f.flush().unwrap();
        let (msgs, off) = read_messages(f.path(), 0);
        assert_eq!(msgs.len(), 1);
        assert_eq!(off, l1.len() as u64);

        // append a complete line + a partial (no newline) line
        let l2 = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"b\"}]}}\n";
        let partial = "{\"type\":\"user\",\"message\":{\"rol";
        f.write_all(l2.as_bytes()).unwrap();
        f.write_all(partial.as_bytes()).unwrap();
        f.flush().unwrap();
        let (msgs2, off2) = read_messages(f.path(), off);
        assert_eq!(msgs2.len(), 1);
        assert_eq!(msgs2[0].text, "b");
        assert_eq!(off2, off + l2.len() as u64); // partial tail NOT consumed
    }
}
