//! lucarne-rmux — the live rmux-sdk binding for the terminal-monitor subsystem.
//!
//! Connects to the SYSTEM rmux daemon (the daemon the user's own `rmux` uses),
//! mirrors its panes into the rmux-free [`lucarne_term`] vocabulary, and injects
//! input. This crate (with [`adapter`]) is the ONLY place that names `rmux_sdk`,
//! so preview-API churn never leaks into the gateway, web channel, or CLI.

pub mod adapter;
pub mod monitor;

pub use monitor::{GridUpdate, MonitorError, RmuxMonitor};
