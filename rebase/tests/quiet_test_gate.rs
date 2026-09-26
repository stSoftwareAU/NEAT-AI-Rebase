//! A green gate is a quiet gate (Issue #122).
//!
//! `quality.sh` and `.github/workflows/ci.yml` both run the test suite on every
//! pull request. Without `-q` — and worse, with `--verbose` — each run prints a
//! line per passing test, burying the one failure a reviewer is looking for.
//! A failing test still prints its name, assertion and panic message under
//! `-q`, so quiet costs no signal. These tests read the committed gate files
//! and assert every `cargo test` invocation in them is quiet.

use std::path::{Path, PathBuf};

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

/// Every `cargo test` command line in a shell script or workflow.
///
/// Skips comments, and strips a workflow's inline `run:` prefix so a one-line
/// step and a line inside a `run: |` block are read the same way.
fn cargo_test_commands(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.strip_prefix("run:").map_or(line, str::trim_start))
        .filter(|line| line.starts_with("cargo test"))
        .map(str::to_string)
        .collect()
}

/// Whether a `cargo test` command line prints only a summary on a green run.
fn is_quiet(command: &str) -> bool {
    let flags: Vec<&str> = command.split_whitespace().collect();
    let quiet = flags.iter().any(|flag| matches!(*flag, "-q" | "--quiet"));
    let verbose = flags.iter().any(|flag| matches!(*flag, "-v" | "--verbose"));
    quiet && !verbose
}

/// Asserts the committed `file` runs the suite at least once, and always quietly.
fn assert_quiet_gate(file: &str) {
    let path = repo_root().join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let commands = cargo_test_commands(&text);

    assert!(
        !commands.is_empty(),
        "{file} no longer runs `cargo test` — the gate would pass without testing anything"
    );
    for command in commands {
        assert!(
            is_quiet(&command),
            "{file} runs `{command}` — a green run must be quiet: pass `-q` and drop `--verbose`"
        );
    }
}

#[test]
fn the_local_quality_gate_runs_the_suite_quietly() {
    assert_quiet_gate("quality.sh");
}

#[test]
fn the_ci_workflow_runs_the_suite_quietly() {
    assert_quiet_gate(".github/workflows/ci.yml");
}

#[test]
fn a_verbose_or_unflagged_run_is_not_quiet() {
    assert!(!is_quiet("cargo test --workspace --all-features"));
    assert!(!is_quiet("cargo test --workspace --all-features --verbose"));
    assert!(!is_quiet("cargo test -q --workspace -v"));
    assert!(is_quiet("cargo test --workspace --all-features -q"));
    assert!(is_quiet("cargo test --quiet --workspace"));
    // `-q` inside another token is not the flag.
    assert!(!is_quiet("cargo test --workspace -- --test-threads=1 -qx"));
}

#[test]
fn commands_are_found_in_steps_and_blocks_but_not_comments() {
    let workflow = "\
# `cargo test --all` runs in CI.
      - name: Run tests
        run: cargo test --workspace -q
      - run: |
          set -euo pipefail
          cargo test --doc
";
    assert_eq!(
        cargo_test_commands(workflow),
        vec!["cargo test --workspace -q", "cargo test --doc"]
    );
    assert!(cargo_test_commands("echo \"Running tests...\"\n").is_empty());
}
