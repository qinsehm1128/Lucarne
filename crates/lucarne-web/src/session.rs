//! The `/chat` ws session: open the agent runtime session (with a bounded
//! open-ack wait), then pump inbound client frames → runtime commands and
//! runtime bus outputs → outbound ws frames, under the gateway's idle /
//! max-lifetime / inbound-frame-rate limits.
//!
//! Pure, unit-testable seams are factored out: [`output_to_frame`] (agent event
//! → ws frame classification), [`is_chat_write_kind`] (read-only write refusal),
//! and [`FrameRate`] (anti-flood leaky bucket).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures::stream::{SplitSink, StreamExt};
use futures::SinkExt;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use lucarne::agent_runtime::{
    AgentInput, ApprovalDecision, CommandId, Event, InstanceId, InterventionRequest,
    InterventionResponse, MessageRole, OpenSession, RuntimeBusOutput, RuntimeCommand,
};
use lucarne_termgw::{AccessScope, GatewayLimits};

use crate::state::WebChat;

/// M6: hard cap on how long the chat ws waits for the runtime to ack `Open` with
/// a `SessionOpened` (or `CommandRejected`) before giving up. Without this a
/// missed ack (e.g. a broadcast `Lagged` that dropped it, or a provider that
/// never launches) would hang the task forever and leak the opened runtime
/// session. On timeout we send an error frame and tear down.
const OPEN_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

type Tx = SplitSink<WebSocket, Message>;

async fn send_json(tx: &mut Tx, v: &Value) -> bool {
    tx.send(Message::Text(v.to_string().into())).await.is_ok()
}

fn unique() -> u64 {
    static C: AtomicU64 = AtomicU64::new(0);
    C.fetch_add(1, Ordering::Relaxed)
}

/// Simple per-connection inbound-frame rate limiter (SEC-004 / H1): a leaky
/// bucket of `max_per_sec` frames refilled each second. `allow()` returns false
/// when the connection exceeds its budget within the current 1-second window.
/// Mirrors termgw's `FrameRate` so `/chat` obeys the same anti-flood limit.
struct FrameRate {
    max_per_sec: u32,
    count: u32,
    window_start: tokio::time::Instant,
}

impl FrameRate {
    fn new(max_per_sec: u32) -> Self {
        Self {
            max_per_sec,
            count: 0,
            window_start: tokio::time::Instant::now(),
        }
    }

    fn allow(&mut self) -> bool {
        if self.max_per_sec == 0 {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now.duration_since(self.window_start) >= std::time::Duration::from_secs(1) {
            self.window_start = now;
            self.count = 0;
        }
        self.count = self.count.saturating_add(1);
        self.count <= self.max_per_sec
    }
}

pub(crate) async fn client(
    state: Arc<WebChat>,
    socket: WebSocket,
    scope: AccessScope,
    limits: GatewayLimits,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    // C1: a read-only chat session may mirror the agent transcript but must not
    // drive the agent (prompt / approve / interrupt). H1: the `_permit` is held
    // for the socket's lifetime (global connection cap) and the loop enforces an
    // idle timeout, a max session lifetime, and an inbound frame-rate limit —
    // the SAME limits as termgw's ws routes.
    let readonly = scope.is_readonly();
    let (mut tx, mut rx) = socket.split();
    let mut bus_events = state.outputs.subscribe();

    // Pick the first available provider.
    let Some(descriptor) = state.runtime.providers().into_iter().next() else {
        let _ = send_json(
            &mut tx,
            &json!({"type":"error","msg":"no agent provider available (is claude/codex on PATH?)"}),
        )
        .await;
        return;
    };
    let provider_id = descriptor.id;
    let provider_label = provider_id.as_str().to_string();

    // Open a session, tagged with a unique command id so we can match its ack.
    let command_id = CommandId(format!("web-{}", unique()).into());
    let open = RuntimeCommand::Open {
        command_id: command_id.clone(),
        provider_id,
        req: OpenSession {
            cwd: Some(state.cwd.clone().into()),
            ..Default::default()
        },
    };
    if let Err(e) = state.runtime.bus().command(open).await {
        let _ = send_json(
            &mut tx,
            &json!({"type":"error","msg": format!("open failed: {}", e.message)}),
        )
        .await;
        return;
    }

    // Await SessionOpened (or rejection) for our command id.
    //
    // M6: robust open-ack wait. A bare `recv().await` loop could hang forever
    // (and leak the opened runtime session) if the ack is never observed — e.g.
    // the broadcast `Lagged` dropped it, the provider never launches, or the
    // client vanished mid-open. So we `select!` over three exits:
    //   - the bus ack/rejection for our command id,
    //   - a hard timeout (OPEN_ACK_TIMEOUT),
    //   - the client socket closing while we wait.
    // On `Lagged` we keep draining until the deadline (a still-buffered ack may
    // arrive); only the deadline gives up. Every non-success path returns WITHOUT
    // a dangling task — but it can leak a session whose `instance_id` we never
    // learned, so on timeout we log loudly (the runtime's own idle/session
    // lifetime eventually reaps it).
    let open_deadline = tokio::time::Instant::now() + OPEN_ACK_TIMEOUT;
    let instance_id = loop {
        tokio::select! {
            _ = tokio::time::sleep_until(open_deadline) => {
                tracing::warn!(
                    target: "lucarne_web",
                    timeout_secs = OPEN_ACK_TIMEOUT.as_secs(),
                    "chat ws timed out waiting for SessionOpened ack; closing"
                );
                let _ = send_json(
                    &mut tx,
                    &json!({"type":"error","msg":"timed out opening agent session"}),
                )
                .await;
                return;
            }
            // The client disconnected (or sent Close) before the session opened:
            // stop waiting so the task does not linger. We never learned the
            // instance_id, so there is nothing to Close here.
            inbound = rx.next() => match inbound {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    tracing::info!(target: "lucarne_web", "chat ws closed before session opened");
                    return;
                }
                // Ignore any pre-open inbound frame (we are not ready yet).
                Some(Ok(_)) => continue,
            },
            out = bus_events.recv() => match out {
                Ok(RuntimeBusOutput::SessionOpened(ev)) if ev.command_id == command_id => {
                    break ev.instance_id;
                }
                Ok(RuntimeBusOutput::CommandRejected(ev))
                    if ev.command_id.as_ref() == Some(&command_id) =>
                {
                    let _ = send_json(&mut tx, &json!({"type":"error","msg": ev.message.to_string()})).await;
                    return;
                }
                Ok(_) => continue,
                // M6: a dropped-message window may have swallowed our ack. Keep
                // draining until the deadline rather than spinning forever or
                // silently hanging; the timeout arm bounds the wait.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(target: "lucarne_web", skipped = n, "chat ws bus lagged while awaiting open ack");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    if !send_json(&mut tx, &json!({"type":"ready","provider": provider_label, "readonly": readonly})).await {
        let _ = state.runtime.bus().command(RuntimeCommand::Close { instance_id }).await;
        return;
    }

    // SEC-006 / H1: idle + max-lifetime close, so a live chat socket cannot
    // outlive its connect-time ticket indefinitely.
    let deadline = tokio::time::Instant::now() + limits.max_session_lifetime;
    let mut idle = tokio::time::interval(limits.idle_timeout);
    idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    idle.tick().await; // consume the immediate first tick
    let mut frame_rate = FrameRate::new(limits.max_inbound_frames_per_sec);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                tracing::info!(target: "lucarne_web", "chat ws closed on max session lifetime");
                break;
            }
            _ = idle.tick() => {
                tracing::info!(target: "lucarne_web", "chat ws closed on idle timeout");
                break;
            }
            inbound = rx.next() => match inbound {
                Some(Ok(Message::Text(t))) => {
                    idle.reset();
                    // H1: throttle inbound frames per connection (anti flood).
                    if !frame_rate.allow() {
                        tracing::warn!(target: "lucarne_web", "chat ws inbound frame rate exceeded; closing");
                        break;
                    }
                    if !handle_inbound(&state, &instance_id, &mut tx, t.as_str(), readonly).await {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => { idle.reset(); }
                Some(Err(_)) => break,
            },
            out = bus_events.recv() => match out {
                Ok(output) => {
                    if let Some(frame) = output_to_frame(&output, &instance_id) {
                        if !send_json(&mut tx, &frame).await {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    let _ = state.runtime.bus().command(RuntimeCommand::Close { instance_id }).await;
}

async fn handle_inbound(
    state: &Arc<WebChat>,
    instance_id: &InstanceId,
    tx: &mut Tx,
    text: &str,
    readonly: bool,
) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return true;
    };
    let kind = v.get("type").and_then(Value::as_str);
    // C1: prompt / approve / interrupt all drive the live agent — writes. A
    // read-only chat session may only mirror, so refuse them WITHOUT issuing any
    // RuntimeCommand (no Submit / Resolve / Interrupt reaches the runtime).
    if readonly && is_chat_write_kind(kind) {
        tracing::info!(target: "lucarne_web", kind = ?kind, "refused write frame on read-only chat session");
        return send_json(
            tx,
            &json!({"type":"error","msg":"read-only session: this action is not permitted"}),
        )
        .await;
    }
    match kind {
        Some("prompt") => {
            let prompt = v.get("text").and_then(Value::as_str).unwrap_or("");
            let cmd = RuntimeCommand::Submit {
                instance_id: instance_id.clone(),
                input: AgentInput { text: prompt.into(), images: Vec::new() },
            };
            if let Err(e) = state.runtime.bus().command(cmd).await {
                return send_json(tx, &json!({"type":"error","msg": e.message.to_string()})).await;
            }
            true
        }
        Some("approve") => {
            let req_id = v.get("req_id").and_then(Value::as_str).unwrap_or("");
            let allow = v.get("allow").and_then(Value::as_bool).unwrap_or(false);
            let decision = if allow { ApprovalDecision::Allow } else { ApprovalDecision::Deny };
            let cmd = RuntimeCommand::Resolve {
                instance_id: instance_id.clone(),
                req_id: req_id.into(),
                response: InterventionResponse::Approval(decision),
            };
            let _ = state.runtime.bus().command(cmd).await;
            true
        }
        Some("interrupt") => {
            let _ = state
                .runtime
                .bus()
                .command(RuntimeCommand::Interrupt { instance_id: instance_id.clone() })
                .await;
            true
        }
        _ => true,
    }
}

/// Map a runtime bus output addressed to `instance_id` into a chat frame.
fn output_to_frame(output: &RuntimeBusOutput, instance_id: &InstanceId) -> Option<Value> {
    match output {
        RuntimeBusOutput::Event(ev) if &ev.instance_id == instance_id => match &ev.event {
            Event::Message(m) => Some(json!({
                "type": "message",
                "role": match m.role { MessageRole::User => "user", MessageRole::Assistant => "assistant" },
                "text": m.text.as_str(),
                "streaming": m.streaming,
            })),
            Event::Reasoning(r) => Some(json!({"type":"reasoning","text": r.text.as_str()})),
            Event::ToolCall(t) => Some(json!({"type":"tool","name": t.name.as_str()})),
            Event::TurnCompleted(_) => Some(json!({"type":"turn_complete"})),
            Event::TurnFailed(t) => Some(json!({"type":"turn_failed","error": t.error.as_str()})),
            Event::InterventionRequest(InterventionRequest::Approval(a)) => Some(json!({
                "type": "approval",
                "req_id": a.req_id.as_str(),
                "tool": a.tool_name.as_str(),
                "message": a.message.as_ref().map(|m| m.as_str()),
            })),
            _ => None,
        },
        RuntimeBusOutput::SessionClosed(ev) if &ev.instance_id == instance_id => {
            Some(json!({"type":"error","msg": format!("agent session closed: {}", ev.reason)}))
        }
        RuntimeBusOutput::CommandRejected(ev) if ev.instance_id.as_ref() == Some(instance_id) => {
            Some(json!({"type":"error","msg": ev.message.to_string()}))
        }
        _ => None,
    }
}

/// True for inbound chat frame kinds that DRIVE the live agent — a write (C1):
/// `prompt` (Submit), `approve` (Resolve), `interrupt` (Interrupt). A read-only
/// chat session refuses these and never issues the corresponding RuntimeCommand;
/// all other kinds (and mirror-only output) are allowed.
fn is_chat_write_kind(kind: Option<&str>) -> bool {
    matches!(kind, Some("prompt") | Some("approve") | Some("interrupt"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucarne::agent_runtime::events::TurnFailedEvent;
    use lucarne::agent_runtime::{
        ApprovalRequest, CallId, MessageEvent, ProviderId, ReasoningEvent, RuntimeBusEvent,
        SessionClosedEvent, SessionId, ToolCallEvent,
    };

    fn instance(id: &str) -> InstanceId {
        InstanceId(id.into())
    }

    fn event_for(id: &str, event: Event) -> RuntimeBusOutput {
        RuntimeBusOutput::Event(RuntimeBusEvent {
            instance_id: instance(id),
            provider_id: ProviderId::from_static("claude"),
            session_id: SessionId("session-1".into()),
            event,
        })
    }

    // C1: the read-only chat refusal classifies prompt / approve / interrupt as
    // writes (so they are refused and no RuntimeCommand is issued), while other
    // kinds are not writes (allowed to pass / mirror).
    #[test]
    fn chat_write_kinds_are_classified_for_readonly_refusal() {
        assert!(is_chat_write_kind(Some("prompt")));
        assert!(is_chat_write_kind(Some("approve")));
        assert!(is_chat_write_kind(Some("interrupt")));
        // Non-write / unknown kinds are not refused.
        assert!(!is_chat_write_kind(Some("ping")));
        assert!(!is_chat_write_kind(Some("subscribe")));
        assert!(!is_chat_write_kind(None));
    }

    // T4: outbound frame mapping — each agent event addressed to our instance is
    // classified into the right ws frame type, with the payload fields the UI
    // expects. This is the failure mode the module comments call out (event →
    // ServerFrame/ws message classification).
    #[test]
    fn message_event_maps_to_message_frame_with_role_and_streaming() {
        let out = event_for(
            "inst-1",
            Event::Message(MessageEvent {
                role: MessageRole::Assistant,
                text: "hi".into(),
                streaming: true,
            }),
        );
        let frame = output_to_frame(&out, &instance("inst-1")).expect("message frame");
        assert_eq!(frame["type"], "message");
        assert_eq!(frame["role"], "assistant");
        assert_eq!(frame["text"], "hi");
        assert_eq!(frame["streaming"], true);

        let user = event_for(
            "inst-1",
            Event::Message(MessageEvent {
                role: MessageRole::User,
                text: "yo".into(),
                streaming: false,
            }),
        );
        let frame = output_to_frame(&user, &instance("inst-1")).expect("message frame");
        assert_eq!(frame["role"], "user");
        assert_eq!(frame["streaming"], false);
    }

    #[test]
    fn reasoning_and_tool_and_turn_events_map_to_their_frames() {
        let reasoning = event_for("inst-1", Event::Reasoning(ReasoningEvent { text: "think".into() }));
        let frame = output_to_frame(&reasoning, &instance("inst-1")).expect("reasoning frame");
        assert_eq!(frame["type"], "reasoning");
        assert_eq!(frame["text"], "think");

        let tool = event_for(
            "inst-1",
            Event::ToolCall(ToolCallEvent {
                call_id: CallId("c1".into()),
                name: "bash".into(),
                input: json!({"cmd": "ls"}),
            }),
        );
        let frame = output_to_frame(&tool, &instance("inst-1")).expect("tool frame");
        assert_eq!(frame["type"], "tool");
        assert_eq!(frame["name"], "bash");

        let failed = event_for(
            "inst-1",
            Event::TurnFailed(TurnFailedEvent {
                turn_id: "t1".into(),
                error: "boom".into(),
                code: "api".into(),
            }),
        );
        let frame = output_to_frame(&failed, &instance("inst-1")).expect("turn_failed frame");
        assert_eq!(frame["type"], "turn_failed");
        assert_eq!(frame["error"], "boom");
    }

    #[test]
    fn approval_request_maps_to_approval_frame() {
        let approval = event_for(
            "inst-1",
            Event::InterventionRequest(InterventionRequest::Approval(ApprovalRequest {
                req_id: "r1".into(),
                tool_name: "write".into(),
                message: Some("ok?".into()),
                input: None,
            })),
        );
        let frame = output_to_frame(&approval, &instance("inst-1")).expect("approval frame");
        assert_eq!(frame["type"], "approval");
        assert_eq!(frame["req_id"], "r1");
        assert_eq!(frame["tool"], "write");
        assert_eq!(frame["message"], "ok?");
    }

    #[test]
    fn session_closed_and_command_rejected_map_to_error_frames() {
        let closed = RuntimeBusOutput::SessionClosed(SessionClosedEvent {
            instance_id: instance("inst-1"),
            provider_id: ProviderId::from_static("claude"),
            session_id: SessionId("session-1".into()),
            reason: "provider exited".into(),
        });
        let frame = output_to_frame(&closed, &instance("inst-1")).expect("closed → error frame");
        assert_eq!(frame["type"], "error");
        assert!(frame["msg"].as_str().unwrap().contains("provider exited"));

        let rejected = RuntimeBusOutput::CommandRejected(
            lucarne::agent_runtime::CommandRejectedEvent {
                command_id: None,
                session_id: None,
                instance_id: Some(instance("inst-1")),
                message: "nope".into(),
            },
        );
        let frame = output_to_frame(&rejected, &instance("inst-1")).expect("rejected → error frame");
        assert_eq!(frame["type"], "error");
        assert_eq!(frame["msg"], "nope");
    }

    // Events addressed to a DIFFERENT instance must never leak into this socket's
    // frame stream (the `instance_id` guard).
    #[test]
    fn events_for_a_different_instance_are_dropped() {
        let other = event_for(
            "inst-2",
            Event::Reasoning(ReasoningEvent { text: "not mine".into() }),
        );
        assert!(output_to_frame(&other, &instance("inst-1")).is_none());
    }

    // Filtered-out events (e.g. ToolResult / Usage are not surfaced to chat) yield
    // no frame even when addressed to our instance.
    #[test]
    fn unmapped_events_yield_no_frame() {
        use lucarne::agent_runtime::ToolResultEvent;
        let tool_result = event_for(
            "inst-1",
            Event::ToolResult(ToolResultEvent {
                call_id: CallId("c1".into()),
                output: json!("done"),
                is_error: None,
            }),
        );
        assert!(output_to_frame(&tool_result, &instance("inst-1")).is_none());
    }

    // T4: FrameRate limit boundary — within a 1-second window exactly
    // `max_per_sec` frames are allowed; the next one is refused. A zero limit
    // disables the bucket (always allow). Paused time keeps the window fixed so
    // the boundary is deterministic.
    #[tokio::test(start_paused = true)]
    async fn frame_rate_allows_up_to_limit_then_refuses_within_window() {
        let mut rate = FrameRate::new(3);
        assert!(rate.allow());
        assert!(rate.allow());
        assert!(rate.allow());
        // 4th frame in the same 1s window is over budget.
        assert!(!rate.allow());
        assert!(!rate.allow());
    }

    #[tokio::test(start_paused = true)]
    async fn frame_rate_refills_after_one_second_window() {
        let mut rate = FrameRate::new(2);
        assert!(rate.allow());
        assert!(rate.allow());
        assert!(!rate.allow());
        // Advance past the 1-second window: the bucket refills.
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(rate.allow());
        assert!(rate.allow());
        assert!(!rate.allow());
    }

    #[test]
    fn frame_rate_zero_limit_always_allows() {
        let mut rate = FrameRate::new(0);
        for _ in 0..1000 {
            assert!(rate.allow());
        }
    }

    // T4 (M6): the open-ack wait gives up at OPEN_ACK_TIMEOUT. We exercise the
    // exact `select!` timeout shape the `client` loop uses — `sleep_until(open
    // deadline)` fires before an ack arrives on a bus that never delivers one —
    // under paused time, so the deadline is hit deterministically without a real
    // 30s wait. This is the M6 "missed ack must not hang forever" guarantee.
    #[tokio::test(start_paused = true)]
    async fn open_ack_wait_times_out_when_no_ack_arrives() {
        let (tx, _rx) = broadcast::channel::<RuntimeBusOutput>(8);
        let mut bus = tx.subscribe();
        let open_deadline = tokio::time::Instant::now() + OPEN_ACK_TIMEOUT;

        let timed_out = loop {
            tokio::select! {
                _ = tokio::time::sleep_until(open_deadline) => break true,
                out = bus.recv() => match out {
                    // No ack is ever published, so any delivery here is unexpected.
                    Ok(_) => break false,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break false,
                }
            }
        };
        assert!(timed_out, "open-ack wait must give up at OPEN_ACK_TIMEOUT");
    }
}
