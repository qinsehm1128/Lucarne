//! webdev — the combined dev runner: ONE converter (one port) serving the
//! terminal mirror (/ws), the agent chat (/chat), the CLI control API (/api),
//! and the dual-mode web app (static). This is exactly what cloudflared tunnels
//! for remote access; local just hits the port directly.
//!
//! ```text
//! WEBDEV_ADDR=127.0.0.1:7800 LUCARNE_CWD=$PWD cargo +nightly run -Zbuild-dir-new-layout \
//!   -p lucarne-web --example webdev
//! ```
//! Then open http://127.0.0.1:7800 — Terminal tab mirrors your system rmux,
//! Chat tab talks to a local agent (claude/codex if on PATH).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use lucarne_rmux::RmuxMonitor;
use lucarne_web::WebChat;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Terminal: monitor the system rmux daemon and adopt its sessions.
    let monitor = Arc::new(RmuxMonitor::connect().await?);
    let adopted = monitor.adopt_all().await?;
    eprintln!("webdev: adopted {} system rmux session(s)", adopted.len());

    // Periodically record which monitored panes are running an agent, into the
    // SQLite registry, so chat history only ever shows rmux-related sessions.
    {
        let monitor = monitor.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                for d in monitor.sessions().await {
                    if let Some(cwd) = &d.cwd {
                        if let Some(a) = lucarne_agentbind::bind(cwd) {
                            lucarne_agentbind::db::record(
                                &a.kind, &a.session_id, cwd, &d.id, &d.title,
                                &a.transcript.to_string_lossy(),
                            );
                        }
                    }
                }
            }
        });
    }

    // Chat: an agent runtime rooted at the working dir.
    let cwd = std::env::var("LUCARNE_CWD").unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string())
    });
    let chat = match WebChat::new(cwd.clone(), None).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("webdev: chat init failed: {e}");
            return Ok(());
        }
    };
    eprintln!("webdev: agent providers: {:?}", chat.providers());

    let addr: SocketAddr = std::env::var("WEBDEV_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:7800".to_string())
        .parse()?;
    let web_dir = PathBuf::from(std::env::var("WEBDEV_WEB").unwrap_or_else(|_| "web".to_string()));

    // ONE converter: terminal gateway + chat bridge merged onto one router/port.
    let app = lucarne_termgw::router(monitor, web_dir).merge(lucarne_web::router(chat));

    eprintln!("webdev: serving http://{addr}  (agent cwd: {cwd})");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
