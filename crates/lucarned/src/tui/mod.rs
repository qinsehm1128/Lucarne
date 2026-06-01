//! Interactive `lucarned tui` dashboard (Decision 1: the single interactive
//! entry; the old `term` binary is removed and its reusable logic migrated here).
//!
//! [`run`] owns the terminal lifecycle (Decision 6: robust restore):
//! 1. enter raw mode + the alternate screen,
//! 2. install a panic hook that restores the terminal (`disable_raw_mode` +
//!    `LeaveAlternateScreen`) BEFORE the previously-installed hook runs, so a
//!    panic never leaves the user with a broken terminal,
//! 3. run the draw/event loop,
//! 4. ALWAYS restore on every exit path (normal return or error).
//!
//! The submodules house the panel backends: [`sessions`] (rmux session control),
//! [`remote`] (go-public control plane + QR), and [`config`] (provider field
//! collection) — all migrated verbatim from the old `lucarne-termctl` CLI.

pub mod app;
pub mod config;
pub mod event;
pub mod remote;
pub mod sessions;
pub mod ui;

use std::io::{self, Stdout};

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use app::App;

/// Launch the full-screen dashboard. Returns `Ok(())` on a clean exit, or an
/// error string the CLI surfaces to the user. The terminal is restored on every
/// exit path (including panic, via the installed hook).
pub fn run() -> Result<(), String> {
    // Decision 6: install a panic hook that restores the terminal first, so a
    // panic from anywhere in the loop does not leave raw mode / the alternate
    // screen active. We chain to the previously-installed hook afterwards.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        previous_hook(info);
    }));

    enable_raw_mode().map_err(|e| format!("failed to enable raw mode: {e}"))?;
    let mut stdout = io::stdout();
    if let Err(e) = execute!(stdout, EnterAlternateScreen) {
        // Roll back raw mode before bailing so we never leave the terminal in a
        // half-initialized state.
        let _ = disable_raw_mode();
        return Err(format!("failed to enter alternate screen: {e}"));
    }

    let backend = CrosstermBackend::new(stdout);
    let result = Terminal::new(backend)
        .map_err(|e| format!("failed to build terminal: {e}"))
        .and_then(|mut terminal| run_loop(&mut terminal));

    // ALWAYS restore on exit, regardless of how the loop ended.
    let restore = restore_terminal().map_err(|e| format!("failed to restore terminal: {e}"));

    // Prefer surfacing the loop error; otherwise surface a restore error.
    result.and(restore)
}

/// The draw/event loop. Draws a frame, then blocks for one key event, until the
/// app requests to quit. A Sessions-panel attach request triggers the pop-out
/// handoff (suspend → run `rmux attach-session` → resume), which lives in the
/// sessions module so the loop never learns rmux/terminal-handoff specifics.
fn run_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<(), String> {
    let mut app = App::new();
    // Populate the session list before the first draw (construction is I/O-free).
    app.sessions.refresh();
    // Pull the current remote-access status so the Go-Public panel opens informed
    // (a missing/cold daemon just lands an error in its message line — no panic).
    app.go_public.refresh();
    // Resolve the lucarned.yaml path + seed the Config panel from its current
    // remote: section (a missing file just opens the form with defaults).
    app.config.load();
    while app.running {
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .map_err(|e| format!("draw failed: {e}"))?;
        match event::handle_next(&mut app).map_err(|e| format!("event read failed: {e}"))? {
            sessions::SessionAction::Attach(name) => {
                sessions::attach_handoff(terminal, &mut app.sessions, &name)?
            }
            sessions::SessionAction::None => {}
        }
    }
    Ok(())
}

/// Restore the terminal: leave the alternate screen and disable raw mode. Safe to
/// call more than once (e.g. from both the panic hook and the normal exit path).
fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen)?;
    disable_raw_mode()
}
