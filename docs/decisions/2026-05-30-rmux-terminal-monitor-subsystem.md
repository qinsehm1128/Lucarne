# rmux terminal-monitor subsystem

## Context

Lucarne is a structured agent message bridge: it spawns agent CLIs over plain
pipes and parses their stdout as NDJSON/JSON-RPC. It has no terminal emulation,
no PTY, and no multiplexer integration.

A new capability is required: monitor the terminal sessions on the user's
system-wide rmux daemon (rmux is a daemon-backed, tmux-compatible multiplexer,
v0.3.1), mirror them faithfully and interactively to a web client, allow a
session to be attached locally (popped into the user's default terminal) and
detached again (retracted) while the remote mirror keeps running, and expose a
thin CLI to list / create / attach / detach / kill those sessions.

The load-bearing mechanic — an SDK control-mode observer coexisting with a CLI
`attach-session` client on the same session, with `detach-client` leaving the
session alive — was validated against the running daemon in
`.workflow/scratch/spike4-attach-handoff.md` (all claims PASS).

## Decision

Add the terminal-monitor as a NEW core subsystem, parallel to `AgentRuntime`,
in two new crates:

- **`lucarne-term`** — the rmux-free vocabulary: terminal grid value types
  (`Cell`/`Color`/`Style`/`Cursor`/`PaneGrid`/delta), the self-authored snapshot
  differ (rmux exposes no native cell/row delta), the session registry, and
  terminal input. It carries NO `rmux-*` dependency, so the gateway, web channel,
  and CLI consume terminal types without pulling the preview SDK.
- **`lucarne-rmux`** — the live binding: `adapter` (the sole place that maps
  `rmux_sdk` value types into `lucarne-term`) and `monitor` (connect to the
  system daemon, adopt sessions, mirror panes, inject input). It is the ONLY
  crate that names `rmux_sdk`.

It is wired into `lucarned` behind an optional `remote` cargo feature so default
builds never pull the preview SDK.

### Monitor model

The monitor connects to the DEFAULT system socket — the same daemon the user's
own `rmux` uses — and observes it. Discovered sessions register as
`Origin::Adopted` (we observe; we do not own them); sessions created via the CLI
register as `Origin::Managed`. The SDK is a control-mode observer that coexists
with a CLI `attach-session` client on the same session, so "pop a session into a
local terminal / retract it" is rmux-native (`attach-session` / `detach-client`)
and needs no new daemon IPC.

### Boundary rules

1. `rmux_sdk` names live only in `lucarne-rmux` (`adapter` + `monitor`). Preview
   API churn stops at that boundary; nothing else in the workspace deserializes
   or matches rmux types.
2. The subsystem is NOT an `agent-sessions` provider. That layer parses external
   transcript FILES; a live terminal pane is not a transcript and must not be
   forced through provider parse/discovery/watch contracts (AGENTS.md).
3. The subsystem is NOT routed through the agent framer/dialect pipeline.
   Terminal bytes (a cell grid) and structured agent events are different data
   shapes; reusing the NDJSON pipeline would mean ANSI scraping, which is
   rejected.
4. Any persisted session metadata is a cold record: it must use
   `ControlPlaneSqliteStore` cold read/write APIs, not the startup hot path
   (see `2026-05-24-lazy-control-plane-state.md`).
5. Terminal scrollback/history reads must be bounded windows, never whole-pane
   scans — consistent with the existing watch/history hot-path discipline.
6. The PTY is never force-resized; viewport changes are hints and the renderer
   scales, so multiple mirror clients never fight over pane size.

## Rationale

The terminal data shape is fundamentally incompatible with Lucarne's structured
agent pipeline, so reuse there is neither possible nor desirable; a parallel
subsystem with its own typed contract is the clean fit. Splitting the rmux-free
vocabulary (`lucarne-term`) from the live binding (`lucarne-rmux`) confines the
preview SDK to one crate and lets every downstream consumer compile without it.
Feature-gating in `lucarned` keeps the default resident daemon lean (the release
profile is size-optimized) while making the capability opt-in.

Agent chat reaching the web client is a SEPARATE concern handled by a web
WebSocket agent bridge — peer in spirit to the Telegram/WeChat channels, but a
ws bridge that drives a Lucarne `AgentRuntime` session directly rather than a
`lucarne_channel::Channel` trait implementation — not by this subsystem. The web
app simply hosts both a terminal view and a chat view.

## Consequences

- `lucarned` gains an optional `remote` feature that starts the monitor and the
  terminal gateway; default builds are unaffected.
- The gateway (`lucarne-termgw`) consumes the monitor's grid fan-out, applies the
  per-client differ, and serves an interactive web terminal view + an HTTP
  control surface for the CLI.
- Pop-out/retract is implemented with rmux's own `attach-session` /
  `detach-client`; the thin CLI wraps the rmux binary + the gateway HTTP — no new
  control-plane IPC is introduced.
- `--no-default-features` and the default feature set both compile without
  `rmux-sdk`; only `--features rmux` pulls it.
