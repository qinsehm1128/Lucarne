//! Boundary contract: `lucarne-rmux` is the SINGLE layer that names `rmux_sdk`.
//!
//! The terminal-monitor subsystem's boundary invariant (its `Cargo.toml`
//! comment + ADR `2026-05-30-rmux-terminal-monitor-subsystem.md`) is that the
//! live rmux-sdk binding is confined to this one crate — preview-API churn
//! stops here, and every value it emits is the rmux-free `lucarne-term`
//! vocabulary. This test is the POSITIVE counterpart to
//! `lucarne-term/tests/contract_boundary.rs` (which guards that `lucarne-term`
//! stays rmux-free): here we assert that `lucarne-rmux`'s own dependency graph
//! DOES contain `rmux-sdk`, so the binding lives exactly where the boundary
//! says it should.
//!
//! Mirrors `lucarne-adapter/tests/contract_boundary.rs`: it asserts on the
//! `cargo tree` dependency graph (same invocation + string parse) rather than a
//! compile-time check.

use std::process::Command;

#[test]
fn rmux_contract_depends_on_rmux_sdk() {
    // Same `cargo tree -p <crate> --no-dev` invocation + parse as the adapter
    // contract test. `--no-dev` keeps the comparison to the production graph;
    // should this cargo reject the flag (empty stdout), fall back to the plain
    // tree so the positive guard still validates the dependency presence.
    let stdout = cargo_tree(&["tree", "-p", "lucarne-rmux", "--no-dev"]);
    let stdout = if stdout.trim().is_empty() {
        cargo_tree(&["tree", "-p", "lucarne-rmux"])
    } else {
        stdout
    };
    assert!(
        stdout.contains("rmux-sdk"),
        "lucarne-rmux is the single layer that binds rmux_sdk — its dependency \
         graph must contain rmux-sdk:\n{stdout}"
    );
}

fn cargo_tree(args: &[&str]) -> String {
    let output = Command::new("cargo")
        .args(args)
        .output()
        .expect("run cargo tree");
    String::from_utf8(output.stdout).unwrap()
}
