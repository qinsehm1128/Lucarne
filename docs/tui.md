# Lucarne TUI (`lucarned tui`)

An interactive, full-screen terminal dashboard for the rmux terminal-monitor
subsystem — opencode-style arrow-key navigation over the same thin operations the
daemon already exposes. It is the **single** interactive entry point for managing
mirrored rmux sessions and the public-access tunnel.

> **What changed:** the standalone `term` binary (`lucarne-termctl`) has been
> **removed**. Everything it did is now reachable through `lucarned tui`, and its
> reusable logic (control-plane calls, terminal QR rendering, rmux argv, archive,
> provider field collection) was migrated into `lucarned` — not rewritten.

---

## Build & launch

The TUI is gated behind the `tui` cargo feature (which implies `remote`). It pulls
in `ratatui` + `crossterm`; the **default `lucarned` build links none of them** and
stays a pure structured-message bridge (guarded by `tests/default_build_purity.rs`).

```bash
# from a release/feature build
lucarned tui

# from source (AGENTS.md build discipline: nightly + new build-dir layout)
cargo +nightly run -Zbuild-dir-new-layout -p lucarned --features tui -- tui
```

Running `lucarned tui` without the `tui` feature prints a clear "rebuild with
`--features tui`" error.

---

## Layout

```
┌─lucarned────────┐┌─<panel>──────────────────────────────────┐
│  Sessions       ││ <panel body: status / list / form>       │
│> Go Public      ││                                          │
│  Config         ││                                          │
└─────────────────┘└──────────────────────────────────────────┘
 Tab/←→ panel   <panel-specific keys>                  q quit     ← bottom hint bar
```

Left = panel list, right = the focused panel, bottom = a one-line hint bar that
always shows the active panel's keys. If the window is very short, enlarge it (or
reduce the font) so the hint bar is visible.

`Tab` / `←` `→` switch panels; `q` quits; the terminal is always restored on exit
(including on panic, via a process-level hook).

---

## Panels & keybindings

### Sessions — manage system rmux sessions

Lists live sessions on your **system rmux daemon** and acts on them by shelling the
native `rmux` CLI (no new IPC). Works standalone (no `lucarned` daemon needed).

| Key | Action |
|-----|--------|
| `↑` `↓` | Move selection |
| `Enter` | **Attach (pop-out):** suspends the TUI, hands the current terminal to `rmux attach-session`; on exit the TUI re-enters |
| `d` | Detach clients (the session keeps running) |
| `k` / `Del` | Kill the session |
| `a` | Archive: capture content into the shared store, then close |
| `r` | Refresh the list |

### Go Public — start/stop the public-access tunnel

Drives the daemon's loopback control plane (`/api/remote/{start,stop,status}` on
`127.0.0.1:7801` by default) and renders the login QR.

| Key | Action |
|-----|--------|
| `s` | **Start** remote access (go public) |
| `x` | Stop remote access |
| `r` | Refresh status |
| `Enter` | Show the login **QR** (when a tunnel is up) — scannable, high-contrast; falls back to the plain login URL if the terminal is too small |
| `Esc` | Close the QR modal |

> **Requires the `lucarned` daemon to be running.** The daemon owns the tunnel
> lifecycle (it serves the loopback control plane); the TUI is a thin front-end and
> never opens a tunnel itself. If the daemon is not running, the panel shows an
> actionable hint instead of a raw error:
>
> ```
> control plane unreachable on 127.0.0.1:7801 — the lucarned daemon isn't running.
> Start it first (`lucarned autostart start`, or `brew services start lucarned`).
> ```
>
> Actually opening a tunnel also needs `cloudflared` installed/configured (see the
> `remote:` section of `lucarned.yaml`).

End-to-end (two terminals):

```bash
# Terminal A — run the daemon (a --features tui build implies remote and serves
# the loopback control plane from boot; the tunnel starts lazily on `s`)
cargo +nightly run -Zbuild-dir-new-layout -p lucarned --features tui

# Terminal B — open the TUI, go to "Go Public", press `s`
cargo +nightly run -Zbuild-dir-new-layout -p lucarned --features tui -- tui
```

### Config — edit remote-access config

A provider-config editor driven entirely by the `lucarne_remote` provider
descriptors (no hardcoded provider/field names). Edits are written back to
`lucarned.yaml` with a timestamped backup and an atomic temp+rename; on unix the
config and backup are created `0o600`. Secret fields are masked on screen.

| Key | Action |
|-----|--------|
| `↑` `↓` | Move between fields |
| `Enter` | Edit the field / cycle the selected provider |
| `s` | Save (validates, e.g. rejects gateway port == control port) |
| `Esc` | Cancel the current edit |

---

## Architecture & boundaries

- **Single entry, one binary.** Only `lucarned` ships (the release installer
  packages `lucarned`); the TUI is `lucarned tui`. No second binary to install.
- **Zero new daemon IPC.** The three panels reuse existing surfaces only:
  the native `rmux` CLI + the shared archive store (Sessions), the loopback
  `/api/remote/*` control plane (Go Public), and `lucarne_remote::builtin()` +
  `write_config_with_backup` (Config).
- **Not a live mirror.** The TUI is a list/action console; it does not render live
  terminal content. The full interactive mirror lives in the web terminal view.
- **Provider boundary (AGENTS.md).** Provider specifics stay behind
  `lucarne_remote` descriptors; the TUI/common layers route opaque ids only.
- **Feature isolation.** `ratatui`/`crossterm`/`rmux` are compiled only under the
  `tui` feature; the default daemon build is unaffected.

See the decision record:
[`docs/decisions/2026-06-01-lucarned-tui-frontend.md`](decisions/2026-06-01-lucarned-tui-frontend.md).

## Migration from the old `term` CLI

| Old `term` command | Now |
|--------------------|-----|
| `term ls` | Sessions panel (list) |
| `term attach <id>` / `term enter <id>` | Sessions panel → `Enter` |
| `term detach <id>` | Sessions panel → `d` |
| `term kill <id>` | Sessions panel → `k` / `Del` |
| `term archive <id>` | Sessions panel → `a` |
| `term go-public` | Go Public panel → `s` |
