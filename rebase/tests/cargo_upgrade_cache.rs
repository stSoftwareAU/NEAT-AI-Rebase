//! The weekly Cargo bump must not build the whole dependency tree cold
//! (Issue #124).
//!
//! `cargo-upgrade.yml` runs `cargo deny check` and `cargo test --workspace
//! --all-features` whenever the bump moves the lockfile. Without a cache every
//! such run re-downloads every crate, stretching a job that has a 30-minute
//! ceiling. These tests read the committed workflow's `upgrade` job and assert
//! it restores the same Cargo cache `ci.yml` does, before the verification
//! step, and only on runs that have something to verify.
//!
//! Rust integration tests are separate crates, so the small YAML step reader
//! is local here, the way each of the repository's other workflow test files
//! carries its own.

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

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The steps of `job`, each as its own lines with comments and blank lines
/// dropped. Panics when the job or its `steps:` block is absent: a missing job
/// is a real failure, not a vacuous pass.
fn job_steps(workflow: &str, job: &str) -> Vec<Vec<String>> {
    let header = format!("{job}:");
    let mut lines = workflow
        .lines()
        .skip_while(|line| !(indent_of(line) == 2 && line.trim() == header));
    lines
        .next()
        .unwrap_or_else(|| panic!("workflow declares a `{job}` job"));

    let mut lines = lines.skip_while(|line| line.trim() != "steps:");
    let steps_line = lines
        .next()
        .unwrap_or_else(|| panic!("`{job}` job declares `steps:`"));
    let steps_indent = indent_of(steps_line);

    let mut steps: Vec<Vec<String>> = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = indent_of(line);
        if indent <= steps_indent && !trimmed.starts_with("- ") {
            break; // the next job-level key or the next job
        }
        if trimmed.starts_with("- ") && indent <= steps_indent + 2 {
            steps.push(vec![trimmed.trim_start_matches("- ").to_string()]);
        } else if let Some(step) = steps.last_mut() {
            step.push(trimmed.to_string());
        }
    }
    assert!(!steps.is_empty(), "`{job}` job has at least one step");
    steps
}

/// The value of `key:` on a step's own line, with any trailing comment
/// dropped.
fn step_value(step: &[String], key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    step.iter().find_map(|line| {
        let value = line.strip_prefix(&prefix)?;
        let value = value.split(" #").next().unwrap_or(value);
        Some(value.trim().to_string())
    })
}

/// The entries of a block scalar (`key: |`) — the lines after `key:` up to the
/// next `key:` line in the same step.
fn block_entries(step: &[String], key: &str) -> Vec<String> {
    let prefix = format!("{key}:");
    step.iter()
        .skip_while(|line| !line.starts_with(&prefix))
        .skip(1)
        .take_while(|line| !is_key_line(line))
        .cloned()
        .collect()
}

/// Is this a `name:`/`with:`-style mapping key rather than a block entry?
fn is_key_line(line: &str) -> bool {
    line.split_once(':').is_some_and(|(key, _)| {
        !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

fn step_position(steps: &[Vec<String>], name: &str) -> usize {
    steps
        .iter()
        .position(|step| step_value(step, "name").as_deref() == Some(name))
        .unwrap_or_else(|| panic!("step `{name}` exists"))
}

/// The `upgrade` job's `actions/cache` step and its position.
fn upgrade_cache_step(steps: &[Vec<String>]) -> (usize, &Vec<String>) {
    steps
        .iter()
        .enumerate()
        .find(|(_, step)| {
            step_value(step, "uses").is_some_and(|uses| uses.starts_with("actions/cache@"))
        })
        .expect("`upgrade` job restores a Cargo cache with actions/cache")
}

/// The `ci.yml` cache step — the reference this job must match.
fn ci_cache_step() -> Vec<String> {
    job_steps(&workflow_text("ci.yml"), "quality")
        .into_iter()
        .find(|step| {
            step_value(step, "uses").is_some_and(|uses| uses.starts_with("actions/cache@"))
        })
        .expect("ci.yml restores a Cargo cache")
}

#[test]
fn the_upgrade_job_caches_the_cargo_registry_and_git_checkouts() {
    let steps = job_steps(&workflow_text("cargo-upgrade.yml"), "upgrade");
    let (_, cache) = upgrade_cache_step(&steps);

    let paths = block_entries(cache, "path");
    for expected in ["~/.cargo/registry", "~/.cargo/git"] {
        assert!(
            paths.iter().any(|path| path == expected),
            "cache path includes {expected}, got {paths:?}"
        );
    }
}

#[test]
fn the_cache_key_tracks_the_lockfile_and_manifests_with_a_fallback() {
    let steps = job_steps(&workflow_text("cargo-upgrade.yml"), "upgrade");
    let (_, cache) = upgrade_cache_step(&steps);

    let key = step_value(cache, "key").expect("cache step sets `key:`");
    assert_eq!(
        key, "${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock', '**/Cargo.toml') }}",
        "key hashes every lockfile and manifest"
    );
    assert_eq!(
        block_entries(cache, "restore-keys"),
        vec!["${{ runner.os }}-cargo-".to_string()],
        "a changed lockfile still restores the nearest earlier cache"
    );
}

#[test]
fn the_cache_matches_ci_so_the_two_workflows_share_one_cache() {
    let steps = job_steps(&workflow_text("cargo-upgrade.yml"), "upgrade");
    let (_, cache) = upgrade_cache_step(&steps);
    let ci = ci_cache_step();

    for key in ["uses", "key"] {
        assert_eq!(
            step_value(cache, key),
            step_value(&ci, key),
            "`{key}:` matches ci.yml"
        );
    }
    for key in ["path", "restore-keys"] {
        assert_eq!(
            block_entries(cache, key),
            block_entries(&ci, key),
            "`{key}:` matches ci.yml"
        );
    }
}

#[test]
fn the_cache_action_is_pinned_to_a_full_commit_sha() {
    let steps = job_steps(&workflow_text("cargo-upgrade.yml"), "upgrade");
    let (_, cache) = upgrade_cache_step(&steps);

    let uses = step_value(cache, "uses").expect("cache step sets `uses:`");
    let sha = uses
        .strip_prefix("actions/cache@")
        .expect("uses actions/cache");
    assert!(
        sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "actions/cache is pinned to a 40-character commit SHA, got {sha:?}"
    );
}

#[test]
fn the_cache_is_restored_after_the_bump_and_before_verification() {
    let steps = job_steps(&workflow_text("cargo-upgrade.yml"), "upgrade");
    let (cache, _) = upgrade_cache_step(&steps);

    // After the bump, so the key hashes the lockfile the suite actually builds.
    let bump = step_position(
        &steps,
        "Upgrade compatible requirements and refresh the lockfile",
    );
    let verify = step_position(&steps, "Verify the upgrade");
    assert!(bump < cache, "cache is restored after the lockfile refresh");
    assert!(
        cache < verify,
        "cache is restored before `Verify the upgrade`"
    );
}

#[test]
fn the_cache_is_skipped_when_the_bump_changed_nothing() {
    let steps = job_steps(&workflow_text("cargo-upgrade.yml"), "upgrade");
    let (_, cache) = upgrade_cache_step(&steps);

    assert_eq!(
        step_value(cache, "if").as_deref(),
        Some("steps.changes.outputs.changed == 'true'"),
        "a no-op run neither restores nor saves the cache"
    );
}
