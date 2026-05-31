//! lucarne-web — a thin web bridge to Lucarne's agent runtime.
//!
//! One `/chat` WebSocket per browser tab drives an [`AgentRuntime`] session:
//! open it, submit prompts, stream replies / reasoning / tool calls, and resolve
//! approvals inline. This is the "chat over web pipe" half of the dual-mode web
//! converter — a peer in spirit to the Telegram/WeChat channels, but the
//! transport is the web ws. Local (direct) and remote (cloudflared tunnel) reach
//! it identically; only auth (a thin gateway layer) differs.
//!
//! Layering (mirrors the Telegram/WeChat channel split):
//! * [`state`] — the [`WebChat`] shared state: the agent runtime, the bus
//!   fan-out, and the detached event pump (aborted on drop).
//! * [`router`] — the `/chat` ws routes ([`router`] / [`router_gated`]) and the
//!   pre-upgrade auth gate (`authorize_chat`).
//! * [`session`] — the per-socket client loop: open-ack wait (M6), inbound
//!   dispatch, outbound frame mapping, [`FrameRate`], and idle/lifetime close.
//!
//! [`AgentRuntime`]: lucarne::agent_runtime::AgentRuntime
//! [`FrameRate`]: session

mod router;
mod session;
mod state;

pub use router::{router, router_gated};
pub use state::WebChat;
