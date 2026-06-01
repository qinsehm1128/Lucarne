//! lucarne-rmux — rmux terminal capability package.
//!
//! Connects to the SYSTEM rmux daemon (the daemon the user's own `rmux` uses),
//! mirrors its panes into a stable terminal vocabulary, archives terminal
//! sessions, and injects input. The package owns the terminal domain types plus
//! the only `rmux_sdk` binding used by Lucarne.

pub mod adapter;
pub mod archive;
pub mod monitor;
pub mod term;

pub use monitor::{GridUpdate, MonitorError, RmuxMonitor};
pub use term::{
    control_key_token, ClientFrame, Color, ControlKey, Cursor, DiffResult, Differ, Dims, GridDelta,
    KeyMods, Origin, PaneGrid, ServerFrame, SessionDescriptor, SessionId, SessionRegistry, Style,
    TermInput,
};
