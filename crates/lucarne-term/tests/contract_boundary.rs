//! Boundary contract: `lucarne-term` is the rmux-FREE terminal vocabulary.
//!
//! The terminal-monitor subsystem's boundary invariant (its `Cargo.toml`
//! comment + ADR `2026-05-30-rmux-terminal-monitor-subsystem.md`) is that
//! `lucarne-term` carries the wire/data types with NO `rmux-*` dependency, so
//! the gateway / web channel / CLI can compile the terminal vocabulary without
//! ever pulling the preview rmux SDK. The live rmux binding lives only in the
//! sibling `lucarne-rmux` crate.
//!
//! This mirrors `lucarne-adapter/tests/contract_boundary.rs`: it asserts on the
//! `cargo tree` dependency graph (same invocation + parse) rather than relying
//! on a compile error, so a regression that adds an rmux dependency to
//! `lucarne-term` is caught loudly here.

use std::process::Command;

#[test]
fn term_contract_has_no_rmux_dependency() {
    let output = Command::new("cargo")
        .args(["tree", "-p", "lucarne-term", "--no-dev"])
        .output()
        .expect("run cargo tree");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("rmux-sdk") && !stdout.contains("rmux_sdk") && !stdout.contains("rmux"),
        "lucarne-term must stay rmux-free (no rmux/rmux-sdk dependency):\n{stdout}"
    );
}
