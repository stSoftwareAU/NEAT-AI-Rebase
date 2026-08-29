//! The CI gates must actually fire on the PRs they are supposed to gate
//! (Issues #59, #60, #62).
//!
//! Milestone sub-issue PRs target a shared `milestone/<slug>` branch rather
//! than `Develop`, and GitHub's branch filter glob `*` stops at a `/` — so a
//! `branches: ["*"]` filter silently matches nothing with a slash in it and
//! every sub-issue PR merges into the milestone branch ungated. These tests
//! model GitHub's own matching rules and assert the committed workflow's
//! filter against the branch names the fleet really uses.

use std::path::{Path, PathBuf};

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

/// Does `pattern` match `branch` under GitHub's workflow filter rules?
///
/// `*` matches any run of characters except `/`; `**` matches any run
/// including `/`. Everything else is literal. That single-segment `*` is the
/// whole bug: `*` never matches `milestone/rebase-v1`.
fn filter_matches(pattern: &str, branch: &str) -> bool {
    match pattern.find('*') {
        None => pattern == branch,
        Some(star) => {
            let (literal, rest) = pattern.split_at(star);
            let Some(tail) = branch.strip_prefix(literal) else {
                return false;
            };
            let (crosses_slash, rest) = match rest.strip_prefix("**") {
                Some(rest) => (true, rest),
                None => (false, &rest[1..]),
            };
            // Try every split the wildcard could consume, shortest first.
            (0..=tail.len())
                .filter(|end| tail.is_char_boundary(*end))
                .filter(|end| crosses_slash || !tail[..*end].contains('/'))
                .any(|end| filter_matches(rest, &tail[end..]))
        }
    }
}

/// The `branches:` list under `on: pull_request:` of a workflow file.
///
/// Accepts both the flow form (`branches: ["*", "milestone/*"]`) and the block
/// form (`branches:` followed by `- Develop`). Panics rather than returning an
/// empty list when the key is absent: a missing filter is a real failure, not a
/// vacuous pass.
fn pull_request_branches(workflow: &str) -> Vec<String> {
    let mut lines = workflow.lines();
    let branches = lines
        .find_map(|line| {
            let trimmed = line.trim_start();
            trimmed.strip_prefix("branches:").map(str::trim)
        })
        .expect("workflow declares a pull_request branches filter");

    if !branches.is_empty() {
        return branches
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(unquote)
            .filter(|entry| !entry.is_empty())
            .collect();
    }

    lines
        .map_while(|line| line.trim_start().strip_prefix("- ").map(unquote))
        .collect()
}

fn unquote(entry: &str) -> String {
    entry.trim().trim_matches(['"', '\'']).to_string()
}

/// The committed `pull_request` branch filter of `.github/workflows/<file>`.
fn workflow_filter(file: &str) -> Vec<String> {
    let path = repo_root().join(".github/workflows").join(file);
    let workflow = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    pull_request_branches(&workflow)
}

fn matches_any(patterns: &[String], branch: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| filter_matches(pattern, branch))
}

#[test]
fn single_star_does_not_cross_a_slash() {
    assert!(filter_matches("*", "Develop"));
    assert!(!filter_matches("*", "milestone/rebase-v1"));
    assert!(filter_matches("milestone/*", "milestone/rebase-v1"));
    assert!(!filter_matches("milestone/*", "milestone/nested/slug"));
    assert!(filter_matches("**", "milestone/nested/slug"));
    assert!(!filter_matches("milestone/*", "Develop"));
    assert!(!filter_matches("Develop", "Develop-2"));
}

#[test]
fn cargo_audit_gates_milestone_pull_requests() {
    let filter = workflow_filter("cargo-audit.yml");
    for branch in ["milestone/rebase-v1", "milestone/producer-wiring"] {
        assert!(
            matches_any(&filter, branch),
            "cargo-audit.yml filter {filter:?} does not gate PRs into {branch}"
        );
    }
}

#[test]
fn cargo_audit_still_gates_unnested_branches() {
    let filter = workflow_filter("cargo-audit.yml");
    for branch in ["Develop", "main", "issue-59-fix"] {
        assert!(
            matches_any(&filter, branch),
            "cargo-audit.yml filter {filter:?} stopped gating PRs into {branch}"
        );
    }
}

#[test]
fn ci_gates_milestone_pull_requests() {
    let filter = workflow_filter("ci.yml");
    for branch in ["milestone/rebase-v1", "milestone/producer-wiring"] {
        assert!(
            matches_any(&filter, branch),
            "ci.yml filter {filter:?} does not gate PRs into {branch}"
        );
    }
}

#[test]
fn ci_still_gates_develop() {
    let filter = workflow_filter("ci.yml");
    assert!(
        matches_any(&filter, "Develop"),
        "ci.yml filter {filter:?} stopped gating PRs into Develop"
    );
}

#[test]
fn markdown_lint_gates_milestone_pull_requests() {
    let filter = workflow_filter("markdown-lint.yml");
    for branch in ["milestone/rebase-v1", "milestone/producer-wiring"] {
        assert!(
            matches_any(&filter, branch),
            "markdown-lint.yml filter {filter:?} does not gate PRs into {branch}"
        );
    }
}

#[test]
fn markdown_lint_still_gates_unnested_branches() {
    let filter = workflow_filter("markdown-lint.yml");
    for branch in ["Develop", "main", "issue-62-fix"] {
        assert!(
            matches_any(&filter, branch),
            "markdown-lint.yml filter {filter:?} stopped gating PRs into {branch}"
        );
    }
}
