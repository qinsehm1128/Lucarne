//! lucarne-term — the thin `term` CLI for the rmux terminal monitor.
//!
//! Wraps the `rmux` binary + the shared [`lucarne_archive`] store, so it works
//! standalone (no running gateway needed) and shares archives with the web. It
//! drives the SAME system rmux daemon the gateway monitors, so "pop out / retract"
//! are rmux-native and no control-plane IPC is introduced.

use std::process::{exit, Command};

const HELP: &str = "\
lucarne term — control the rmux sessions the gateway monitors

USAGE:
    lucarne-term <COMMAND> [ARGS]

SESSION:
    ls                        list live sessions on the system rmux daemon
    new [NAME]                create a detached session (default: lucarne-<pid>)
    attach <NAME>             pop the session out into THIS terminal (= enter)
    enter  <NAME>             alias for attach
    detach <NAME>             retract: detach clients, the session keeps running
    kill   <NAME>             delete (kill) the session

ARCHIVE:
    archive <NAME>            capture the session's content, then close it
    archives                  list archived sessions
    show    <ARCHIVE_ID>      print an archived session's preserved content
    restore <ARCHIVE_ID>      reopen a shell at the archived cwd + print content

AGENT:
    resume <SESSION_ID> [CWD]  resume a claude session in a new rmux pane
                               (runs `claude --resume <SESSION_ID>`)

REMOTE:
    go-public [BACKEND]        expose the gateway publicly via a tunnel backend
    remote    [BACKEND]        alias for go-public
                               picks a built-in provider (interactive if omitted),
                               prompts its required fields, drives the daemon
                               loopback control API, prints the public URL + a
                               terminal QR of the login link + the access key.
                               Flags: --gateway-port <P> (default 7800),
                                      --control-port <P> (default: gateway-port + 1;
                                          the loopback-only control plane the tunnel
                                          never targets — SEC-002),
                                      --dry-run (assemble + print, never call the daemon),
                                      --<field>=<value> (preset a provider field, non-interactive)

    help                      show this help
";

fn rmux_bin() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let p = std::path::PathBuf::from(home).join(".cargo/bin/rmux");
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    "rmux".to_string()
}

/// Run `rmux <args>` inheriting stdio; return its exit code.
fn run(args: &[&str]) -> i32 {
    match Command::new(rmux_bin()).args(args).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("term: failed to run rmux: {e}");
            1
        }
    }
}

/// Run `rmux <args>` and capture stdout (None on failure).
fn rmux_out(args: &[&str]) -> Option<String> {
    let out = Command::new(rmux_bin()).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn pane_cwd(name: &str) -> Option<String> {
    rmux_out(&["display-message", "-p", "-t", name, "#{pane_current_path}"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Pop a session into the CURRENT terminal (unix: replace the process).
fn attach(name: &str) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(rmux_bin())
            .args(["attach-session", "-t", name])
            .exec();
        eprintln!("term attach: {err}");
        exit(1);
    }
    #[cfg(not(unix))]
    {
        exit(run(&["attach-session", "-t", name]));
    }
}

/// Capture a session's content into the shared archive store, then close it.
fn archive_session(name: &str) -> ! {
    let session_id = format!("{name}:0:0");
    let cwd = pane_cwd(name);
    let content = rmux_out(&["capture-pane", "-p", "-S", "-", "-t", name]).unwrap_or_default();
    match lucarne_archive::save(&session_id, name, cwd.as_deref(), &content, lucarne_archive::now_epoch()) {
        Ok(archive_id) => {
            run(&["kill-session", "-t", name]);
            println!("archived '{name}' -> {archive_id}  (terminal closed, content preserved)");
            exit(0);
        }
        Err(e) => {
            eprintln!("term archive: {e}");
            exit(1);
        }
    }
}

fn list_archives() -> ! {
    let items = lucarne_archive::list();
    if items.is_empty() {
        println!("(no archived sessions)");
    }
    for m in items {
        println!("{}\t{}\t{}", m.archive_id, m.title, m.cwd.unwrap_or_default());
    }
    exit(0);
}

fn show_archive(archive_id: &str) -> ! {
    match lucarne_archive::get(archive_id) {
        Some(rec) => {
            print!("{}", rec.content);
            exit(0);
        }
        None => {
            eprintln!("term show: no such archive '{archive_id}'");
            exit(1);
        }
    }
}

/// Reopen a shell at the archived cwd (the original process is gone) and print
/// the preserved content.
fn restore_archive(archive_id: &str) -> ! {
    let Some(rec) = lucarne_archive::get(archive_id) else {
        eprintln!("term restore: no such archive '{archive_id}'");
        exit(1);
    };
    let short: String = archive_id.chars().take(16).collect();
    let new_name = format!("restored-{short}");
    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", &new_name];
    if let Some(cwd) = &rec.cwd {
        args.push("-c");
        args.push(cwd.as_str());
    }
    run(&args);
    println!("# restored '{}' (was session {})", rec.title, rec.session_id);
    println!(
        "# reopened a shell at {} as: {}   (term attach {})",
        rec.cwd.clone().unwrap_or_else(|| ".".to_string()),
        new_name,
        new_name
    );
    println!("# --- preserved content ---");
    print!("{}", rec.content);
    exit(0);
}

/// Resume a claude agent session inside a new rmux pane.
fn resume_agent(session_id: &str, cwd: Option<&str>) -> ! {
    let short: String = session_id.chars().take(8).collect();
    let new_name = format!("resumed-{short}");
    let mut args: Vec<&str> = vec!["new-session", "-d", "-s", &new_name];
    if let Some(c) = cwd {
        args.push("-c");
        args.push(c);
    }
    args.extend_from_slice(&["claude", "--resume", session_id]);
    let code = run(&args);
    if code == 0 {
        println!("resuming claude {session_id} in rmux session: {new_name}");
        println!("  term attach {new_name}");
    }
    exit(code);
}

fn arg<'a>(args: &'a [String], i: usize, cmd: &str, what: &str) -> &'a str {
    match args.get(i) {
        Some(v) => v.as_str(),
        None => {
            eprintln!("term {cmd}: missing <{what}>");
            exit(2);
        }
    }
}

// ---- go-public: expose the gateway publicly via the daemon's tunnel ----
//
// The CLI stays a thin wrapper (Locked decision L6): it never spawns a tunnel
// itself. It picks a built-in provider, collects that provider's required
// fields, then drives the daemon's loopback-only control plane
// (`POST /api/remote/start`) so the daemon — which owns the tunnel lifecycle —
// brings the tunnel up and hands back the public URL + access token. The CLI
// then prints the URL, renders a terminal QR of the login link, and prints the
// access key.

/// Default loopback gateway port the daemon binds in remote mode
/// (`lucarned` `DEFAULT_REMOTE_GATEWAY_ADDR = 127.0.0.1:7800`). Overridable with
/// `--gateway-port <P>`.
const DEFAULT_GATEWAY_PORT: u16 = 7800;

/// Default loopback CONTROL-plane port (SEC-002): the daemon serves
/// `/api/remote/*` on a DISTINCT loopback port the tunnel never targets
/// (`lucarned` `DEFAULT_REMOTE_CONTROL_ADDR = 127.0.0.1:7801`). When not given
/// explicitly it is derived as `gateway-port + 1`, matching the daemon's default.
/// Overridable with `--control-port <P>`.
///
/// L1: uses `checked_add` so a gateway bound to port 65535 does not silently
/// wrap to 0 — `None` means the caller must pass an explicit `--control-port`.
fn default_control_port(gateway_port: u16) -> Option<u16> {
    gateway_port.checked_add(1)
}

/// What `go-public` resolved before touching the network: the selected provider,
/// the collected (non-secret-aware) field map, and the loopback URL it will POST.
/// Keeping this assembly pure makes the `--dry-run`/non-interactive path
/// unit-testable without a running daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GoPublicPlan {
    provider: String,
    fields: std::collections::BTreeMap<String, String>,
    control_url: String,
}

/// Assemble the control-plane request for `provider` from `fields`, validating
/// the provider exists in the built-in registry and all its `required` fields
/// are present. Pure (no I/O), so the dry-run path can be asserted in tests.
///
/// SEC-002: the request targets the CONTROL port (the loopback-only control
/// plane), NOT the public gateway port — `/api/remote/*` no longer lives on the
/// tunneled gateway router.
fn build_go_public_plan(
    provider: &str,
    fields: std::collections::BTreeMap<String, String>,
    control_port: u16,
) -> Result<GoPublicPlan, String> {
    let registry = lucarne_remote::builtin();
    let p = registry
        .get(provider)
        .ok_or_else(|| format!("unknown provider `{provider}` (known: {:?})", registry.ids()))?;
    for field in p.required_fields() {
        if field.required {
            let present = fields.get(field.key).is_some_and(|v| !v.is_empty());
            if !present {
                return Err(format!("missing required field `{}`", field.key));
            }
        }
    }
    Ok(GoPublicPlan {
        provider: provider.to_string(),
        fields,
        control_url: format!("http://127.0.0.1:{control_port}/api/remote/start"),
    })
}

/// Render `content` as a small terminal QR code (half-block rows). Mirrors
/// `lucarne-wechat`'s `render_terminal_qr` (`adapter.rs:900-938`) so the visual
/// is identical to the WeChat login QR.
fn render_terminal_qr(content: &str) -> Result<String, qrcode::types::QrError> {
    use qrcode::types::Color;
    use qrcode::{EcLevel, QrCode};

    let code = QrCode::with_error_correction_level(content.trim().as_bytes(), EcLevel::L)?;
    let module_count = code.width();
    let modules = code.to_colors();
    let color_at = |row: usize, col: usize| modules[row * module_count + col];

    let odd_row = module_count % 2 == 1;
    let output_rows = module_count.div_ceil(2);
    let mut output = String::new();

    output.push_str(&"▄".repeat(module_count + 2));
    output.push('\n');

    for row in 0..output_rows {
        output.push('█');
        for col in 0..module_count {
            let top = color_at(row * 2, col);
            let bottom = if row * 2 + 1 < module_count {
                color_at(row * 2 + 1, col)
            } else {
                Color::Light
            };
            output.push(match (top, bottom) {
                (Color::Light, Color::Light) => '█',
                (Color::Light, Color::Dark) => '▀',
                (Color::Dark, Color::Light) => '▄',
                (Color::Dark, Color::Dark) => ' ',
            });
        }
        output.push('█');
        output.push('\n');
    }

    if !odd_row {
        output.push_str(&"▀".repeat(module_count + 2));
        output.push('\n');
    }

    Ok(output)
}

/// The login URL a remote client opens: the public URL with the access token
/// carried in the fragment (`#token=…`), matching the gateway's
/// `RemoteControlStatus` doc contract. Returns the bare URL when no token.
fn login_url(public_url: &str, access_token: Option<&str>) -> String {
    match access_token {
        Some(token) if !token.is_empty() => format!("{public_url}#token={token}"),
        _ => public_url.to_string(),
    }
}

/// Parsed `go-public` invocation flags.
struct GoPublicArgs {
    backend: Option<String>,
    gateway_port: u16,
    /// Explicit control-plane port (SEC-002); `None` → derive `gateway_port + 1`.
    control_port: Option<u16>,
    dry_run: bool,
    /// Preset field values (`--<key>=<value>`) for the non-interactive path.
    preset_fields: std::collections::BTreeMap<String, String>,
}

/// Parse the `go-public` argument tail (everything after the subcommand).
fn parse_go_public_args(args: &[String]) -> Result<GoPublicArgs, String> {
    let mut backend = None;
    let mut gateway_port = DEFAULT_GATEWAY_PORT;
    let mut control_port: Option<u16> = None;
    let mut dry_run = false;
    let mut preset_fields = std::collections::BTreeMap::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--dry-run" => dry_run = true,
            "--gateway-port" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--gateway-port requires a value".to_string())?;
                gateway_port = v
                    .parse()
                    .map_err(|_| format!("invalid --gateway-port `{v}`"))?;
            }
            _ if a.starts_with("--gateway-port=") => {
                let v = &a["--gateway-port=".len()..];
                gateway_port = v
                    .parse()
                    .map_err(|_| format!("invalid --gateway-port `{v}`"))?;
            }
            "--control-port" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--control-port requires a value".to_string())?;
                control_port = Some(
                    v.parse()
                        .map_err(|_| format!("invalid --control-port `{v}`"))?,
                );
            }
            _ if a.starts_with("--control-port=") => {
                let v = &a["--control-port=".len()..];
                control_port = Some(
                    v.parse()
                        .map_err(|_| format!("invalid --control-port `{v}`"))?,
                );
            }
            // `--<field>=<value>` presets a provider field (non-interactive).
            _ if a.starts_with("--") && a.contains('=') => {
                let body = &a[2..];
                let (key, value) = body.split_once('=').unwrap();
                preset_fields.insert(key.to_string(), value.to_string());
            }
            _ if a.starts_with('-') => {
                return Err(format!("unknown flag `{a}`"));
            }
            // First positional is the backend id.
            _ if backend.is_none() => backend = Some(a.to_string()),
            _ => return Err(format!("unexpected argument `{a}`")),
        }
        i += 1;
    }

    Ok(GoPublicArgs {
        backend,
        gateway_port,
        control_port,
        dry_run,
        preset_fields,
    })
}

/// Interactively pick a provider id from the built-in registry (stdlib stdin,
/// mirroring `onboarding/terminal.rs`).
fn prompt_provider(ids: &[&'static str]) -> Result<String, String> {
    use std::io::{self, BufRead, Write};

    println!("Available remote-access backends:");
    for (n, id) in ids.iter().enumerate() {
        println!("  {}) {}", n + 1, id);
    }
    print!("Choose a backend [1]: ");
    io::stdout().flush().map_err(|e| e.to_string())?;

    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(ids[0].to_string());
    }
    // Accept either an index or a literal id.
    if let Ok(n) = line.parse::<usize>() {
        if n >= 1 && n <= ids.len() {
            return Ok(ids[n - 1].to_string());
        }
        return Err(format!("choice {n} out of range"));
    }
    if ids.contains(&line) {
        return Ok(line.to_string());
    }
    Err(format!("unknown backend `{line}`"))
}

/// Prompt for one provider field over stdin. Secret fields (L2) are read with
/// terminal echo disabled (`rpassword`) so the value — e.g. a cloudflare tunnel
/// token — is never echoed onto the visible prompt line; non-secret fields use a
/// plain line read.
fn prompt_field(field: &lucarne_remote::RequiredField) -> Result<String, String> {
    use std::io::{self, BufRead, Write};

    let suffix = match (field.required, field.secret) {
        (true, true) => " (required, secret)",
        (true, false) => " (required)",
        (false, true) => " (optional, secret)",
        (false, false) => " (optional)",
    };
    if field.secret {
        // L2: no-echo read so the secret never appears on screen.
        let prompt = format!("{}{}: ", field.label, suffix);
        let value = rpassword::prompt_password(prompt).map_err(|e| e.to_string())?;
        return Ok(value.trim_end_matches(['\r', '\n']).to_string());
    }
    print!("{}{}: ", field.label, suffix);
    io::stdout().flush().map_err(|e| e.to_string())?;

    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Resolve the provider id: explicit backend arg, else interactive choice.
fn resolve_provider(backend: Option<&str>) -> Result<String, String> {
    let registry = lucarne_remote::builtin();
    let ids = registry.ids();
    if ids.is_empty() {
        return Err("no built-in remote-access providers registered".to_string());
    }
    match backend {
        Some(b) => {
            if registry.get(b).is_none() {
                return Err(format!("unknown provider `{b}` (known: {:?})", ids));
            }
            Ok(b.to_string())
        }
        None => prompt_provider(&ids),
    }
}

/// Collect this provider's fields: every preset is forwarded (advertised or
/// not — e.g. cloudflared's named-tunnel `public_url` is a config field that is
/// not advertised in `required_fields()`), and any *advertised* field still
/// missing is prompted for when `interactive`.
fn collect_fields(
    provider: &str,
    presets: &std::collections::BTreeMap<String, String>,
    interactive: bool,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let registry = lucarne_remote::builtin();
    let p = registry
        .get(provider)
        .ok_or_else(|| format!("unknown provider `{provider}`"))?;
    // Start from all caller-supplied presets so non-advertised config fields
    // (the named-tunnel `public_url`) still reach the daemon.
    let mut fields: std::collections::BTreeMap<String, String> = presets
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for field in p.required_fields() {
        if fields.contains_key(field.key) {
            continue;
        }
        let value = if interactive {
            prompt_field(field)?
        } else {
            String::new()
        };
        if !value.is_empty() {
            fields.insert(field.key.to_string(), value);
        }
    }
    Ok(fields)
}

/// POST `/api/remote/start` to the daemon loopback control plane and parse the
/// `RemoteControlStatus` response. The CLI sends the chosen provider id + that
/// provider's fields as the JSON body ([`RemoteStartParams`]); the daemon uses
/// them to override / merge its pre-configured tunnel (G3) and, on a cold daemon,
/// lazily brings the gateway + tunnel up on this first call.
fn call_remote_start(
    plan: &GoPublicPlan,
) -> Result<lucarne_remote_status::RemoteStartStatus, String> {
    let body = serde_json::json!({
        "provider": plan.provider,
        "fields": plan.fields,
    });
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(&plan.control_url)
        .json(&body)
        .send()
        .map_err(|e| format!("failed to reach daemon at {}: {e}", plan.control_url))?;
    if !resp.status().is_success() {
        let code = resp.status();
        let detail = resp.text().unwrap_or_default();
        return Err(format!("daemon returned {code}: {detail}"));
    }
    resp.json::<lucarne_remote_status::RemoteStartStatus>()
        .map_err(|e| format!("failed to parse daemon response: {e}"))
}

/// Mirror of `lucarne_termgw::RemoteControlStatus` for deserialization (the CLI
/// does not depend on the gateway crate; the JSON shape is the stable contract).
mod lucarne_remote_status {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    pub struct RemoteStartStatus {
        #[allow(dead_code)]
        pub running: bool,
        #[allow(dead_code)]
        pub provider: Option<String>,
        pub public_url: Option<String>,
        pub access_token: Option<String>,
    }
}

/// Print the public URL, the login QR, and the access key.
fn report_tunnel_up(status: &lucarne_remote_status::RemoteStartStatus) {
    match &status.public_url {
        Some(public_url) => {
            let url = login_url(public_url, status.access_token.as_deref());
            println!("\nRemote access is live.");
            println!("  public URL: {public_url}");
            match render_terminal_qr(&url) {
                Ok(qr) => {
                    println!("\nScan to open on your phone:\n{qr}");
                }
                Err(e) => {
                    eprintln!("term go-public: QR render failed: {e}");
                    println!("  login URL: {url}");
                }
            }
            match &status.access_token {
                Some(token) if !token.is_empty() => println!("  access key: {token}"),
                _ => println!("  access key: (none — insecure / no auth enforced)"),
            }
        }
        None => {
            eprintln!("term go-public: daemon reported no public URL (is the tunnel configured?)");
        }
    }
}

/// `go-public` entry point. Returns the process exit code.
fn go_public_cli(args: &[String]) -> i32 {
    let parsed = match parse_go_public_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("term go-public: {e}");
            return 2;
        }
    };

    // Non-interactive when a backend is given AND every field is preset (or
    // simply when running --dry-run with a backend): no prompting needed.
    let provider = match resolve_provider(parsed.backend.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("term go-public: {e}");
            return 2;
        }
    };

    // Interactive prompting only when fields are not all preset and we have a TTY
    // intent. We treat presence of the backend arg + presets as "non-interactive
    // enough"; the dry-run path never prompts.
    let interactive = !parsed.dry_run;
    let fields = match collect_fields(&provider, &parsed.preset_fields, interactive) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("term go-public: {e}");
            return 2;
        }
    };

    // SEC-002: target the loopback-only control port (the tunnel never targets
    // it), derived as gateway-port + 1 unless overridden with --control-port.
    // L1: if the gateway port is 65535 the derivation overflows — require an
    // explicit --control-port instead of silently wrapping to 0.
    let control_port = match parsed.control_port {
        Some(p) => p,
        None => match default_control_port(parsed.gateway_port) {
            Some(p) => p,
            None => {
                eprintln!(
                    "term go-public: gateway port {} leaves no room for a derived control port; \
                     pass --control-port explicitly",
                    parsed.gateway_port
                );
                return 2;
            }
        },
    };
    let plan = match build_go_public_plan(&provider, fields, control_port) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("term go-public: {e}");
            return 2;
        }
    };

    if parsed.dry_run {
        println!("[dry-run] provider: {}", plan.provider);
        let keys: Vec<&str> = plan.fields.keys().map(String::as_str).collect();
        println!("[dry-run] field keys: {keys:?}");
        println!("[dry-run] would POST: {}", plan.control_url);
        return 0;
    }

    println!(
        "Starting remote access via `{}` (daemon control plane: {}) …",
        plan.provider, plan.control_url
    );
    match call_remote_start(&plan) {
        Ok(status) => {
            report_tunnel_up(&status);
            0
        }
        Err(e) => {
            eprintln!("term go-public: {e}");
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "ls" | "list" => exit(run(&["list-sessions"])),
        "new" => {
            let name = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| format!("lucarne-{}", std::process::id()));
            let code = run(&["new-session", "-d", "-s", &name]);
            if code == 0 {
                println!("{name}");
            }
            exit(code);
        }
        "attach" | "enter" => attach(arg(&args, 1, cmd, "NAME")),
        "detach" => exit(run(&["detach-client", "-s", arg(&args, 1, "detach", "NAME")])),
        "kill" => exit(run(&["kill-session", "-t", arg(&args, 1, "kill", "NAME")])),
        "archive" => archive_session(arg(&args, 1, "archive", "NAME")),
        "archives" | "list-archives" => list_archives(),
        "show" => show_archive(arg(&args, 1, "show", "ARCHIVE_ID")),
        "restore" => restore_archive(arg(&args, 1, "restore", "ARCHIVE_ID")),
        "resume" => resume_agent(
            arg(&args, 1, "resume", "SESSION_ID"),
            args.get(2).map(String::as_str),
        ),
        "go-public" | "remote" => exit(go_public_cli(&args[1..])),
        "help" | "-h" | "--help" => print!("{HELP}"),
        other => {
            eprintln!("term: unknown command '{other}'\n");
            print!("{HELP}");
            exit(2);
        }
    }
}

#[cfg(test)]
mod go_public_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn dry_run_assembles_provider_fields_and_control_url() {
        // The dry-run / non-interactive path: provider lookup + field assembly +
        // the constructed loopback control URL — no daemon, no network. SEC-002:
        // the URL targets the CONTROL port, not the public gateway port.
        let plan = build_go_public_plan(
            "cloudflared",
            fields(&[("public_url", "https://demo.example.test")]),
            7801,
        )
        .expect("cloudflared plan");

        assert_eq!(plan.provider, "cloudflared");
        assert_eq!(
            plan.control_url,
            "http://127.0.0.1:7801/api/remote/start"
        );
        assert_eq!(
            plan.fields.get("public_url").map(String::as_str),
            Some("https://demo.example.test")
        );
    }

    #[test]
    fn control_port_flag_changes_control_url() {
        let plan = build_go_public_plan("cloudflared", BTreeMap::new(), 9009)
            .expect("plan with default cloudflared fields");
        assert_eq!(plan.control_url, "http://127.0.0.1:9009/api/remote/start");
    }

    #[test]
    fn control_port_defaults_to_gateway_port_plus_one() {
        // SEC-002: the CLI derives the control port from the gateway port + 1,
        // matching the daemon's default off-tunnel control listener.
        assert_eq!(default_control_port(7800), Some(7801));
        assert_eq!(default_control_port(9000), Some(9001));
        // L1: port 65535 + 1 overflows → None (explicit --control-port required).
        assert_eq!(default_control_port(65535), None);
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let err = build_go_public_plan("nope", BTreeMap::new(), 7800)
            .expect_err("unknown provider must fail");
        assert!(err.contains("unknown provider `nope`"), "got: {err}");
    }

    #[test]
    fn built_in_providers_are_enumerable() {
        // Mirrors the interactive-selection source of truth: the CLI lists what
        // RemoteRegistry::builtin() advertises.
        let registry = lucarne_remote::builtin();
        assert!(registry.ids().contains(&"cloudflared"));
    }

    #[test]
    fn parse_args_dry_run_backend_and_preset_field() {
        let args = vec![
            "cloudflared".to_string(),
            "--dry-run".to_string(),
            "--gateway-port".to_string(),
            "7810".to_string(),
            "--public_url=https://x.example.test".to_string(),
        ];
        let parsed = parse_go_public_args(&args).expect("parse");
        assert_eq!(parsed.backend.as_deref(), Some("cloudflared"));
        assert!(parsed.dry_run);
        assert_eq!(parsed.gateway_port, 7810);
        assert_eq!(
            parsed.preset_fields.get("public_url").map(String::as_str),
            Some("https://x.example.test")
        );
    }

    #[test]
    fn parse_args_defaults_to_gateway_7800() {
        let parsed = parse_go_public_args(&[]).expect("empty parse");
        assert_eq!(parsed.gateway_port, DEFAULT_GATEWAY_PORT);
        assert_eq!(parsed.gateway_port, 7800);
        assert!(parsed.control_port.is_none(), "control port derived by default");
        assert!(parsed.backend.is_none());
        assert!(!parsed.dry_run);
    }

    #[test]
    fn parse_args_accepts_explicit_control_port() {
        // Both spaced and `=` forms.
        let spaced = parse_go_public_args(&[
            "cloudflared".to_string(),
            "--control-port".to_string(),
            "7950".to_string(),
        ])
        .expect("parse spaced");
        assert_eq!(spaced.control_port, Some(7950));

        let eqform = parse_go_public_args(&[
            "cloudflared".to_string(),
            "--control-port=7951".to_string(),
        ])
        .expect("parse = form");
        assert_eq!(eqform.control_port, Some(7951));

        // Invalid value is rejected.
        assert!(parse_go_public_args(&[
            "--control-port".to_string(),
            "notaport".to_string()
        ])
        .is_err());
    }

    #[test]
    fn collect_fields_non_interactive_uses_presets_only() {
        // interactive=false must never block on stdin: only presets are used.
        let collected = collect_fields(
            "cloudflared",
            &fields(&[("token", "abc"), ("public_url", "https://t.example.test")]),
            false,
        )
        .expect("collect");
        assert_eq!(collected.get("token").map(String::as_str), Some("abc"));
        assert_eq!(
            collected.get("public_url").map(String::as_str),
            Some("https://t.example.test")
        );
    }

    #[test]
    fn login_url_appends_token_fragment() {
        assert_eq!(
            login_url("https://demo.example.test", Some("secret123")),
            "https://demo.example.test#token=secret123"
        );
        assert_eq!(
            login_url("https://demo.example.test", None),
            "https://demo.example.test"
        );
        // Empty token is treated as absent.
        assert_eq!(
            login_url("https://demo.example.test", Some("")),
            "https://demo.example.test"
        );
    }

    #[test]
    fn qr_renders_for_login_url() {
        let url = login_url("https://demo.example.test", Some("k"));
        let qr = render_terminal_qr(&url).expect("qr renders");
        // Half-block QR uses the block glyphs from the wechat renderer.
        assert!(qr.contains('█'));
        assert!(qr.lines().count() > 3);
    }

    #[test]
    fn end_to_end_dry_run_path_returns_success() {
        // Full non-interactive dry-run invocation: never touches the network.
        let args = vec![
            "cloudflared".to_string(),
            "--dry-run".to_string(),
            "--public_url=https://demo.example.test".to_string(),
        ];
        assert_eq!(go_public_cli(&args), 0);
    }
}
