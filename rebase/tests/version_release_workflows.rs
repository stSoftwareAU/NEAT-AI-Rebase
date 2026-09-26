//! The version gate and the release job must fire on the events and the files
//! they are meant to watch (Issue #106).
//!
//! `version-increment.yml` bumps `rebase/Cargo.toml` on a pull request that
//! changes gated source; `release.yml` cuts the `v<semver>` tag once that bump
//! reaches `Develop`. Both are configuration, and a misconfigured trigger or
//! path list fails silently: the workflow simply never runs, a change merges
//! under an unchanged version, and the fleet keeps the stale binary. These
//! tests model GitHub's own filter rules and assert the committed workflows
//! against the events and paths the fleet really produces.
//!
//! The branch side of `version-increment.yml` lives in
//! `workflow_branch_filters.rs`. Rust integration tests are separate crates,
//! so the glob matcher and the YAML reader are local here, the way each of the
//! repository's other workflow test files carries its own.

use std::path::{Path, PathBuf};

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

fn workflow_text(file: &str) -> String {
    let path = repo_root().join(".github/workflows").join(file);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Does `pattern` match `candidate` under GitHub's `paths:`/`branches:` glob
/// rules? `*` matches any run of characters except `/`; `**` matches any run
/// including `/`. Everything else is literal.
fn glob_matches(pattern: &str, candidate: &str) -> bool {
    match pattern.find('*') {
        None => pattern == candidate,
        Some(star) => {
            let (literal, rest) = pattern.split_at(star);
            let Some(tail) = candidate.strip_prefix(literal) else {
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
                .any(|end| glob_matches(rest, &tail[end..]))
        }
    }
}

fn matches_any(patterns: &[String], candidate: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| glob_matches(pattern, candidate))
}

fn unquote(entry: &str) -> String {
    entry.trim().trim_matches(['"', '\'']).to_string()
}

/// The `on:` block, as `(trigger name, the lines nested under it)` pairs.
///
/// Reads the mapping keys nested one level under `on:`, skipping blank lines
/// and comments and stopping at the next top-level key. Panics when `on:` is
/// absent: a workflow with no trigger block is a real failure, not a vacuous
/// pass.
fn trigger_sections(workflow: &str) -> Vec<(String, Vec<&str>)> {
    let mut lines = workflow.lines().skip_while(|line| line.trim_end() != "on:");
    lines.next().expect("workflow declares an `on:` block");

    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    let mut trigger_indent = None;
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 {
            break; // next top-level key — the `on:` block is finished.
        }
        let depth = *trigger_indent.get_or_insert(indent);
        if indent == depth {
            if let Some((name, _)) = trimmed.split_once(':') {
                sections.push((name.to_string(), Vec::new()));
            }
            continue;
        }
        if let Some((_, block)) = sections.last_mut() {
            block.push(line);
        }
    }
    sections
}

/// The trigger names of a workflow's `on:` block.
fn trigger_names(workflow: &str) -> Vec<String> {
    trigger_sections(workflow)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The lines nested under `on: <trigger>:`, without the trigger line itself.
///
/// Panics when the trigger is absent: a workflow that does not declare the
/// trigger under test is a real failure, not a vacuous pass.
fn trigger_block<'a>(workflow: &'a str, trigger: &str) -> Vec<&'a str> {
    trigger_sections(workflow)
        .into_iter()
        .find(|(name, _)| name == trigger)
        .unwrap_or_else(|| panic!("workflow declares an `on: {trigger}:` trigger"))
        .1
}

/// The `<key>:` list inside a trigger block, in either the flow form
/// (`branches: [Develop]`) or the block form (`branches:` then `- Develop`).
///
/// Panics when the key is absent: a missing filter is a real failure.
fn trigger_list(block: &[&str], key: &str) -> Vec<String> {
    let prefix = format!("{key}:");
    let start = block
        .iter()
        .position(|line| line.trim_start().starts_with(&prefix))
        .unwrap_or_else(|| panic!("trigger declares a `{key}:` list"));

    let line = block[start];
    let trimmed = line.trim_start();
    let key_indent = line.len() - trimmed.len();
    let inline = trimmed[prefix.len()..].trim();
    if !inline.is_empty() {
        return inline
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(unquote)
            .filter(|entry| !entry.is_empty())
            .collect();
    }

    block[start + 1..]
        .iter()
        .take_while(|line| {
            let trimmed = line.trim_start();
            trimmed.is_empty() || (line.len() - trimmed.len()) > key_indent
        })
        .filter_map(|line| line.trim_start().strip_prefix("- ").map(unquote))
        .collect()
}

/// The `paths:` / `branches:` list of `on: <trigger>:` in a committed workflow.
fn workflow_list(file: &str, trigger: &str, key: &str) -> Vec<String> {
    let workflow = workflow_text(file);
    trigger_list(&trigger_block(&workflow, trigger), key)
}

#[test]
fn glob_rules_follow_github_path_matching() {
    assert!(glob_matches("rebase/Cargo.toml", "rebase/Cargo.toml"));
    assert!(!glob_matches("rebase/Cargo.toml", "rebase/Cargo.lock"));
    assert!(glob_matches("rebase/src/**", "rebase/src/lib.rs"));
    assert!(glob_matches("rebase/src/**", "rebase/src/nested/deep.rs"));
    // The single-segment `*` is the trap: it never crosses a `/`.
    assert!(!glob_matches("rebase/src/*", "rebase/src/nested/deep.rs"));
    assert!(!glob_matches("rebase/src/**", "rebase/tests/lib.rs"));
}

#[test]
fn trigger_block_isolates_one_trigger() {
    let workflow = concat!(
        "name: Version Increment\n",
        "\n",
        "on:\n",
        "  pull_request:\n",
        "    types: [opened]\n",
        "    branches:\n",
        "      - Develop\n",
        "  push:\n",
        "    branches: [main]\n",
        "\n",
        "jobs:\n",
        "  bump:\n",
        "    branches: [not-a-trigger-list]\n",
    );
    assert_eq!(trigger_names(workflow), vec!["pull_request", "push"]);
    assert_eq!(
        trigger_list(&trigger_block(workflow, "pull_request"), "branches"),
        vec!["Develop"]
    );
    assert_eq!(
        trigger_list(&trigger_block(workflow, "push"), "branches"),
        vec!["main"]
    );
}

#[test]
fn trigger_list_reads_a_commented_block_list() {
    let workflow = concat!(
        "on:\n",
        "  pull_request:\n",
        "    paths:\n",
        "      # a comment inside the list is not an entry\n",
        "      - \"rebase/src/**\"\n",
        "      - \"Cargo.lock\"\n",
        "    types: [opened]\n",
        "\n",
        "jobs:\n",
    );
    assert_eq!(
        trigger_list(&trigger_block(workflow, "pull_request"), "paths"),
        vec!["rebase/src/**", "Cargo.lock"]
    );
}

#[test]
fn version_increment_runs_on_pull_requests_not_pushes() {
    let triggers = trigger_names(&workflow_text("version-increment.yml"));
    assert!(
        triggers.iter().any(|trigger| trigger == "pull_request"),
        "version-increment.yml triggers {triggers:?} lost `pull_request` — the \
         bump would never reach the PR branch"
    );
    assert!(
        !triggers.iter().any(|trigger| trigger == "push"),
        "version-increment.yml triggers {triggers:?} include `push` — the gate \
         would try to bump the default branch after the merge"
    );
}

#[test]
fn version_increment_watches_every_gated_path() {
    let paths = workflow_list("version-increment.yml", "pull_request", "paths");
    for changed in [
        "rebase/src/main.rs",
        "rebase/src/lib.rs",
        "rebase/src/nested/module.rs",
        "rebase/Cargo.toml",
        "Cargo.lock",
        "scripts/auto-version.sh",
        // The copied NEAT-AI-core helpers (Issue #107): a PR that edits a copy
        // is the PR whose family-sync step overwrites it.
        "scripts/runlib.sh",
        "scripts/family-pins.sh",
        ".github/workflows/version-increment.yml",
        // The toolchain this gate installs moved into the shared action
        // (Issue #125); a pin change there still has to run the gate.
        ".github/actions/setup-rust/action.yml",
    ] {
        assert!(
            matches_any(&paths, changed),
            "version-increment.yml paths {paths:?} do not gate a change to {changed}"
        );
    }
}

#[test]
fn version_increment_ignores_changes_that_need_no_bump() {
    let paths = workflow_list("version-increment.yml", "pull_request", "paths");
    for unchanged in [
        "README.md",
        "docs/archive/pr-summaries/pr-summary-106.md",
        "rebase/tests/version_release_workflows.rs",
    ] {
        assert!(
            !matches_any(&paths, unchanged),
            "version-increment.yml paths {paths:?} would bump the version for {unchanged}"
        );
    }
}

#[test]
fn release_cuts_tags_on_pushes_to_develop() {
    let triggers = trigger_names(&workflow_text("release.yml"));
    assert!(
        triggers.iter().any(|trigger| trigger == "push"),
        "release.yml triggers {triggers:?} lost `push` — no bump would ever be \
         tagged"
    );
    let branches = workflow_list("release.yml", "push", "branches");
    assert!(
        matches_any(&branches, "Develop"),
        "release.yml push branches {branches:?} do not include Develop"
    );
    for elsewhere in ["milestone/rebase-v1", "issue-106-version-gate"] {
        assert!(
            !matches_any(&branches, elsewhere),
            "release.yml push branches {branches:?} would tag a release from {elsewhere}"
        );
    }
}

#[test]
fn release_fires_only_on_a_manifest_change() {
    let paths = workflow_list("release.yml", "push", "paths");
    assert!(
        matches_any(&paths, "rebase/Cargo.toml"),
        "release.yml push paths {paths:?} do not watch rebase/Cargo.toml — the \
         version bump is what marks a release"
    );
    for unrelated in ["rebase/src/lib.rs", "Cargo.lock", "README.md"] {
        assert!(
            !matches_any(&paths, unrelated),
            "release.yml push paths {paths:?} would cut a release for {unrelated}"
        );
    }
}
