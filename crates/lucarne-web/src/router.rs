//! `/chat` ws routing + auth gate.
//!
//! Builds the `/chat` route in two flavours — bare [`router`] (local dev, no
//! auth/limits) and [`router_gated`] (the public deployment, gated by the
//! gateway's single-use ticket auth + shared connection pool) — and resolves the
//! per-upgrade permit / access scope via [`authorize_chat`] BEFORE upgrade so a
//! refused request never opens an agent session.

use std::sync::Arc;

use axum::extract::{Query, State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;

use lucarne_termgw::{authorize_ws, AccessScope, AuthState, GatewayLimits, WsConnectionPool};

use crate::session::client;
use crate::state::WebChat;

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
pub(crate) async fn authorize_chat(
    auth: &AuthState,
    ticket: Option<&str>,
    pool: &WsConnectionPool,
) -> Result<(AccessScope, tokio::sync::OwnedSemaphorePermit), Response> {
    let (scope, permit) = authorize_ws(auth, ticket, pool).await?;
    if scope.is_readonly() {
        tracing::info!(
            target: "lucarne_web",
            "refused /chat upgrade for read-only ticket (use /ws to mirror)"
        );
        drop(permit);
        return Err((
            StatusCode::FORBIDDEN,
            "read-only session: chat is not permitted (use the terminal mirror)",
        )
            .into_response());
    }
    Ok((scope, permit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::WebChat;
    use lucarne_termgw::{AccessToken, GatewayLimits};

    // R3-2: a read-only ticket must be refused at the `/chat` authorization with
    // 403 BEFORE any `on_upgrade` — so the chat `client` (which unconditionally
    // issues `RuntimeCommand::Open` to create a fresh agent session) never runs. A
    // read-only user mirrors via termgw's `/ws` instead. Driven against the
    // `authorize_chat` seam the handler calls before upgrading (deterministic; no
    // socket, no agent spawn, independent of the `WebSocketUpgrade` extractor).
    #[tokio::test]
    async fn readonly_ticket_is_refused_before_chat_upgrade() {
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
        let auth = AuthState::disabled();
        let pool = WsConnectionPool::new(GatewayLimits::default());
        let (scope, _permit) = authorize_chat(&auth, None, &pool)
            .await
            .expect("auth-disabled local /chat must be allowed");
        assert!(!scope.is_readonly(), "auth disabled → full local access");
    }

    // The bare `router` (local dev) builds without auth; `router_gated` threads
    // the caller's auth + shared pool. Both expose `/chat` and accept an
    // `Arc<WebChat>` — the API `remote.rs` / `webdev.rs` depend on.
    #[tokio::test]
    async fn routers_build_over_a_chat_bridge() {
        // No provider binaries are required to construct the bridge.
        let chat = WebChat::new(".".to_string(), Some(Vec::new()))
            .await
            .expect("build chat bridge");
        let _ = router(chat.clone());
        let auth = AuthState::disabled();
        let pool = WsConnectionPool::new(GatewayLimits::default());
        let _ = router_gated(chat, auth, pool);
    }
}
