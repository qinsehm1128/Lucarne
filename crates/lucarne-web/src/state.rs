//! Shared chat state: the agent runtime plus a fan-out of all its bus outputs.
//!
//! [`WebChat`] owns the [`AgentRuntime`], the broadcast fan-out every `/chat`
//! socket subscribes to, and the detached event pump that forwards runtime bus
//! events into that fan-out. The pump is held as an [`AbortHandle`] so it is
//! aborted when the last `Arc<WebChat>` drops (Drop-based abort, mirroring
//! `lucarne-adapter`'s supervisor) — no leaked detached task.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::task::AbortHandle;

use lucarne::agent_runtime::{
    AgentRuntime, RuntimeBusFilter, RuntimeBusOutput, RuntimeCommand,
};

/// Bound on the fan-out broadcast channel. Slow `/chat` subscribers that fall
/// this far behind observe `Lagged` and resync rather than stalling the pump.
pub(crate) const OUTPUT_CAP: usize = 1024;

/// The runtime event filter the chat UI wants (assistant replies are OFF by
/// default in the runtime, so this is required to receive them).
const CHAT_FILTER: RuntimeBusFilter = RuntimeBusFilter {
    session_lifecycle: true,
    user_messages: true,
    assistant_messages: true,
    reasoning: true,
    tool_calls: true,
    tool_results: false,
    usage: false,
    intervention_requests: true,
    turn_lifecycle: true,
};

/// Shared chat state: the agent runtime plus a fan-out of all its bus outputs.
pub struct WebChat {
    pub(crate) runtime: Arc<AgentRuntime>,
    pub(crate) outputs: broadcast::Sender<RuntimeBusOutput>,
    pub(crate) cwd: String,
    /// Abort handle for the detached event-forward pump spawned in [`WebChat::new`].
    /// Held so the pump is torn down when the last `Arc<WebChat>` drops (see the
    /// `Drop` impl) instead of lingering as an orphaned task.
    pump: AbortHandle,
}

impl WebChat {
    /// Build a chat bridge: start an agent runtime, register the available local
    /// agents, open the event filter, and fan its outputs out to clients.
    /// `enabled` restricts to specific provider ids (None = all available).
    pub async fn new(cwd: String, enabled: Option<Vec<String>>) -> Result<Arc<Self>, String> {
        let runtime = Arc::new(AgentRuntime::new());
        match enabled {
            Some(ids) => runtime.register_defaults_filtered(&ids),
            None => runtime.register_defaults(),
        }
        let bus = runtime.bus();
        bus.command(RuntimeCommand::UpdateFilter { filter: CHAT_FILTER })
            .await
            .map_err(|e| e.message.to_string())?;
        let mut events = bus.take_events().await.map_err(|e| e.message.to_string())?;
        let (outputs, _) = broadcast::channel(OUTPUT_CAP);
        let pump = outputs.clone();
        // The event pump forwards every runtime bus output into the fan-out. Its
        // `AbortHandle` is stored on `WebChat` so the pump is aborted on drop —
        // it would otherwise outlive the `WebChat` it serves.
        let handle = tokio::spawn(async move {
            while let Some(out) = events.recv().await {
                let _ = pump.send(out);
            }
        });
        Ok(Arc::new(Self {
            runtime,
            outputs,
            cwd,
            pump: handle.abort_handle(),
        }))
    }

    /// Provider ids that registered (claude / codex / …; missing binaries skipped).
    pub fn providers(&self) -> Vec<String> {
        self.runtime
            .providers()
            .into_iter()
            .map(|d| d.id.as_str().to_string())
            .collect()
    }
}

impl Drop for WebChat {
    /// Tear down the detached event pump when the last `Arc<WebChat>` releases.
    /// Mirrors `lucarne-adapter`'s Drop-based abort so no forwarding task is
    /// leaked after the chat bridge is gone.
    fn drop(&mut self) {
        self.pump.abort();
    }
}

#[cfg(test)]
mod tests {
    // T3: the detached event pump is held as an AbortHandle (not dropped on the
    // floor) so it can be torn down on Drop — the production source must store
    // the handle on `WebChat` and abort it in `Drop`.
    #[test]
    fn event_pump_handle_is_retained_for_drop_abort() {
        let source = include_str!("state.rs")
            .split("\n#[cfg(test)]")
            .next()
            .expect("production source");
        assert!(
            source.contains("pump: AbortHandle"),
            "WebChat must retain the event-pump AbortHandle for Drop-based abort"
        );
        assert!(
            source.contains("self.pump.abort()"),
            "Drop for WebChat must abort the retained event pump"
        );
    }
}
