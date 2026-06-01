//! lucarne — agent multiplexer.
//!
//! A protocol-normalizing layer for AI agent CLIs (Claude / Codex /
//! Copilot / Gemini / Pi). Spawns subprocesses, translates
//! vendor-specific stdio into canonical [`event::Event`]s, mediates
//! permission requests, and exposes a unified [`runtime::Session`].
//!
//! Everything lives under one crate, dialects are `mod`s under
//! [`dialects`], and each adapter/dialect pair is feature-gated.

#[cfg(feature = "memory-profiling")]
#[macro_export]
macro_rules! memory_profile_snapshot {
    ($label:literal) => {{
        $crate::observability::emit_memory_profile_snapshot($label);
    }};
}

#[cfg(not(feature = "memory-profiling"))]
#[macro_export]
macro_rules! memory_profile_snapshot {
    ($label:literal) => {{}};
}

pub mod adapter;
pub mod adapters;
pub(crate) mod agent_registry;
pub mod agent_runtime;
pub mod control_plane;
pub mod core_service;
pub mod daemon;
pub mod dialect;
pub mod dialects;
pub mod error;
pub mod event;
pub mod framer;
pub mod history;
pub(crate) mod host;
pub mod journal;
pub mod launcher;
#[cfg(feature = "memory-profiling")]
pub mod observability;
mod provider_id;
pub mod runtime;
#[cfg(feature = "terminal-agent-bind")]
pub mod terminal_agent_bind;
pub(crate) mod time_display;

pub mod testing;

pub use adapter::ProtocolAdapter;
pub use agent_runtime::{
    AgentCapabilities, AgentCommand, AgentCommandCatalog, AgentCommandInput,
    AgentCommandInvocation, AgentCommandSource, AgentError, AgentErrorKind, AgentEventStream,
    AgentForkResult, AgentForkTarget, AgentForkTargetCatalog, AgentImageInput, AgentInput,
    AgentModelCatalog, AgentModelOption, AgentPermissionCatalog, AgentPermissionOption,
    AgentReasoningOption, AgentSessionOptions, AgentSkillCatalog, AgentSkillSummary, AgentStatus,
    AgentTokenUsage, ApprovalDecision, ApprovalRequest, Attachment, CallId, CommandId,
    CommandRejectedEvent, CommandResultEvent, InstanceId, InterventionRequest,
    InterventionResponse, MessageEvent, MessageRole, OpenSession, Question, QuestionAnswer,
    QuestionOption, QuestionRequest, QuestionResponse, ReasoningEvent, ResumeSession,
    RuntimeBusEvent, RuntimeBusFilter, RuntimeBusOutput, RuntimeBusStream, RuntimeCommand,
    SessionClosedEvent, SessionId, SessionOpenedEvent, SessionRef, ToolCallEvent, ToolResultEvent,
    UsageEvent,
};
pub use core_service::{
    default_lucarned_home_dir, default_state_db_path, CoreOptions, LucarneCore,
};
pub use daemon::LucarneDaemon;
pub use dialect::{Dialect, Input, OutFrame, SessionParams};
pub use error::{LucarneError, Result};
pub use event::Event;
pub use provider_id::ProviderId;
pub use runtime::Session;
