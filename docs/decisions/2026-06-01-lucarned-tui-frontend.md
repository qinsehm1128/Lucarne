# `lucarned tui` is the single interactive frontend

## Context

The rmux terminal-monitor subsystem (see
`2026-05-30-rmux-terminal-monitor-subsystem.md`) shipped a thin standalone
`term` CLI (`lucarne-termctl`, a `[[bin]] term`) to drive the local operator
flows: list / attach / detach / kill / archive rmux sessions, run `go-public` to
open the remote-access tunnel, and collect the tunnel provider's credential
fields.

This left the workspace with two binaries — `lucarned` (the resident daemon) and
`term` (the operator CLI) — but the official `cargo-dist` config only ships one:
`[workspace.metadata.dist] packages = ["lucarned"]`. The `term` binary was never
distributed, so users who installed Lucarne had no supported way to reach the
operator flows. Two binaries are also a UX burden: which one do I run, and which
features do I build it with?

The user clarified, over several rounds, that they want exactly one entry: a
full-screen, arrow-key-navigable dashboard launched as `lucarned tui`, covering
the rmux terminal-forwarding subsystem only — session list / attach (pop-out) /
detach / kill / archive, go-public start/stop/status with a scannable QR, and
remote-access config editing. They explicitly do NOT want a `term`-style command
interface, agent-conversation browsing, or live terminal-content mirroring (live
pane mirroring stays a web-client concern).

## Decision

Add a single interactive entry, `lucarned tui`, as the only operator frontend,
and remove the standalone `term` binary.

### Single entry behind the `tui` feature

`lucarned` gains a `tui` subcommand (wired through `lucarned-ctl`'s `Command`
enum + `main` dispatch) that launches a full-screen `ratatui` dashboard. The TUI
is compiled only under a new `tui` cargo feature, which **implies `remote`** (it
reuses the loopback control plane + the tunnel provider registry). The default
daemon build keeps `default = []`, so it compiles and links no `ratatui`, no
`crossterm`, and no rmux binding. A no-feature build still parses `tui` and
returns a clear "rebuild with `--features tui`" error.

The dashboard has three panels, switched with `Tab` / `←` `→`:

- **Sessions** — a list/action console over the system-wide rmux daemon:
  `Enter` attach (pop-out + return), `d` detach, `k`/`Del` kill, `a` archive,
  `r` refresh.
- **Go Public** — `s`/`x`/`r` drive the daemon's existing
  `/api/remote/{start,stop,status}` loopback control plane; `Enter` opens a QR
  modal of the login URL.
- **Config** — descriptor-driven provider field forms that save back to
  `lucarned.yaml`.

### Migrate `lucarne-termctl` logic and tests; do not rewrite

The reusable `term` logic — the go-public control-plane call (`call_remote_start`
and siblings), `render_terminal_qr`, `login_url`, the rmux argv helpers
(`list-sessions` / `attach-session` / `detach-client` / `kill-session`),
`archive_session`, and the provider field collection (`collect_fields` /
`prompt_field`) — was migrated verbatim into `lucarned`'s `tui` submodules,
together with its existing unit tests (the `go_public_tests` module moved over
unchanged). The `lucarne-termctl` crate (and its `[[bin]] term`) was deleted and
removed from the workspace `members` / `default-members`; the rmux-free
`lucarne-term` library crate is unrelated and untouched.

### TUI = list/action console, not a live pane mirror

The Sessions panel renders only session metadata (name + meta line); it never
pulls a pane cell grid. Live terminal-content mirroring remains the web client's
job (per the terminal-monitor subsystem ADR). Attach suspends the TUI (leaves raw
mode + the alternate screen), spawns and waits on `rmux attach-session`, and
re-enters the TUI on detach/exit — it deliberately does NOT `exec`-replace the
process (which the old `term attach` did), because that would kill the dashboard.

### QR rendered in a high-contrast modal with a URL fallback

`render_terminal_qr` produces a half-block Unicode grid. Inside the TUI it is
drawn in a CENTERED `ratatui` `Paragraph` modal with an EXPLICIT high-contrast
`Style` (`fg(Black).bg(White)`) so the QR stays scannable regardless of terminal
theme, with `Clear` rendered underneath. When the modal's inner rect is smaller
than the grid (or QR generation fails), it falls back to the plain login URL plus
a "terminal too small for QR — open this URL" hint.

### Reuse existing interfaces; zero new control-plane IPC

All three capabilities ride on interfaces that already exist:

- sessions → the rmux daemon's own CLI + `lucarne-archive`;
- go-public → the daemon's loopback `/api/remote/{start,stop,status}` routes
  (reusing the migrated `call_remote_start`);
- config → the `lucarne_remote::builtin()` provider registry for field
  descriptors + `write_config_with_backup` (reachable in-crate now that the TUI
  lives inside `lucarned`).

No new daemon control-plane IPC was introduced.

### Amendment of locked merge-scope decision #5

The locked merge-scope decision #5 — "self-made TUI dropped" — is **explicitly
amended** by this ADR: a self-authored TUI is, in fact, the chosen single
frontend. Locked decision #6 — "thin wrapper, no new control-plane IPC" — is
**preserved in spirit and in fact**: the TUI still shells the rmux binary and
hits the daemon's existing control plane, adding zero new IPC. Only #5 is
relaxed; the rmux-rc six locked decisions and the rest of the merge scope are
unchanged.

## Rationale

One binary matches what `cargo-dist` actually ships and removes the "which tool?"
ambiguity. A full-screen dashboard is a better fit than a command CLI for the
list-driven, interactive operator flows (browse sessions, watch tunnel status,
scan a QR), and it can reuse the migrated, already-tested `term` logic without a
rewrite. Feature-gating behind `tui` (implies `remote`) keeps the resident daemon
— whose release profile is size-optimized — a pure message bridge in its default
build, with no `ratatui`/`crossterm`/rmux linked. The control-plane-only posture
keeps the new frontend a thin layer over stable APIs, honoring the no-new-IPC
constraint.

## Consequences

- `lucarned` gains a `tui` feature that pulls `ratatui` 0.30 + `crossterm` 0.29
  (one crossterm major, via ratatui's bundled backend) plus the migrated
  `lucarne-archive` / `qrcode` / `rpassword` helpers — all `optional = true` and
  compiled only under `tui`.
- The default `lucarned` build is unchanged: it links none of the TUI stack and
  remains a pure bridge.
- A durable purity guard — `crates/lucarned/tests/default_build_purity.rs` —
  asserts (via `cargo tree -p lucarned` with no features) that the default build
  links no `ratatui`, `crossterm`, or rmux crate. It runs in every `cargo test`
  / `cargo test --workspace`, so a future change that re-pulls the TUI stack into
  the default build fails CI/tests. The release workflow (`release.yml`) is
  `cargo-dist`-autogenerated and would clobber manual edits on regeneration, so
  the guard lives in the test suite rather than as a release-workflow step.
- The standalone `term` binary, the `lucarne-termctl` crate, and the `term`-style
  command docs are removed; `README.md`, `README.cn.md`, and `docs/commands.md`
  now document `lucarned tui` and its keybindings.

## Alternatives rejected

- **Keep the `term` CLI** (alongside or instead of the daemon) — leaves two
  binaries, one of which is never distributed, and keeps the operator flows
  unreachable for installed users.
- **`lucarned term <subcommand>` command-style subcommands** — re-creates the
  command interface the user explicitly rejected; worse fit for the interactive,
  list-driven flows.
- **`dialoguer`/`inquire`-style line prompts instead of a full-screen TUI** —
  cannot host a live session list, tunnel status, or an in-terminal QR modal; the
  user asked for an opencode-style dashboard.
- **Web-only (drop the local interactive entry entirely)** — the user wants a
  local, keyboard-driven operator console; the web client serves the remote
  viewing use case, not local operation.
