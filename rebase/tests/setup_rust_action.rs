//! The pinned Rust toolchain install lives in one composite action
//! (Issue #125).
//!
//! Five workflows used to repeat the same `dtolnay/rust-toolchain` SHA and the
//! same `1.98.0` version, so a toolchain bump had to touch every one of them.
//! `.github/actions/setup-rust` now owns that pin and every workflow calls it.
//! The checkout stays in each workflow: a local action can only run once the
//! repository is checked out, and each caller keeps its own `ref` and
//! credential settings. These tests read the committed files and assert both.

use std::path::{Path, PathBuf};

/// The local composite action every Rust workflow must call.
const SETUP_RUST: &str = "./.github/actions/setup-rust";

/// The workflows that build or run Cargo and so need the toolchain.
const RUST_WORKFLOWS: [&str; 5] = [
    "cargo-audit.yml",
    "cargo-upgrade.yml",
    "ci.yml",
    "sbom.yml",
    "version-increment.yml",
];

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn workflow(name: &str) -> String {
    read(&format!(".github/workflows/{name}"))
}

/// Every committed workflow as `(file name, body)`.
fn all_workflows() -> Vec<(String, String)> {
    let dir = repo_root().join(".github/workflows");
    let mut found: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "yml" || ext == "yaml")
        })
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let body = std::fs::read_to_string(&path).expect("read workflow");
            (name, body)
        })
        .collect();
    found.sort();
    assert!(
        !found.is_empty(),
        "no workflows found — refusing to pass vacuously"
    );
    found
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn is_blank_or_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// Every `steps:` list in the file, each as its steps' lines, in order.
///
/// A step starts at a `- ` item one level under `steps:` and runs until the
/// next item at that indent; the list ends at the first line indented no
/// deeper than `steps:` itself.
fn step_lists(text: &str) -> Vec<Vec<Vec<String>>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut lists = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_end().trim_start() != "steps:" {
            i += 1;
            continue;
        }
        let steps_indent = indent_of(lines[i]);
        let mut steps: Vec<Vec<String>> = Vec::new();
        let mut item_indent = None;
        i += 1;
        while i < lines.len() {
            let line = lines[i];
            if is_blank_or_comment(line) {
                i += 1;
                continue;
            }
            let indent = indent_of(line);
            if indent <= steps_indent {
                break;
            }
            let starts_item = line.trim_start().starts_with("- ");
            if starts_item && item_indent.is_none_or(|at| at == indent) {
                item_indent = Some(indent);
                steps.push(Vec::new());
            }
            if let Some(step) = steps.last_mut() {
                // The item marker is dropped so every key reads the same way.
                let key_line = if starts_item && item_indent == Some(indent) {
                    format!("{}  {}", " ".repeat(indent), &line.trim_start()[2..])
                } else {
                    line.to_string()
                };
                step.push(key_line);
            }
            i += 1;
        }
        lists.push(steps);
    }
    lists
}

/// A key's value in a step (its own or one nested under `with:`), with any
/// trailing ` #` comment and surrounding quotes removed.
fn step_value(step: &[String], key: &str) -> Option<String> {
    step.iter().find_map(|line| {
        let rest = line.trim_start().strip_prefix(key)?.strip_prefix(':')?;
        let value = rest.split(" #").next().unwrap_or("").trim();
        Some(value.trim_matches('"').to_string())
    })
}

/// The raw `uses:` value with its version comment intact.
fn uses_line(step: &[String]) -> Option<String> {
    step.iter().find_map(|line| {
        line.trim_start()
            .strip_prefix("uses:")
            .map(|rest| rest.trim().to_string())
    })
}

fn uses(step: &[String]) -> Option<String> {
    step_value(step, "uses")
}

fn is_checkout(step: &[String]) -> bool {
    uses(step).is_some_and(|u| u.starts_with("actions/checkout@"))
}

fn is_setup_rust(step: &[String]) -> bool {
    uses(step).as_deref() == Some(SETUP_RUST)
}

/// The steps of the single job in `name` that calls the shared action.
fn rust_job_steps(name: &str) -> Vec<Vec<String>> {
    let body = workflow(name);
    let mut matching: Vec<Vec<Vec<String>>> = step_lists(&body)
        .into_iter()
        .filter(|steps| steps.iter().any(|s| is_setup_rust(s)))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "{name}: expected exactly one job calling {SETUP_RUST}"
    );
    matching.remove(0)
}

fn action_text() -> String {
    read(".github/actions/setup-rust/action.yml")
}

/// The `channel` pinned in `rust-toolchain.toml`.
fn pinned_channel() -> String {
    read("rust-toolchain.toml")
        .lines()
        .find_map(|line| line.trim().strip_prefix("channel"))
        .and_then(|rest| rest.trim().strip_prefix('='))
        .map(|value| value.trim().trim_matches('"').to_string())
        .expect("rust-toolchain.toml declares a channel")
}

#[test]
fn step_lists_parser_reads_nested_items_and_comments() {
    let text = "\
jobs:
  a:
    steps:
      # a comment
      - name: One
        uses: actions/checkout@abc # v1
        with:
          ref: Develop
      - uses: ./.github/actions/setup-rust
        with:
          components: rustfmt, clippy
  b:
    runs-on: x
";
    let lists = step_lists(text);
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].len(), 2);
    assert!(is_checkout(&lists[0][0]));
    assert_eq!(step_value(&lists[0][0], "ref").as_deref(), Some("Develop"));
    assert!(is_setup_rust(&lists[0][1]));
    assert_eq!(
        step_value(&lists[0][1], "components").as_deref(),
        Some("rustfmt, clippy")
    );
    assert!(step_lists("jobs: {}\n").is_empty());
}

#[test]
fn shared_action_is_composite_and_pins_the_installer_by_sha() {
    let text = action_text();
    let lists = step_lists(&text);
    assert_eq!(lists.len(), 1, "the action has one steps list");
    assert!(
        text.lines()
            .any(|l| l.trim().trim_matches('"') == "using: composite"
                || l.trim() == "using: \"composite\""),
        "setup-rust must be a composite action"
    );
    let installers: Vec<&Vec<String>> = lists[0]
        .iter()
        .filter(|s| uses(s).is_some_and(|u| u.starts_with("dtolnay/rust-toolchain@")))
        .collect();
    assert_eq!(installers.len(), 1, "exactly one toolchain installer step");

    let line = uses_line(installers[0]).unwrap();
    let (reference, comment) = line
        .split_once(" #")
        .expect("the installer pin carries a version comment");
    let sha = reference.trim().rsplit('@').next().unwrap();
    assert_eq!(sha.len(), 40, "pin is a full commit SHA: {sha}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "hex SHA: {sha}");
    assert!(!comment.trim().is_empty(), "version comment is not empty");
}

#[test]
fn shared_action_installs_the_channel_rust_toolchain_toml_pins() {
    let lists = step_lists(&action_text());
    let installer = lists[0]
        .iter()
        .find(|s| uses(s).is_some_and(|u| u.starts_with("dtolnay/rust-toolchain@")))
        .expect("installer step");
    assert_eq!(step_value(installer, "toolchain"), Some(pinned_channel()));
    // Callers that need extra components (CI's rustfmt and clippy) pass them
    // through; the action forwards them rather than dropping them.
    assert_eq!(
        step_value(installer, "components").as_deref(),
        Some("${{ inputs.components }}")
    );
}

#[test]
fn no_workflow_calls_the_toolchain_installer_directly() {
    for (name, body) in all_workflows() {
        for steps in step_lists(&body) {
            for step in steps {
                let direct = uses(&step).is_some_and(|u| u.starts_with("dtolnay/rust-toolchain@"));
                assert!(
                    !direct,
                    "{name}: install Rust through {SETUP_RUST}, not dtolnay/rust-toolchain directly"
                );
            }
        }
    }
}

#[test]
fn every_rust_workflow_checks_out_before_calling_the_shared_action() {
    for name in RUST_WORKFLOWS {
        let steps = rust_job_steps(name);
        let setup = steps.iter().position(|s| is_setup_rust(s)).unwrap();
        let checkout = steps
            .iter()
            .position(|s| is_checkout(s))
            .unwrap_or_else(|| panic!("{name}: the Rust job checks the repository out"));
        assert!(
            checkout < setup,
            "{name}: a local action only exists after checkout, so checkout must come first"
        );
    }
}

#[test]
fn each_caller_keeps_its_own_checkout_inputs() {
    for name in RUST_WORKFLOWS {
        let steps = rust_job_steps(name);
        let checkout = steps.iter().find(|s| is_checkout(s)).unwrap();
        assert_eq!(
            step_value(checkout, "persist-credentials").as_deref(),
            Some("false"),
            "{name}: no git credential survives the checkout"
        );
        let expected_ref = match name {
            "cargo-upgrade.yml" => Some("Develop"),
            "version-increment.yml" => Some("${{ github.event.pull_request.head.sha }}"),
            _ => None,
        };
        assert_eq!(
            step_value(checkout, "ref").as_deref(),
            expected_ref,
            "{name}: checkout ref"
        );
    }
    let version = rust_job_steps("version-increment.yml");
    let checkout = version.iter().find(|s| is_checkout(s)).unwrap();
    assert_eq!(step_value(checkout, "fetch-depth").as_deref(), Some("0"));
}

#[test]
fn ci_still_asks_for_rustfmt_and_clippy() {
    let steps = rust_job_steps("ci.yml");
    let setup = steps.iter().find(|s| is_setup_rust(s)).unwrap();
    assert_eq!(
        step_value(setup, "components").as_deref(),
        Some("rustfmt, clippy")
    );
}
