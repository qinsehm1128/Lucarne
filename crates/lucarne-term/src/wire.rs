//! wire — the tagged Server/Client frames exchanged with web mirror clients.
//!
//! Terminal-only subset of the upstream rmux_remote_control protocol: the chat,
//! archive, and pop-out/return frames are intentionally dropped. Chat reaches the
//! web app over a separate Lucarne `Channel` (not this terminal mirror); pop-out
//! / retract is rmux-native (`attach-session` / `detach-client`) via the CLI, not
//! a wire frame.

use serde::{Deserialize, Serialize};

use crate::grid::{Cursor, GridDelta, PaneGrid};
use crate::input::TermInput;
use crate::registry::{SessionDescriptor, SessionId};

/// Server → Client frames. Tagged by `type` in `snake_case`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// The monitored session list (sent on connect & after create/close).
    SessionList { sessions: Vec<SessionDescriptor> },
    /// Full grid — on subscribe, on resize, or as a delta-gap resync fallback.
    Snapshot {
        session: SessionId,
        grid: PaneGrid,
        cursor: Cursor,
    },
    /// Incremental update — the hot path (dirty-row runs only).
    SnapshotDelta {
        session: SessionId,
        base_rev: u64,
        rev: u64,
        delta: GridDelta,
        cursor: Cursor,
    },
    /// A session was created in response to `ClientFrame::CreateSession`.
    SessionCreated { session: SessionId },
    /// A session was closed (via `CloseSession` or it exited).
    SessionClosed { session: SessionId },
    Error { code: u16, msg: String },
    Pong { t: u64 },
}

/// Client → Server frames. Tagged by `type` in `snake_case`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Subscribe to a session's mirror; the server replies with a full Snapshot
    /// and then streams SnapshotDelta.
    Subscribe { session: SessionId },
    /// Stop receiving a session's mirror.
    Detach { session: SessionId },
    /// Keys / text / control for the mirror (injected via `send_text`/`send_key`).
    Input {
        session: SessionId,
        event: TermInput,
    },
    /// Delta-gap recovery: ask for a fresh full Snapshot.
    Resync { session: SessionId, have_rev: u64 },
    /// Create a new shell session on the system daemon.
    CreateSession { title: Option<String> },
    /// Kill a session on the system daemon.
    CloseSession { session: SessionId },
    Ping { t: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Cursor, PaneGrid};

    #[test]
    fn server_snapshot_round_trips_tagged() {
        let f = ServerFrame::Snapshot {
            session: "s:0:0".into(),
            grid: PaneGrid {
                cols: 1,
                rows: 1,
                cells: vec![],
                rev: 3,
            },
            cursor: Cursor {
                row: 0,
                col: 0,
                visible: true,
                style_raw: 0,
            },
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"type\":\"snapshot\""));
        let back: ServerFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn client_input_round_trips_tagged() {
        let f = ClientFrame::Input {
            session: "s:0:0".into(),
            event: TermInput::Text { text: "ls\n".into() },
        };
        let back: ClientFrame =
            serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back, f);
    }
}
