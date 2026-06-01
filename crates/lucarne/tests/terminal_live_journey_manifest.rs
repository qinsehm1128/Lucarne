//! Terminal-adjacent live/E2E safety-gate manifest.
//!
//! Route/unit tests are not enough for the rmux/web/remote entrypoint fusion:
//! these flows drive live agents through local, remote, external Web clients,
//! and terminal-adjacent paths. This manifest is intentionally executable so
//! future work cannot claim merge readiness without naming the live/E2E checks
//! that protect the real control path.

#[derive(Clone, Copy, Debug)]
struct LiveJourneyGate {
    id: &'static str,
    path: &'static str,
    required_evidence: &'static [&'static str],
}

const GATES: &[LiveJourneyGate] = &[
    LiveJourneyGate {
        id: "terminal_open_resume_live_agent",
        path: "local/web/terminal-adjacent",
        required_evidence: &[
            "open live agent session",
            "resume live agent session",
            "shares daemon LucarneCore runtime/control plane",
        ],
    },
    LiveJourneyGate {
        id: "terminal_submit_prompt",
        path: "external web client -> /agent/{id}",
        required_evidence: &[
            "submit prompt",
            "prompt reaches bound live pane",
            "user-visible response frame",
        ],
    },
    LiveJourneyGate {
        id: "terminal_streaming_events",
        path: "external web client -> /agent/{id}",
        required_evidence: &[
            "assistant streaming events",
            "reasoning/tool frames",
            "turn completion",
        ],
    },
    LiveJourneyGate {
        id: "terminal_approval_interrupt",
        path: "external web client -> /agent/{id}",
        required_evidence: &[
            "approval request surfaced",
            "approval decision routed",
            "interrupt routed to live agent",
        ],
    },
    LiveJourneyGate {
        id: "terminal_close_live_agent",
        path: "local/web/remote",
        required_evidence: &[
            "close live agent session",
            "runtime close observed",
            "control-plane live state detached",
        ],
    },
    LiveJourneyGate {
        id: "remote_readonly_refusal",
        path: "remote tunnel /ws /agent",
        required_evidence: &[
            "read-only token can mirror",
            "read-only prompt/input refused",
            "refusal happens before write side effect",
        ],
    },
    LiveJourneyGate {
        id: "terminal_reconnect_disconnect",
        path: "websocket reconnect/disconnect",
        required_evidence: &[
            "disconnect releases ws permit",
            "reconnect obtains fresh ticket",
            "terminal mirror and agent transcript state resynchronize",
        ],
    },
    LiveJourneyGate {
        id: "terminal_archive_close_core_isolation",
        path: "terminal archive-and-close",
        required_evidence: &[
            "archive captures bounded scrollback",
            "rmux session closes",
            "does not mutate LucarneCore.live_sessions",
        ],
    },
];

#[test]
fn terminal_live_journey_gate_is_complete() {
    let expected = [
        "terminal_open_resume_live_agent",
        "terminal_submit_prompt",
        "terminal_streaming_events",
        "terminal_approval_interrupt",
        "terminal_close_live_agent",
        "remote_readonly_refusal",
        "terminal_reconnect_disconnect",
        "terminal_archive_close_core_isolation",
    ];
    let actual = GATES.iter().map(|gate| gate.id).collect::<Vec<_>>();
    assert_eq!(actual, expected);
    for gate in GATES {
        assert!(
            !gate.path.trim().is_empty(),
            "{} must name the live path it protects",
            gate.id
        );
        assert!(
            gate.required_evidence.len() >= 3,
            "{} must name concrete live/E2E evidence, not only a route/unit assertion",
            gate.id
        );
    }
}

#[test]
fn terminal_live_journey_gate_mentions_core_isolation() {
    let archive = GATES
        .iter()
        .find(|gate| gate.id == "terminal_archive_close_core_isolation")
        .expect("archive gate");
    assert!(
        archive
            .required_evidence
            .iter()
            .any(|evidence| evidence.contains("LucarneCore.live_sessions")),
        "archive-and-close must explicitly guard against mutating LucarneCore.live_sessions"
    );
}
