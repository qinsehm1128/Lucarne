//! lucarne-web — a thin web bridge to Lucarne's agent runtime.
//!
//! One `/chat` WebSocket per browser tab drives an [`AgentRuntime`] session:
//! open it, submit prompts, stream replies / reasoning / tool calls, and resolve
//! approvals inline. This is the "chat over web pipe" half of the dual-mode web
//! converter — a peer in spirit to the Telegram/WeChat channels, but the
//! transport is the web ws. Local (direct) and remote (cloudflared tunnel) reach
//! it identically; only auth (a thin gateway layer) differs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures::stream::{SplitSink, StreamExt};
use futures::SinkExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use lucarne::agent_runtime::{
    AgentInput, AgentRuntime, ApprovalDecision, CommandId, Event, InstanceId, InterventionRequest,
    InterventionResponse, MessageRole, OpenSession, RuntimeBusFilter, RuntimeBusOutput,
    RuntimeCommand,
};
use lucarne_termgw::{authorize_ws, AccessScope, AuthState, GatewayLimits, WsConnectionPool};

const OUTPUT_CAP: usize = 1024;

/// M6: hard cap on how long the chat ws waits for the runtime to ack `Open` with
/// a `SessionOpened` (or `CommandRejected`) before giving up. Without this a
/// missed ack (e.g. a broadcast `Lagged` that dropped it, or a provider that
/// never launches) would hang the task forever and leak the opened runtime
/// session. On timeout we send an error frame and tear down.
const OPEN_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    runtime: Arc<AgentRuntime>,
    outputs: broadcast::Sender<RuntimeBusOutput>,
    cwd: String,
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
        tokio::spawn(async move {
            while let Some(out) = events.recv().await {
                let _ = pump.send(out);
            }
        });
        Ok(Arc::new(Self { runtime, outputs, cwd }))
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

/// The `/chat` ws route with NO auth/limits (local dev / direct loopback only).
///
/// The `/chat` ws does not inherit the gateway router's auth across `merge`, so
/// any PUBLIC deployment must use [`router_gated`] instead — which routes the
/// chat ws through the SAME single-use-ticket auth, the SAME global connection
/// cap, idle/lifetime close, inbound frame rate, and the read-only access scope
/// as termgw's `/ws` + `/agent` (H1 / C1). This bare variant is for the unauthed
/// local dev runner.
pub fn router(state: Arc<WebChat>) -> Router {
    let gate = ChatGate {
        chat: state,
        auth: AuthState::disabled(),
        pool: WsConnectionPool::new(GatewayLimits::default()),
    };
    Router::new()
        .route("/chat", get(ws_handler))
        .with_state(gate)
}

/// The `/chat` ws route gated by the gateway's auth + limits (H1 / C1).
///
/// Routes the chat ws through [`authorize_ws`]: it acquires a permit from the
/// shared [`WsConnectionPool`] (so `/chat` counts against the SAME global
/// `max_ws_connections` cap as `/ws` + `/agent`), consumes the single-use ticket
/// BEFORE upgrade (M5: permit first), resolves the [`AccessScope`], and enforces
/// `pool.limits()` (idle / max-lifetime / inbound frame rate) plus read-only
/// refusal of inbound `prompt` / `approve` / `interrupt` in the client loop.
pub fn router_gated(state: Arc<WebChat>, auth: AuthState, pool: WsConnectionPool) -> Router {
    let gate = ChatGate {
        chat: state,
        auth,
        pool,
    };
    Router::new()
        .route("/chat", get(ws_handler))
        .with_state(gate)
}

/// `/chat` ws state: the chat bridge plus the gateway auth + shared connection
/// pool so the chat ws obeys the same auth + limits as the terminal ws routes.
#[derive(Clone)]
struct ChatGate {
    chat: Arc<WebChat>,
    auth: AuthState,
    pool: WsConnectionPool,
}

/// A `?ticket=` carried in the `/chat` upgrade query string (SEC-001), consumed
/// before upgrade exactly like termgw's `/ws`.
#[derive(Deserialize)]
struct TicketQuery {
    ticket: Option<String>,
}

async fn ws_handler(
    State(gate): State<ChatGate>,
    Query(q): Query<TicketQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // H1 / M5 / R3-2: resolve the connection permit + access scope and apply the
    // read-only chat refusal. This runs BEFORE `on_upgrade` so a refused request
    // never opens an agent session (see [`authorize_chat`]).
    let (scope, permit) = match authorize_chat(&gate.auth, q.ticket.as_deref(), &gate.pool).await {
        Ok(ok) => ok,
        Err(refusal) => return refusal,
    };
    let limits = gate.pool.limits();
    let chat = gate.chat.clone();
    ws.on_upgrade(move |socket| client(chat, socket, scope, limits, permit))
}

/// Authorize a `/chat` ws upgrade: acquire the shared permit, consume the ticket,
/// resolve the [`AccessScope`], and apply the R3-2 read-only refusal — all before
/// any `on_upgrade`, so a refused request never opens an agent session.
///
/// R3-2: a read-only credential must NOT reach `/chat`. The chat ws opens a fresh
/// agent runtime session (it unconditionally issues `RuntimeCommand::Open` in
/// [`client`]), which is a WRITE — incompatible with a mirror-only scope. A
/// read-only user observes via termgw's `/ws` (terminal mirror) and `/agent`
/// (transcript mirror) instead. So a read-only ticket is rejected with 403 here
/// and its connection permit released; only a `Full` scope proceeds to upgrade.
///
/// H1 / M5: the permit is acquired FIRST (a saturated cap → 503 without burning
/// the ticket); an invalid ticket → 401 and the permit is released.
#[allow(clippy::result_large_err)]
async fn authorize_chat(
    auth: &AuthState,
    ticket: Option<&str>,
    pool: &WsConnectionPool,
) -> Result<(AccessScope, tokio::sync::OwnedSemaphorePermit), Response> {
    let (scope, permit) = authorize_ws(auth, ticket, pool).await?;
    if scope.is_readonly() {
        tracing::info!("lucarne-web: refused /chat upgrade for read-only ticket (use /ws to mirror)");
        drop(permit);
        return Err((
            StatusCode::FORBIDDEN,
            "read-only session: chat is not permitted (use the terminal mirror)",
        )
            .into_response());
    }
    Ok((scope, permit))
}

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

async fn client(
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
        let _ = send_json(&mut tx, &json!({"type":"error","msg": format!("open failed: {}", e.message)})).await;
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
                    "lucarne-web: chat ws timed out waiting for SessionOpened ack; closing"
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
                    tracing::info!("lucarne-web: chat ws closed before session opened");
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
                    tracing::warn!(skipped = n, "lucarne-web: chat ws bus lagged while awaiting open ack");
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
                tracing::info!("lucarne-web: chat ws closed on max session lifetime");
                break;
            }
            _ = idle.tick() => {
                tracing::info!("lucarne-web: chat ws closed on idle timeout");
                break;
            }
            inbound = rx.next() => match inbound {
                Some(Ok(Message::Text(t))) => {
                    idle.reset();
                    // H1: throttle inbound frames per connection (anti flood).
                    if !frame_rate.allow() {
                        tracing::warn!("lucarne-web: chat ws inbound frame rate exceeded; closing");
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
        tracing::info!(kind = ?kind, "lucarne-web: refused write frame on read-only chat session");
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

    // R3-2: a read-only ticket must be refused at the `/chat` authorization with
    // 403 BEFORE any `on_upgrade` — so the chat `client` (which unconditionally
    // issues `RuntimeCommand::Open` to create a fresh agent session) never runs. A
    // read-only user mirrors via termgw's `/ws` instead. Driven against the
    // `authorize_chat` seam the handler calls before upgrading (deterministic; no
    // socket, no agent spawn, independent of the `WebSocketUpgrade` extractor).
    #[tokio::test]
    async fn readonly_ticket_is_refused_before_chat_upgrade() {
        use axum::http::StatusCode;
        use lucarne_termgw::{AccessScope, AccessToken, GatewayLimits};

        let full = AccessToken::generate();
        let readonly = AccessToken::generate();
        let auth = AuthState::with_tokens(full, readonly);
        let pool = WsConnectionPool::new(GatewayLimits::default());

        // A read-only ticket → 403, and the connection permit is released (so the
        // refusal does not leak a slot). No `Open` is reachable: the handler only
        // upgrades on `Ok`.
        let ro_ticket = auth.tickets.issue_scoped(AccessScope::ReadOnly).await;
        let refusal = authorize_chat(&auth, Some(&ro_ticket), &pool)
            .await
            .expect_err("read-only ticket must be refused");
        assert_eq!(
            refusal.status(),
            StatusCode::FORBIDDEN,
            "read-only ticket must be refused at /chat (no Open / no session created)"
        );

        // A full ticket → Ok(scope, permit): proceeds to upgrade (only here does
        // `client` run and issue `Open`).
        let full_ticket = auth.tickets.issue_scoped(AccessScope::Full).await;
        let (scope, _permit) = authorize_chat(&auth, Some(&full_ticket), &pool)
            .await
            .expect("a full ticket must pass the readonly gate");
        assert_eq!(scope, AccessScope::Full);
    }

    // R3-2 (auth disabled / local dev): `authorize_ws` yields `Full` when auth is
    // off, so local `/chat` is unaffected by the read-only refusal.
    #[tokio::test]
    async fn chat_authorization_allows_full_when_auth_disabled() {
        use lucarne_termgw::GatewayLimits;
        let auth = AuthState::disabled();
        let pool = WsConnectionPool::new(GatewayLimits::default());
        let (scope, _permit) = authorize_chat(&auth, None, &pool)
            .await
            .expect("auth-disabled local /chat must be allowed");
        assert!(!scope.is_readonly(), "auth disabled → full local access");
    }
}
