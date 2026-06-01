//! Default-build fusion guard.
//!
//! `lucarned` is the product entry. Remote access, terminal gateway wiring, the
//! local TUI, and the live rmux binding must be present in the default build so
//! release/install users do not need source-build feature flags.

use std::process::Command;

const REQUIRED: &[&str] = &[
    "ratatui",
    "crossterm",
    "lucarne-rmux",
    "lucarne-termgw",
    "lucarne-remote",
    "rmux-sdk",
];

fn tree_crate_names(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start_matches(|c: char| {
                c.is_whitespace() || matches!(c, '|' | '`' | '+' | '-' | '├' | '│' | '└' | '─')
            });
            let name = trimmed.split_whitespace().next()?;
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

#[test]
fn default_lucarned_build_contains_terminal_gateway_tui_and_rmux_stack() {
    let output = Command::new("cargo")
        .args([
            "+nightly",
            "tree",
            "-Zbuild-dir-new-layout",
            "-p",
            "lucarned",
        ])
        .output()
        .expect("failed to run `cargo +nightly tree -Zbuild-dir-new-layout -p lucarned`");

    assert!(
        output.status.success(),
        "cargo tree failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let names = tree_crate_names(&stdout);
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|required| !names.iter().any(|name| name == required))
        .collect();

    assert!(
        missing.is_empty(),
        "default `lucarned` build must include fused remote/TUI/rmux crates; \
         missing {:?}. Full tree:\n{}",
        missing,
        stdout,
    );
}

#[test]
fn tree_crate_names_parses_leftmost_name() {
    let sample = "\
lucarned v0.4.2 (/path)
├── lucarne-termgw v0.4.2 (/path)
│   └── crossterm v0.29.0 (*)
└── lucarne-rmux v0.4.2 (/path)
";
    let names = tree_crate_names(sample);
    assert!(names.contains(&"lucarned".to_string()));
    assert!(names.contains(&"lucarne-termgw".to_string()));
    assert!(names.contains(&"crossterm".to_string()));
    assert!(names.contains(&"lucarne-rmux".to_string()));
}
