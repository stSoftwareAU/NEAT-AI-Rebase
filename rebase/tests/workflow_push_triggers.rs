//! A checker workflow gates the pull request, not the merge commit (Issues #57,
//! #58).
//!
//! `.github/workflows/ci.yml` is a test/lint gate. Once it is a required status
//! check, a `push:` trigger on the default branch re-runs the whole gate on
//! every merge — a duplicate of the run that already passed on the PR, burning
//! CI minutes and able to redden the default branch for a check that already
//! went green. Deploy/publish workflows are different; a checker is not. These
//! tests read the committed workflow and assert its trigger set.

use std::path::{Path, PathBuf};

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

/// The top-level trigger names under a workflow's `on:` block.
///
/// Reads the mapping keys nested one level under `on:` (`pull_request`,
/// `push`, `schedule`, `workflow_dispatch`, …), skipping blank lines and
/// comments and stopping at the next top-level key. Panics when `on:` is
/// absent: a workflow with no trigger block is a real failure, not a vacuous
/// pass.
fn on_triggers(workflow: &str) -> Vec<String> {
    let mut lines = workflow.lines().skip_while(|line| line.trim_end() != "on:");
    lines.next().expect("workflow declares an `on:` block");

    let mut triggers = Vec::new();
    let mut nested_indent = None;
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 {
            break; // next top-level key — the `on:` block is finished.
        }
        let depth = *nested_indent.get_or_insert(indent);
        if indent == depth
            && let Some(name) = trimmed.split_once(':').map(|(name, _)| name)
        {
            triggers.push(name.to_string());
        }
    }
    triggers
}

/// The trigger names of the committed `.github/workflows/<file>`.
fn workflow_triggers(file: &str) -> Vec<String> {
    let path = repo_root().join(".github/workflows").join(file);
    let workflow = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    on_triggers(&workflow)
}

#[test]
fn on_triggers_reads_nested_keys_only() {
    let workflow = concat!(
        "name: CI\n",
        "\n",
        "on:\n",
        "  pull_request:\n",
        "    # a comment at trigger depth is not a trigger\n",
        "    types: [opened]\n",
        "    branches:\n",
        "      - Develop\n",
        "  schedule:\n",
        "    - cron: \"0 6 * * 1\"\n",
        "  workflow_dispatch:\n",
        "\n",
        "permissions:\n",
        "  contents: read\n",
    );
    assert_eq!(
        on_triggers(workflow),
        vec!["pull_request", "schedule", "workflow_dispatch"]
    );
}

#[test]
fn on_triggers_sees_a_push_trigger() {
    let workflow = concat!(
        "on:\n",
        "  pull_request:\n",
        "    branches: [\"*\"]\n",
        "  push:\n",
        "    branches:\n",
        "      - Develop\n",
        "\n",
        "jobs:\n",
    );
    assert_eq!(on_triggers(workflow), vec!["pull_request", "push"]);
}

#[test]
fn ci_does_not_rerun_on_push_to_the_default_branch() {
    let triggers = workflow_triggers("ci.yml");
    assert!(
        !triggers.iter().any(|trigger| trigger == "push"),
        "ci.yml triggers {triggers:?} still include `push` — the gate would \
         re-run on every merge into the default branch, duplicating the PR run"
    );
}

#[test]
fn ci_still_gates_pull_requests_and_stays_dispatchable() {
    let triggers = workflow_triggers("ci.yml");
    for expected in ["pull_request", "workflow_dispatch"] {
        assert!(
            triggers.iter().any(|trigger| trigger == expected),
            "ci.yml triggers {triggers:?} lost `{expected}`"
        );
    }
}

#[test]
fn markdown_lint_does_not_rerun_on_push_to_the_default_branch() {
    let triggers = workflow_triggers("markdown-lint.yml");
    assert!(
        !triggers.iter().any(|trigger| trigger == "push"),
        "markdown-lint.yml triggers {triggers:?} still include `push` — the \
         gate would re-run on every merge into the default branch, duplicating \
         the PR run"
    );
}

#[test]
fn markdown_lint_still_gates_pull_requests_and_stays_dispatchable() {
    let triggers = workflow_triggers("markdown-lint.yml");
    for expected in ["pull_request", "workflow_dispatch"] {
        assert!(
            triggers.iter().any(|trigger| trigger == expected),
            "markdown-lint.yml triggers {triggers:?} lost `{expected}`"
        );
    }
}
