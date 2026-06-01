//! Dashboard rendering (opencode-style layout, Free decision F).
//!
//! Three regions stacked: a left `List` of panels (driven by [`App::list`] /
//! `ListState`), a right detail pane describing the focused panel, and a bottom
//! hint bar. The Sessions panel renders its rmux session list + action status,
//! the GoPublic panel renders tunnel status + the QR login modal, and the Config
//! panel renders the provider field form.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use super::app::{App, Panel};

/// Draw the whole dashboard for one frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    // Vertical split: body (fills) + a one-line hint bar at the bottom.
    let [body, hints] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Fill(1), Constraint::Length(1)])
        .areas(area);

    // Horizontal split inside the body: left panel list + right detail.
    let [left, right] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(20), Constraint::Fill(1)])
        .areas(body);

    draw_panel_list(frame, app, left);
    draw_detail(frame, app, right);
    draw_hints(frame, app.active, hints);
}

/// Left `List` of panels with the active one highlighted via `ListState`.
fn draw_panel_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = Panel::ALL
        .iter()
        .map(|panel| ListItem::new(Line::from(panel.title())))
        .collect();

    // Index-bound guard: never leave a selection past the end of the list.
    if let Some(idx) = app.list.selected() {
        if idx >= items.len() {
            app.list.select(items.len().checked_sub(1));
        }
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("lucarned"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut app.list);
}

/// Right detail pane for the focused panel. Sessions renders the real rmux
/// session list + action status; the others keep placeholder bodies until their
/// tasks land.
fn draw_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.active {
        Panel::Sessions => draw_sessions(frame, app, area),
        Panel::GoPublic => draw_go_public(frame, app, area),
        Panel::Config => draw_config(frame, app, area),
    }
}

/// Sessions panel detail: a stateful list of live rmux sessions (or an empty
/// hint), a selected-session detail line, and the last action status.
fn draw_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    // Split the detail pane: the session list (fills) + a 3-line status footer.
    let [list_area, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Fill(1), Constraint::Length(3)])
        .areas(area);

    let panel = &mut app.sessions;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Panel::Sessions.title());

    if panel.sessions.is_empty() {
        let empty = Paragraph::new("No running rmux sessions.\n\nPress r to refresh.").block(block);
        frame.render_widget(empty, list_area);
    } else {
        let items: Vec<ListItem> = panel
            .sessions
            .iter()
            .map(|row| {
                let label = if row.meta.is_empty() {
                    row.name.clone()
                } else {
                    format!("{}  —  {}", row.name, row.meta)
                };
                ListItem::new(Line::from(label))
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, list_area, &mut panel.list);
    }

    let selected = panel
        .selected_name()
        .map(|n| format!("selected: {n}"))
        .unwrap_or_else(|| "selected: (none)".to_string());
    let status = panel.status.clone().unwrap_or_default();
    let footer_body = format!("{selected}\n{status}");
    let footer_widget = Paragraph::new(footer_body)
        .block(Block::default().borders(Borders::ALL).title("status"));
    frame.render_widget(footer_widget, footer);
}

/// Go-Public panel detail: the current tunnel status + last action message, the
/// public URL + access token when up, and the centered high-contrast login-QR
/// modal overlaid when open (TASK-003). All control-plane / QR specifics live in
/// [`super::remote`]; this only lays out the body and asks it to draw the modal.
fn draw_go_public(frame: &mut Frame, app: &mut App, area: Rect) {
    let panel = &app.go_public;

    let running = panel.status.as_ref().map(|s| s.running).unwrap_or(false);
    let public_url = panel
        .status
        .as_ref()
        .and_then(|s| s.public_url.as_deref())
        .unwrap_or("(none)");
    let token = match panel.status.as_ref().and_then(|s| s.access_token.as_deref()) {
        Some(t) if !t.is_empty() => "set (hidden — open the QR to share)",
        _ => "(none)",
    };
    let message = panel.message.as_deref().unwrap_or("press r to fetch status");

    let body = format!(
        "Remote access: {state}\n\
         control plane: 127.0.0.1:{port}\n\
         public URL: {public_url}\n\
         access token: {token}\n\n\
         {message}",
        state = if running { "RUNNING" } else { "stopped" },
        port = panel.control_port,
    );
    let detail = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Panel::GoPublic.title()),
    );
    frame.render_widget(detail, area);

    // Overlay the centered login-QR modal when open (explicit high-contrast style
    // + too-small fallback are handled inside `render_qr_modal`).
    if panel.qr_open {
        super::remote::render_qr_modal(frame, panel, area);
    }
}

/// Config panel detail: a descriptor-driven provider-field editor. A stateful row
/// list (provider / ports / one row per provider descriptor field, with secret
/// values masked) over a status footer (load/save/validation result + current
/// edit buffer when editing). All provider specifics come from
/// [`super::config::ConfigPanel`] — this only lays out the rows it is handed.
fn draw_config(frame: &mut Frame, app: &mut App, area: Rect) {
    use super::config::Row;

    let [list_area, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Fill(1), Constraint::Length(3)])
        .areas(area);

    let panel = &mut app.config;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Panel::Config.title());

    // Build one display line per row. The label is panel-owned for the top-level
    // keys; provider fields carry the descriptor label + a `*` required marker.
    let items: Vec<ListItem> = panel
        .rows
        .iter()
        .map(|row| {
            let (label, value) = match row {
                Row::Provider => ("provider".to_string(), panel.row_display(row)),
                Row::GatewayPort => ("gateway port".to_string(), panel.row_display(row)),
                Row::ControlPort => ("control port".to_string(), panel.row_display(row)),
                Row::Field {
                    label, required, ..
                } => {
                    let mark = if *required { " *" } else { "" };
                    (format!("{label}{mark}"), panel.row_display(row))
                }
            };
            ListItem::new(Line::from(format!("{label}: {value}")))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, list_area, &mut panel.list);

    // Footer: while editing, show the in-progress buffer MASKED for a secret row
    // (the typed secret is never echoed); otherwise show the last status line.
    let footer_body = match &panel.editing {
        Some(buf) => {
            let secret = matches!(
                panel
                    .list
                    .selected()
                    .and_then(|i| panel.rows.get(i)),
                Some(Row::Field { secret: true, .. })
            );
            let shown = if secret {
                "•".repeat(buf.chars().count())
            } else {
                buf.clone()
            };
            format!("editing: {shown}_\n(Enter commit  Esc cancel)")
        }
        None => panel.status.clone().unwrap_or_default(),
    };
    let footer_widget = Paragraph::new(footer_body)
        .block(Block::default().borders(Borders::ALL).title("status"));
    frame.render_widget(footer_widget, footer);
}

/// Bottom hint bar (fixed v1 keybinds). Sessions shows its action keys; the other
/// panels show the shared navigation hints.
fn draw_hints(frame: &mut Frame, active: Panel, area: Rect) {
    let text = match active {
        Panel::Sessions => {
            "Tab/←→ panel   ↑↓ move   Enter attach   d detach   k/Del kill   a archive   r refresh   q quit"
        }
        Panel::GoPublic => {
            "Tab/←→ panel   s start   x stop   r status   Enter show QR   Esc close QR   q quit"
        }
        Panel::Config => {
            "Tab/←→ panel   ↑↓ row   Enter edit/cycle   s save   Esc cancel   q quit"
        }
    };
    let hints = Paragraph::new(text).style(Style::default());
    frame.render_widget(hints, area);
}
