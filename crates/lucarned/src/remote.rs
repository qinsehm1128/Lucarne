//! Remote-access subsystem wiring for `lucarned` (compiled only under the
//! `remote` cargo feature).
//!
//! Owns the public-tunnel lifecycle inside the daemon (Locked decision L6): the
//! daemon binds the terminal gateway to **loopback** (Locked decision L3),
//! enforces **default-deny** auth (Locked decision L4 — a token is required;
//! absent → generated at startup; `insecure` is the explicit, loud opt-out), and
//! starts the selected tunnel provider from [`lucarne_remote::builtin`]. The
//! tunnel survives the CLI exiting and is torn down when the daemon shuts down.
//!
//! The CLI drives this over the loopback-only `/api/remote/{start,stop,status}`
//! routes which the gateway forwards to [`DaemonRemoteControl`] (the daemon's
//! [`lucarne_termgw::RemoteControl`] implementation). Mirrors the health
//! subsystem's spawn-after-gateway + shutdown wiring (`main.rs`).

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use lucarne_remote::{ProviderConfig, RemoteRegistry, TunnelHandle, TunnelStatus};
use lucarne_termgw::{
    AccessToken, AuthState, ForwardedIdentityPolicy, GatewayLimits, RemoteControl,
    RemoteControlError, RemoteControlStatus, RemoteStartParams, WsConnectionPool,
};
use lucarne_web::WebChat;
use lucarne_rmux::RmuxMonitor;
use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

/// Web asset directory served by the gateway (static dual-mode web app). Env
/// `LUCARNED_REMOTE_WEB` overrides; defaults to `web` (relative to the daemon's
/// working dir), matching the dev runners (`termgw-dev`, `webdev`).
const DEFAULT_WEB_DIR: &str = "web";

/// How often the H3 health watcher polls the running tunnel and reaps it if the
/// provider reports the child has exited (so `/api/remote/status` reflects
/// reality and a crashed tunnel can be restarted without a manual status call).
const REAPER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Resolved remote-access runtime configuration (produced by
/// `remote_config_from_config` in `main.rs`, after env overrides + the
/// loopback-hardened gateway address parse).
///
/// H6c: the provider's own configuration is a GENERIC, transport-agnostic
/// `provider_fields` map (keyed by `RequiredField::key`) — the daemon no longer
/// owns cloudflare-specific fields. Adding FRP / relay / any future backend is a
/// new provider impl + a `providers.<id>` block in `lucarned.yaml`, with zero
/// daemon-config changes (AGENTS.md provider boundary; ADR L1/L2/L7).
#[derive(Clone, Debug)]
pub struct RemoteRuntimeConfig {
    /// Tunnel backend provider id (e.g. `"cloudflared"`).
    pub provider: String,
    /// Loopback gateway bind address (already validated loopback — L3). This is
    /// the ONLY port the tunnel targets.
    pub gateway_addr: SocketAddr,
    /// Loopback control-plane bind address (SEC-002). A DISTINCT port from
    /// `gateway_addr` that the tunnel never targets; serves `/api/remote/*` and
    /// returns the `access_token`. Already validated loopback.
    pub control_addr: SocketAddr,
    /// Configured gateway access token; `None` → generate at startup (L4).
    pub auth_token: Option<String>,
    /// Optional read-only access token (SEC-013); `None` → no read-only tier.
    pub readonly_token: Option<String>,
    /// Explicit opt-out of auth (loud warning; never the default — L4).
    pub insecure: bool,
    /// H6c: opaque per-provider field map (keyed by the provider's
    /// `RequiredField::key`, e.g. cloudflared's `token` / `public_url` /
    /// `binary_path`). The daemon passes this straight through to
    /// [`ProviderConfig`] without interpreting any field — provider-specific
    /// structure stays at the provider boundary.
    pub provider_fields: std::collections::BTreeMap<String, String>,
}

impl RemoteRuntimeConfig {
    /// Build the opaque per-provider [`ProviderConfig`] for the tunnel backend
    /// (Locked decision L2: providers take a flat key→value map, no daemon
    /// types leak in). H6c: this is now a pure copy of the generic
    /// `provider_fields` map — the daemon maps NO provider-specific field names.
    ///
    /// G3: `overrides` are CLI-supplied field values (from `term go-public`)
    /// merged **over** the daemon's configured fields — a present override wins,
    /// an absent one keeps the configured value. An empty `overrides` map yields
    /// exactly the daemon's pre-configured fields (backward compatible).
    fn provider_config(
        &self,
        overrides: &std::collections::BTreeMap<String, String>,
    ) -> ProviderConfig {
        let mut cfg = ProviderConfig::new();
        // Daemon-configured provider fields (verbatim — no per-field mapping).
        for (key, value) in &self.provider_fields {
            if value.is_empty() {
                continue;
            }
            cfg.fields.insert(key.clone(), value.clone());
        }
        // G3: CLI-supplied fields override / extend the configured ones.
        for (key, value) in overrides {
            if value.is_empty() {
                continue;
            }
            cfg.fields.insert(key.clone(), value.clone());
        }
        cfg
    }
}

/// Handle returned to `run_daemon` after the subsystem is up — carries the
/// fields the daemon logs (`provider`, `public_url`). The live tunnel + control
/// plane live in the spawned tasks / the `Arc<DaemonRemoteControl>`.
pub struct RemoteSubsystem {
    pub provider: String,
    pub public_url: String,
}

/// The daemon's [`RemoteControl`] implementation: it owns the tunnel lifecycle
/// (Locked decision L6) and is driven by the gateway's loopback-only
/// `/api/remote/{start,stop,status}` routes.
///
/// H4: a small [`TunnelState`] state machine behind a `Mutex` lets `start` /
/// `stop` / `status` run the provider's `start` / `stop` / `health` **outside**
/// the lock. The lock is only held to read/transition the state and take out the
/// data needed for the await; the long-running provider await then runs lock-free
/// (so status/stop/start never block each other and shutdown can't deadlock),
/// and the result is written back under a fresh lock.
struct DaemonRemoteControl {
    registry: RemoteRegistry,
    config: RemoteRuntimeConfig,
    /// The gateway access token handed to remote clients (`#token=…`). Present
    /// unless running `insecure` with no token.
    access_token: Option<String>,
    /// Tunnel lifecycle state machine (H4). Guards transitions; provider awaits
    /// happen with this lock released.
    state: Mutex<TunnelState>,
}

/// Tunnel lifecycle (H4). `Starting` / `Stopping` are transient markers so a
/// concurrent caller observes work-in-progress instead of racing a second
/// provider start/stop while the first is awaiting lock-free.
enum TunnelState {
    /// No tunnel running and none being started.
    Idle,
    /// A `start` is in flight (provider await running lock-free).
    Starting,
    /// A tunnel is up; the handle is needed to `stop` / `health` it.
    Running(TunnelHandle),
    /// A `stop` is in flight (provider await running lock-free).
    Stopping,
}

impl DaemonRemoteControl {
    fn status_from(&self, handle: Option<&TunnelHandle>) -> RemoteControlStatus {
        match handle {
            Some(h) => RemoteControlStatus {
                running: true,
                provider: Some(h.provider_id.clone()),
                public_url: Some(h.public_url.to_string()),
                access_token: self.access_token.clone(),
            },
            None => RemoteControlStatus {
                running: false,
                provider: None,
                public_url: None,
                access_token: self.access_token.clone(),
            },
        }
    }
}

#[async_trait]
impl RemoteControl for DaemonRemoteControl {
    /// H4: the `self.state` lock is NEVER held across an `await`. The
    /// already-running idempotent path clones the [`TunnelHandle`] out under the
    /// lock (phase 1a), probes `provider.health(&handle)` with the lock RELEASED
    /// (phase 1b), then re-acquires the lock and re-checks that the SAME handle is
    /// still `Running` (compares `opaque`) before deciding the transition (phase
    /// 1c) — so a concurrent stop/start that ran during the lock-free health await
    /// is never clobbered. The provider `start` spawn (phase 2) and the
    /// write-back of its outcome (phase 3) are likewise split across the lock.
    async fn start(
        &self,
        params: RemoteStartParams,
    ) -> Result<RemoteControlStatus, RemoteControlError> {
        // Phase 1a (locked): inspect state, handle the busy cases, and — for a
        // running tunnel — clone the handle out so the health probe runs OUTSIDE
        // the lock (H4: no state lock is ever held across an `await`). For Idle we
        // claim `Starting` directly here so a concurrent start can't double-spawn.
        let running_handle = {
            let mut guard = self.state.lock().await;
            match &*guard {
                // Already running → clone the handle and verify health lock-free
                // (Phase 1b). We do NOT decide the transition under this lock.
                TunnelState::Running(handle) => Some(handle.clone()),
                TunnelState::Idle => {
                    *guard = TunnelState::Starting;
                    None
                }
                TunnelState::Starting => {
                    return Err(RemoteControlError::Backend(
                        "a tunnel start is already in progress".to_string(),
                    ));
                }
                TunnelState::Stopping => {
                    return Err(RemoteControlError::Backend(
                        "a tunnel stop is in progress; retry shortly".to_string(),
                    ));
                }
            }
        };

        // Phase 1b (lock-free): if a tunnel was running, probe its health with the
        // state lock released. H3: a dead tunnel is restarted; a healthy one is the
        // idempotent success path.
        if let Some(handle) = running_handle {
            let healthy = match self.registry.lookup(&handle.provider_id) {
                Some(p) => !matches!(
                    p.health(&handle).await.unwrap_or(TunnelStatus::Unknown),
                    TunnelStatus::Down
                ),
                None => false,
            };

            // Phase 1c (locked): re-acquire and re-check the state. A concurrent
            // start/stop may have changed it while health awaited lock-free, so we
            // only act when the SAME handle is still Running (compare opaque).
            {
                let mut guard = self.state.lock().await;
                match &*guard {
                    TunnelState::Running(current) if current.opaque == handle.opaque => {
                        if healthy {
                            // Idempotent: still up and healthy.
                            return Ok(self.status_from(Some(current)));
                        }
                        // Dead/unknown: drop the stale handle and claim Starting so
                        // we restart below (Phase 2/3).
                        info!("lucarned remote: tunnel reported Down on start; restarting");
                        *guard = TunnelState::Starting;
                    }
                    // The state moved on under us (a concurrent stop reaped it, or a
                    // start replaced the handle). Report the live status without
                    // racing a second spawn.
                    TunnelState::Running(current) => {
                        return Ok(self.status_from(Some(current)));
                    }
                    TunnelState::Idle => {
                        // A concurrent stop cleared it; claim Starting and restart.
                        *guard = TunnelState::Starting;
                    }
                    TunnelState::Starting => {
                        return Err(RemoteControlError::Backend(
                            "a tunnel start is already in progress".to_string(),
                        ));
                    }
                    TunnelState::Stopping => {
                        return Err(RemoteControlError::Backend(
                            "a tunnel stop is in progress; retry shortly".to_string(),
                        ));
                    }
                }
            }
        }

        // Phase 2 (lock-free): resolve provider + config, then await the spawn.
        let result = self.do_start(params).await;

        // Phase 3 (locked): write the outcome back.
        let mut guard = self.state.lock().await;
        match result {
            Ok(handle) => {
                let status = self.status_from(Some(&handle));
                *guard = TunnelState::Running(handle);
                Ok(status)
            }
            Err(e) => {
                // The start failed; return to Idle so a retry is possible.
                *guard = TunnelState::Idle;
                Err(e)
            }
        }
    }

    async fn stop(&self) -> Result<RemoteControlStatus, RemoteControlError> {
        // Phase 1 (locked): take the running handle (if any) and claim Stopping.
        let handle = {
            let mut guard = self.state.lock().await;
            match &*guard {
                TunnelState::Running(_) => {
                    // Replace with Stopping and extract the handle.
                    match std::mem::replace(&mut *guard, TunnelState::Stopping) {
                        TunnelState::Running(handle) => handle,
                        _ => unreachable!("matched Running above"),
                    }
                }
                // Nothing running (Idle / a transient start/stop): succeed
                // idempotently without touching the in-flight transition.
                _ => return Ok(self.status_from(None)),
            }
        };

        // SEC-011: audit the tunnel stop (provider + public host; no token).
        info!(
            provider = %handle.provider_id,
            public_host = handle.public_url.host_str().unwrap_or(""),
            "lucarned remote: tunnel stopping"
        );

        // Phase 2 (lock-free): run the provider stop await.
        let provider = match self.registry.lookup(&handle.provider_id) {
            Some(p) => p,
            None => {
                // Unknown provider: the handle is unusable, so drop it. Treat as
                // stopped (no process we can reap through this registry).
                let mut guard = self.state.lock().await;
                *guard = TunnelState::Idle;
                return Ok(self.status_from(None));
            }
        };
        let provider_id = handle.provider_id.clone();
        let stop_result = provider.stop(handle.clone()).await;

        // Phase 3 (locked): M1 — only clear the handle on success / NotFound /
        // Down (a tunnel that is genuinely gone). A recoverable error keeps the
        // handle so the caller can retry `stop`.
        let mut guard = self.state.lock().await;
        match stop_result {
            Ok(()) => {
                *guard = TunnelState::Idle;
                Ok(self.status_from(None))
            }
            Err(lucarne_remote::RemoteError::NotFound(_)) => {
                // The provider has no live child for this handle → already gone.
                *guard = TunnelState::Idle;
                Ok(self.status_from(None))
            }
            Err(e) => {
                // M1: recoverable error — keep the handle for a retry.
                warn!(provider = %provider_id, error = %e, "lucarned remote: tunnel stop failed; retaining handle");
                *guard = TunnelState::Running(handle);
                Err(RemoteControlError::Backend(e.to_string()))
            }
        }
    }

    async fn status(&self) -> RemoteControlStatus {
        // Phase 1 (locked): clone the handle out so the health await is lock-free.
        let handle = {
            let guard = self.state.lock().await;
            match &*guard {
                TunnelState::Running(handle) => Some(handle.clone()),
                _ => None,
            }
        };
        let Some(handle) = handle else {
            return self.status_from(None);
        };

        // Phase 2 (lock-free): H3 — ask the provider for live health.
        let health = match self.registry.lookup(&handle.provider_id) {
            Some(p) => p.health(&handle).await.unwrap_or(TunnelStatus::Unknown),
            None => TunnelStatus::Down,
        };

        if matches!(health, TunnelStatus::Down) {
            // H3: a dead tunnel is reaped so a future `start` can relaunch and
            // `/api/remote/status` reflects reality (running=false).
            let mut guard = self.state.lock().await;
            // Only clear if still the same running handle (avoid clobbering a
            // concurrent start/stop transition).
            if let TunnelState::Running(current) = &*guard {
                if current.opaque == handle.opaque {
                    info!("lucarned remote: tunnel health=Down; clearing handle (status)");
                    *guard = TunnelState::Idle;
                }
            }
            return self.status_from(None);
        }

        self.status_from(Some(&handle))
    }
}

impl DaemonRemoteControl {
    /// Resolve provider + config and spawn the tunnel (lock-free). H6a: log the
    /// provider's own `warnings(cfg)` instead of special-casing a provider id.
    async fn do_start(
        &self,
        params: RemoteStartParams,
    ) -> Result<TunnelHandle, RemoteControlError> {
        // G3: a CLI-supplied provider id overrides the daemon's configured one;
        // absent → fall back to the pre-configured provider.
        let provider_id = params
            .provider
            .as_deref()
            .filter(|p| !p.is_empty())
            .unwrap_or(self.config.provider.as_str())
            .to_string();
        let provider = self
            .registry
            .lookup(&provider_id)
            .ok_or_else(|| RemoteControlError::UnknownProvider(provider_id.clone()))?;
        // G3: merge CLI fields over the daemon's configured provider fields.
        let cfg = self.config.provider_config(&params.fields);

        // M7: let the provider validate its own config (e.g. cloudflared requires
        // a named-tunnel `public_url` when a `token` is present, and checks the
        // URL is well-formed). The daemon does NOT branch on the provider id; the
        // rule lives in the provider. A violation → typed BadConfig (400).
        if let Err(detail) = provider.validate_config(&cfg) {
            return Err(RemoteControlError::BadConfig(detail));
        }

        // H6a: log any provider-declared warnings about this config (e.g. a
        // cloudflared quick tunnel exposes terminal content at the CF edge). The
        // daemon does NOT special-case the provider id; the provider owns the text.
        for warning in provider.warnings(&cfg) {
            warn!(provider = %provider_id, "lucarned remote: {warning}");
        }

        let handle = provider
            .start(self.config.gateway_addr, &cfg)
            .await
            .map_err(map_remote_error)?;
        // SEC-011: audit the tunnel start (provider + public host only; never the
        // access token).
        info!(
            provider = %handle.provider_id,
            public_host = handle.public_url.host_str().unwrap_or(""),
            "lucarned remote: tunnel started"
        );
        Ok(handle)
    }
}

/// Map a [`lucarne_remote::RemoteError`] to a typed [`RemoteControlError`] (M2):
/// a missing config field → bad config (400), a not-found provider/handle →
/// not-found (404), everything else (spawn/parse/io) → a backend error (502).
fn map_remote_error(err: lucarne_remote::RemoteError) -> RemoteControlError {
    use lucarne_remote::RemoteError;
    match err {
        RemoteError::MissingField(_) | RemoteError::Parse(_) => {
            RemoteControlError::BadConfig(err.to_string())
        }
        RemoteError::NotFound(_) => RemoteControlError::UnknownProvider(err.to_string()),
        RemoteError::Spawn { .. } | RemoteError::Io(_) => {
            RemoteControlError::Backend(err.to_string())
        }
    }
}

/// Build the gateway [`AuthState`] from the full-access token plus the optional
/// read-only token (SEC-013). A configured `readonly_token` is validated
/// (SEC-008: non-whitespace, ≥32 chars) and wired as the read-only credential;
/// when absent the behaviour is the existing single-token all-or-nothing model.
fn build_auth(
    token: AccessToken,
    readonly_token: Option<&str>,
) -> Result<AuthState, Box<dyn std::error::Error>> {
    match readonly_token.filter(|t| !t.is_empty()) {
        Some(ro) => {
            let readonly = AccessToken::from_secret_validated(ro.to_string()).map_err(
                |e| -> Box<dyn std::error::Error> { format!("remote.readonly_token: {e}").into() },
            )?;
            info!("lucarned remote: read-only access token enabled (SEC-013)");
            Ok(AuthState::with_tokens(token, readonly))
        }
        None => Ok(AuthState::with_token(token)),
    }
}

/// Start the remote-access subsystem: connect the rmux monitor, build the
/// default-deny auth state, serve the loopback gateway (wiring the control
/// plane), auto-start the configured tunnel, and register graceful shutdown.
///
/// Returns the [`RemoteSubsystem`] handle (for daemon logging). Tunnel + gateway
/// run in spawned tasks; the tunnel is stopped when `shutdown` fires.
pub async fn spawn_remote_subsystem(
    config: RemoteRuntimeConfig,
    mut shutdown: watch::Receiver<bool>,
) -> Result<RemoteSubsystem, Box<dyn std::error::Error>> {
    // default-deny (L4): require a token. Generate one when absent unless the
    // operator explicitly opted into insecure exposure. SEC-008: an explicit
    // token must be validated (non-whitespace, ≥32 chars) and fail closed.
    let (auth, access_token) = match (&config.auth_token, config.insecure) {
        (Some(token), _) => {
            let token = AccessToken::from_secret_validated(token.clone())
                .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
            let secret = token.as_str().to_string();
            (build_auth(token, config.readonly_token.as_deref())?, Some(secret))
        }
        (None, false) => {
            let token = AccessToken::generate();
            let secret = token.as_str().to_string();
            warn!(
                "lucarned remote: no auth_token configured — generated an ephemeral \
                 access token for this session (set remote.auth_token to persist it)"
            );
            (build_auth(token, config.readonly_token.as_deref())?, Some(secret))
        }
        (None, true) => {
            warn!(
                "lucarned remote: INSECURE public exposure with NO access token — anyone \
                 reaching the tunnel can drive your terminals. This is RCE-equivalent."
            );
            (AuthState::disabled(), None)
        }
    };

    // H6b: resolve the trusted forwarded-identity policy from the CONFIGURED
    // provider's own contract (cloudflared → `cf-connecting-ip`), so the gateway
    // never hardcodes a provider header. The gateway trusts the header only
    // behind the loopback tunnel source. An unknown provider / no headers → the
    // safe socket-peer-only default.
    let registry = lucarne_remote::builtin();
    let forwarded_policy = registry
        .lookup(&config.provider)
        .map(|p| ForwardedIdentityPolicy::trusting(p.forwarded_identity_headers().iter().copied()))
        .unwrap_or_default();
    let auth = auth.with_forwarded_identity(forwarded_policy);

    // Terminal monitor (mirrors the system rmux daemon) — the gateway surface.
    let monitor = Arc::new(RmuxMonitor::connect().await?);
    let adopted = monitor.adopt_all().await?;
    info!(sessions = adopted.len(), "lucarned remote: adopted system rmux sessions");

    // H1: ONE shared ws-connection pool drives every ws route on the port — the
    // termgw `/ws` + `/agent` AND the merged `lucarne-web` `/chat` — so a single
    // `max_ws_connections` cap (plus the same idle/lifetime/inbound-frame-rate
    // limits) governs all of them. `/chat` previously bypassed all limits.
    let ws_pool = WsConnectionPool::new(GatewayLimits::default());

    // Web chat bridge rooted at the daemon working dir (the dual-mode web app's
    // chat half). Best-effort: a chat init failure must not block the gateway.
    //
    // SEC-001 / H1 / C1: the merged `/chat` ws does NOT inherit the gateway
    // router's auth/limits across `merge`, so we build it with `router_gated`,
    // which routes the chat ws through the SAME single-use ticket auth, the SAME
    // shared connection pool (one global cap), idle/lifetime close, inbound frame
    // rate, AND read-only access scope as termgw's ws routes.
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let chat_router = match WebChat::new(cwd, None).await {
        Ok(chat) => Some(lucarne_web::router_gated(
            chat,
            auth.clone(),
            ws_pool.clone(),
        )),
        Err(e) => {
            warn!(error = %e, "lucarned remote: web chat bridge unavailable; serving terminal-only");
            None
        }
    };

    // The daemon owns the tunnel lifecycle; a SEPARATE loopback control listener
    // (SEC-002) forwards `/api/remote/*` to it. H4: the control starts in the
    // Idle state; its state machine runs provider awaits lock-free.
    let control = Arc::new(DaemonRemoteControl {
        registry,
        config: config.clone(),
        access_token: access_token.clone(),
        state: Mutex::new(TunnelState::Idle),
    });

    // Serve the gateway bound to loopback (L3), default-deny enforced before
    // bind. Build the router so we can merge the (ticket-gated) chat bridge onto
    // one port (the single converter), matching the `webdev` example.
    //
    // SEC-002: the gateway router carries NO `/api/remote/*` control plane — that
    // moves to a distinct loopback listener below. The tunnel only ever targets
    // `gateway_addr`, so the control plane (and the `access_token` it returns) is
    // unreachable from anyone on the tunnel.
    let web_dir = std::path::PathBuf::from(
        std::env::var("LUCARNED_REMOTE_WEB").unwrap_or_else(|_| DEFAULT_WEB_DIR.to_string()),
    );
    let gateway_addr = config.gateway_addr;
    let control_addr = config.control_addr;
    // H1: build the gateway router on the SAME shared ws pool the chat router
    // uses, so `/ws` + `/agent` + `/chat` share one global connection cap.
    let mut app = lucarne_termgw::router_with_pool(monitor, web_dir, auth, ws_pool.clone());
    if let Some(chat) = chat_router {
        app = app.merge(chat);
    }
    let listener = tokio::net::TcpListener::bind(gateway_addr).await?;
    let bound = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(err) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            warn!(error = %err, "lucarned remote gateway stopped");
        }
    });
    info!(addr = %bound, "lucarned remote gateway listening (loopback)");

    // SEC-002: serve the loopback-only control plane on its OWN distinct port the
    // tunnel never targets. This separation — not peer-IP — is the trust boundary
    // that keeps `/api/remote/*` (and the `access_token`) off the tunnel.
    let control_for_plane = control.clone() as Arc<dyn RemoteControl>;
    let control_listener = tokio::net::TcpListener::bind(control_addr).await?;
    let control_bound = control_listener.local_addr()?;
    if !control_bound.ip().is_loopback() {
        return Err("remote control plane must bind a loopback address (SEC-002)".into());
    }
    tokio::spawn(async move {
        let control_app = lucarne_termgw::control_router(Some(control_for_plane));
        if let Err(err) = axum::serve(
            control_listener,
            control_app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            warn!(error = %err, "lucarned remote control plane stopped");
        }
    });
    info!(addr = %control_bound, "lucarned remote control plane listening (loopback-only, off-tunnel)");

    // Auto-start the configured tunnel (the CLI may also start/stop it later via
    // the loopback control plane). The daemon's auto-start uses its
    // pre-configured provider + fields — empty params (G3 override path is the
    // CLI's `/api/remote/start` body).
    let status = control
        .start(RemoteStartParams::default())
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let public_url = status
        .public_url
        .clone()
        .ok_or("remote tunnel started without a public URL")?;
    let provider = status
        .provider
        .clone()
        .unwrap_or_else(|| config.provider.clone());

    // Graceful shutdown: when the daemon signals shutdown, stop the tunnel so the
    // provider process (e.g. cloudflared) is reaped (mirrors the health subsystem
    // shutdown wiring).
    // A second receiver for the H3 health watcher below (the stop task moves the
    // original `shutdown`).
    let shutdown_rx_for_watcher = shutdown.clone();
    let shutdown_control = control.clone();
    tokio::spawn(async move {
        // Wait for the shutdown flag to flip to true.
        loop {
            if *shutdown.borrow() {
                break;
            }
            if shutdown.changed().await.is_err() {
                break;
            }
        }
        if let Err(err) = shutdown_control.stop().await {
            warn!(error = %err, "lucarned remote: tunnel stop on shutdown failed");
        } else {
            info!("lucarned remote tunnel stopped on shutdown");
        }
    });

    // H3: periodic health watcher / reaper. The provider's `health` (via
    // `status()`) detects a child that exited and reaps it (clearing the handle),
    // so a crashed tunnel is noticed and `/api/remote/status` reflects reality
    // even without a client status request. `status()` itself is a no-op when
    // Idle (nothing running), so this is cheap; it stops with the daemon.
    let watcher_control = control.clone();
    let mut watcher_shutdown = shutdown_rx_for_watcher;
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(REAPER_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    // Polling status() runs health() and reaps a Down child.
                    let _ = watcher_control.status().await;
                }
                changed = watcher_shutdown.changed() => {
                    if changed.is_err() || *watcher_shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });

    Ok(RemoteSubsystem {
        provider,
        public_url,
    })
}
