//! lucarne-agentbind — bind an rmux pane (by cwd) to the agent session running
//! inside it, and read that session's transcript as chat messages.
//!
//! An agent launched interactively in a pane (`claude`, `codex`) gives us no
//! structured stream, but it writes a transcript file under a provider-owned
//! layout. This crate does NOT know any provider's on-disk layout, file format,
//! or parsing rules: it routes every discovery / metadata / transcript request
//! through the [`agent_sessions`] provider descriptor contract (the provider
//! boundary mandated by `AGENTS.md`). Given a pane's cwd we ask each provider to
//! discover its sessions, read each session's `cwd` from the provider-parsed
//! metadata, pick the most-recently-active session whose `cwd` matches, and read
//! its transcript into `{role, text}` bubbles — all via provider descriptors, so
//! provider ids, discovery roots, file formats, and parse rules stay owned by the
//! provider layer.

use std::path::{Path, PathBuf};

use agent_sessions::agent_session::{Actor, Body, ContentBlock, Session, SessionMeta};
use agent_sessions::reader::SessionReader;
use agent_sessions::{AgentProviderDescriptor, ParseSelection, agent_providers};

/// The agent session a pane is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundAgent {
    /// Provider id, e.g. "claude" — owned by the provider descriptor, never a
    /// literal hardcoded here.
    pub kind: String,
    /// The agent's own session id (provider-parsed).
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

/// Resolve the agent session bound to a pane with the given cwd, if any.
///
/// Routes through the [`agent_sessions`] provider descriptors: for each provider
/// we discover its sessions, read each session's provider-parsed `cwd`, keep the
/// ones that match the pane cwd, and return the most-recently-active match. The
/// provider id (`kind`), discovery roots, and metadata parsing all stay owned by
/// the provider layer — this function never names a concrete provider, layout, or
/// file format.
pub fn bind(cwd: &str) -> Option<BoundAgent> {
    let mut best: Option<(i64, BoundAgent)> = None;
    for provider in agent_providers() {
        let _ = provider.discover_sources_into(&mut |source| {
            let Ok(meta) = provider.parse_source_meta(&source) else {
                return;
            };
            if !meta_cwd_matches(&meta, cwd) {
                return;
            }
            let modified = source.last_modified_unix();
            if best.as_ref().is_some_and(|(best_m, _)| modified <= *best_m) {
                return;
            }
            let session_id = meta
                .session_id
                .as_deref()
                .map(str::to_owned)
                .or_else(|| {
                    source
                        .path()
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            best = Some((
                modified,
                BoundAgent {
                    kind: provider.id().to_string(),
                    session_id,
                    transcript: source.path().to_path_buf(),
                },
            ));
        });
    }
    best.map(|(_, agent)| agent)
}

/// True when a provider-parsed session's `cwd` matches the pane cwd.
fn meta_cwd_matches(meta: &SessionMeta, cwd: &str) -> bool {
    meta.cwd.as_deref() == Some(cwd)
}

/// Read transcript messages appended since byte `from`, returning the new
/// messages and the new byte offset. An incomplete trailing line (no `\n` yet)
/// is left unconsumed so the next poll re-reads it whole. Pass `from = 0` to
/// read the whole transcript.
///
/// Parsing is delegated to the [`agent_sessions`] provider that owns this file:
/// we read only the complete lines appended since `from` (a bounded window that
/// starts on a line boundary), then hand that byte window to the provider's
/// descriptor parser. The jsonl framing (line boundaries) is the only thing this
/// layer reasons about; the message schema is parsed by the provider.
pub fn read_messages(path: &Path, from: u64) -> (Vec<Msg>, u64) {
    let Some((bytes, consumed)) = read_complete_lines_from(path, from) else {
        return (Vec::new(), from);
    };
    if bytes.is_empty() {
        return (Vec::new(), consumed);
    }
    let Some(provider) = provider_for(path) else {
        return (Vec::new(), consumed);
    };
    match provider.parse_agent_session_bytes(bytes, ParseSelection::empty().with_messages()) {
        Ok(session) => (messages_from_session(provider, &session), consumed),
        Err(_) => (Vec::new(), consumed),
    }
}

/// Read the complete lines (each terminated by `\n`) appended to `path` since
/// byte `from`, returning the assembled bytes plus the new consumed offset. A
/// trailing partial line (no newline yet) is left unconsumed. Returns `None`
/// only when the file cannot be opened/read; an unchanged or shrunk file yields
/// `Some((empty, from))`.
fn read_complete_lines_from(path: &Path, from: u64) -> Option<(Vec<u8>, u64)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len <= from {
        return Some((Vec::new(), from));
    }
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;

    // Keep only complete lines; leave any partial trailing line unconsumed so the
    // next poll re-reads it whole.
    let consumed_len = match buf.iter().rposition(|byte| *byte == b'\n') {
        Some(last_newline) => last_newline + 1,
        None => return Some((Vec::new(), from)), // no complete line yet
    };
    buf.truncate(consumed_len);
    Some((buf, from + consumed_len as u64))
}

/// Project a provider-parsed [`Session`] into user/assistant chat bubbles, using
/// only the provider-driven visibility rule and the shared text projection
/// helpers from [`agent_sessions`] — no provider-specific branching here.
fn messages_from_session(provider: AgentProviderDescriptor, session: &Session) -> Vec<Msg> {
    let mut msgs = Vec::new();
    for event in session.events.iter() {
        let msg = match (&event.actor, &event.body) {
            (Actor::User, Body::Prompt(prompt)) => {
                let text = text_of(prompt.text.as_deref(), &prompt.blocks);
                if text.is_empty() || !provider.is_transcript_user_text_visible(&text) {
                    continue;
                }
                Msg {
                    role: "user".to_string(),
                    text,
                }
            }
            (Actor::Assistant, Body::Response(response)) => {
                let text = text_of(response.text.as_deref(), &response.blocks);
                if text.is_empty() {
                    continue;
                }
                Msg {
                    role: "assistant".to_string(),
                    text,
                }
            }
            _ => continue,
        };
        msgs.push(msg);
    }
    msgs
}

/// Resolve a trimmed display text from a prompt/response's inline text or its
/// content blocks, preferring the explicit text field.
fn text_of(inline: Option<&str>, blocks: &[ContentBlock]) -> String {
    if let Some(text) = inline.map(str::trim).filter(|text| !text.is_empty()) {
        return text.to_string();
    }
    agent_sessions::agent_session::text_from_blocks(blocks)
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

/// Find the [`agent_sessions`] provider descriptor that owns `path` by asking
/// each provider to parse the file's metadata; the provider that succeeds owns
/// it. Detection (file format / schema) stays inside the provider layer.
fn provider_for(path: &Path) -> Option<AgentProviderDescriptor> {
    agent_providers()
        .into_iter()
        .find(|provider| provider.parse_file_meta(path.to_path_buf()).is_ok())
}

/// A short title for a transcript: the provider-parsed session title, falling
/// back to the first visible user message text (bounded window read). Used to
/// make bound/history sessions distinguishable in the chat picker.
pub fn first_user_message(path: &Path) -> Option<String> {
    let provider = provider_for(path)?;
    if let Ok(meta) = provider.parse_file_meta(path.to_path_buf()) {
        if let Some(title) = meta.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            return Some(title.chars().take(60).collect());
        }
    }
    // Fallback: read a bounded window from the end and take the first visible
    // user message text. The window read + jsonl framing is bounded; the schema
    // parsing stays in the provider.
    let bytes = read_tail_window(path)?;
    let session = provider
        .parse_agent_session_bytes(bytes, ParseSelection::empty().with_messages())
        .ok()?;
    session.events.iter().find_map(|event| match (&event.actor, &event.body) {
        (Actor::User, Body::Prompt(prompt)) => {
            let text = text_of(prompt.text.as_deref(), &prompt.blocks);
            if text.is_empty() || !provider.is_transcript_user_text_visible(&text) {
                return None;
            }
            let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            let s: String = first.chars().take(60).collect();
            (!s.trim().is_empty()).then_some(s)
        }
        _ => None,
    })
}

/// Bounded reverse read of complete lines from the head of a transcript, large
/// enough to capture the first user turn, assembled in forward order. Reads at
/// most a bounded number of bytes (never the whole file).
fn read_tail_window(path: &Path) -> Option<Vec<u8>> {
    const MAX_HEAD_BYTES: u64 = 256 * 1024;
    let reader = SessionReader::open(path).ok()?;
    // Read complete lines from the start by limiting the reverse reader to the
    // file's head window, then reversing into forward order.
    let mut lines = reader.reverse_lines_limited(MAX_HEAD_BYTES).ok()?;
    let mut collected = Vec::new();
    while let Some(line) = lines.next_line().ok()? {
        collected.push(line);
    }
    if collected.is_empty() {
        return None;
    }
    let mut bytes = Vec::new();
    for line in collected.iter().rev() {
        bytes.extend_from_slice(line);
        bytes.push(b'\n');
    }
    Some(bytes)
}

/// SQLite-backed registry of agent sessions that have been observed bound to an
/// rmux pane. Chat history reads ONLY from here — it never scans a provider's
/// whole transcript tree — so unrelated local agent sessions are never exposed.
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

    /// A minimal Claude-format transcript line carrying cwd + a user message.
    fn claude_session_line(cwd: &str, session_id: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"{session_id}","cwd":"{cwd}","timestamp":"2026-05-30T00:00:00Z","message":{{"role":"user","content":[{{"type":"text","text":"hello there"}}]}}}}"#
        )
    }

    fn claude_assistant_line() -> String {
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi back"}],"stop_reason":"end_turn"}}"#.to_string()
    }

    #[test]
    fn read_messages_projects_user_and_assistant_via_provider() {
        let mut f = tempfile::Builder::new().suffix(".jsonl").tempfile().unwrap();
        writeln!(f, "{}", claude_session_line("/tmp/x", "sess-1")).unwrap();
        writeln!(f, "{}", claude_assistant_line()).unwrap();
        f.flush().unwrap();

        let (msgs, off) = read_messages(f.path(), 0);
        assert!(off > 0, "consumed offset advances past complete lines");
        assert_eq!(msgs.len(), 2, "one user + one assistant bubble");
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].text, "hello there");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].text, "hi back");
    }

    #[test]
    fn read_messages_leaves_partial_trailing_line_unconsumed() {
        let mut f = tempfile::Builder::new().suffix(".jsonl").tempfile().unwrap();
        let l1 = format!("{}\n", claude_session_line("/tmp/x", "sess-1"));
        f.write_all(l1.as_bytes()).unwrap();
        f.flush().unwrap();
        let (msgs, off) = read_messages(f.path(), 0);
        assert_eq!(msgs.len(), 1);
        assert_eq!(off, l1.len() as u64);

        // Append a complete line + a partial (no newline) line.
        let l2 = format!("{}\n", claude_assistant_line());
        let partial = r#"{"type":"user","message":{"rol"#;
        f.write_all(l2.as_bytes()).unwrap();
        f.write_all(partial.as_bytes()).unwrap();
        f.flush().unwrap();
        let (msgs2, off2) = read_messages(f.path(), off);
        assert_eq!(msgs2.len(), 1);
        assert_eq!(msgs2[0].text, "hi back");
        assert_eq!(off2, off + l2.len() as u64); // partial tail NOT consumed
    }

    #[test]
    fn first_user_message_uses_provider_parsed_title_or_first_user_text() {
        let mut f = tempfile::Builder::new().suffix(".jsonl").tempfile().unwrap();
        writeln!(f, "{}", claude_session_line("/tmp/x", "sess-1")).unwrap();
        writeln!(f, "{}", claude_assistant_line()).unwrap();
        f.flush().unwrap();
        let summary = first_user_message(f.path()).expect("a summary line");
        assert!(
            summary.contains("hello there"),
            "summary should derive from the first user turn, got: {summary}"
        );
    }

    #[test]
    fn bind_matches_pane_cwd_to_provider_parsed_session() {
        // A discoverable Claude project layout under a temp CLAUDE_CONFIG_DIR so we
        // never touch the developer's real ~/.claude. The provider owns this
        // layout; the test only asserts cwd→session resolution through `bind`.
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects").join("-tmp-proj");
        std::fs::create_dir_all(&projects).unwrap();
        let cwd = "/tmp/proj-bind-test";
        std::fs::write(
            projects.join("sess-bind.jsonl"),
            format!(
                "{}\n{}\n",
                claude_session_line(cwd, "sess-bind"),
                claude_assistant_line()
            ),
        )
        .unwrap();

        // Point the Claude provider's discovery root at our temp dir.
        // SAFETY: single-threaded test; restored before returning.
        let prev = std::env::var_os("CLAUDE_CONFIG_DIR");
        unsafe {
            std::env::set_var("CLAUDE_CONFIG_DIR", temp.path());
        }
        let bound = bind(cwd);
        match prev {
            Some(v) => unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") },
        }

        let bound = bound.expect("bind resolves the session for the matching cwd");
        assert_eq!(bound.kind, "claude", "kind comes from the provider descriptor");
        assert_eq!(bound.session_id, "sess-bind");
        assert!(bound.transcript.ends_with("sess-bind.jsonl"));
    }
}
