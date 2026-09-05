//! Every PR-triggered checker workflow cancels its own superseded runs
//! (Issue #85).
//!
//! A `synchronize` push re-triggers each `pull_request` workflow. Without a
//! `concurrency:` group keyed by the ref, and `cancel-in-progress: true`, the
//! run started by the previous push keeps executing to completion — CI minutes
//! burnt on a result nobody wants, and a stale check racing the latest push.
//! These tests read the committed workflows and assert the guard is there.

use std::path::{Path, PathBuf};

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

/// A workflow's top-level `concurrency:` settings.
#[derive(Debug, PartialEq, Eq)]
struct Concurrency {
    /// The `group:` expression, verbatim.
    group: String,
    /// The `cancel-in-progress:` value, absent when the key is not declared.
    cancel_in_progress: Option<bool>,
}

/// Read the top-level `concurrency:` block, or `None` when there is none.
///
/// Only the two keys nested directly under it are read; comments, blank lines
/// and the shorthand `concurrency: <group>` form are handled, and the block
/// ends at the next top-level key.
fn concurrency(workflow: &str) -> Option<Concurrency> {
    let mut lines = workflow.lines().skip_while(|line| {
        let trimmed = line.trim_end();
        trimmed != "concurrency:" && !trimmed.starts_with("concurrency: ")
    });
    let header = lines.next()?;

    // Shorthand: `concurrency: some-group` — a group with no cancel setting.
    if let Some(group) = header.trim_end().strip_prefix("concurrency: ") {
        return Some(Concurrency {
            group: group.trim().to_string(),
            cancel_in_progress: None,
        });
    }

    let mut group = None;
    let mut cancel_in_progress = None;
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.len() == trimmed.len() {
            break; // next top-level key — the block is finished.
        }
        match trimmed.split_once(':') {
            Some(("group", value)) => group = Some(value.trim().to_string()),
            Some(("cancel-in-progress", value)) => {
                cancel_in_progress = Some(value.trim() == "true");
            }
            _ => {}
        }
    }

    Some(Concurrency {
        group: group.expect("a `concurrency:` block declares a `group:`"),
        cancel_in_progress,
    })
}

/// Does the workflow trigger on `pull_request`?
///
/// Matches the trigger key nested one level under `on:`, so a `pull_request`
/// mentioned in a comment or a `run:` block is never counted.
fn triggers_on_pull_request(workflow: &str) -> bool {
    let mut lines = workflow.lines().skip_while(|line| line.trim_end() != "on:");
    if lines.next().is_none() {
        panic!("workflow declares an `on:` block");
    }
    let mut nested_indent = None;
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 {
            break;
        }
        let depth = *nested_indent.get_or_insert(indent);
        if indent == depth && trimmed.split_once(':').map(|(name, _)| name) == Some("pull_request")
        {
            return true;
        }
    }
    false
}

/// Every committed workflow, as `(file name, contents)`, sorted by name.
fn workflows() -> Vec<(String, String)> {
    let dir = repo_root().join(".github/workflows");
    let mut found: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yml"))
        .map(|path| {
            let name = path
                .file_name()
                .expect("workflow path has a file name")
                .to_string_lossy()
                .to_string();
            let body = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            (name, body)
        })
        .collect();
    found.sort();
    assert!(
        !found.is_empty(),
        "no workflows found under {}",
        dir.display()
    );
    found
}

#[test]
fn concurrency_reads_the_group_and_cancel_keys() {
    let workflow = concat!(
        "name: CI\n",
        "\n",
        "concurrency:\n",
        "  # keyed by ref so each PR cancels only its own runs\n",
        "  group: ci-${{ github.ref }}\n",
        "  cancel-in-progress: true\n",
        "\n",
        "jobs:\n",
        "  build:\n",
    );
    assert_eq!(
        concurrency(workflow),
        Some(Concurrency {
            group: "ci-${{ github.ref }}".to_string(),
            cancel_in_progress: Some(true),
        })
    );
}

#[test]
fn concurrency_is_absent_when_the_block_is_not_declared() {
    let workflow = concat!(
        "name: CI\n",
        "\n",
        "on:\n",
        "  pull_request:\n",
        "\n",
        "jobs:\n"
    );
    assert_eq!(concurrency(workflow), None);
}

#[test]
fn concurrency_reads_a_block_that_does_not_cancel() {
    let workflow = concat!(
        "concurrency:\n",
        "  group: cargo-upgrade\n",
        "  cancel-in-progress: false\n",
        "\n",
        "jobs:\n",
    );
    assert_eq!(
        concurrency(workflow),
        Some(Concurrency {
            group: "cargo-upgrade".to_string(),
            cancel_in_progress: Some(false),
        })
    );
}

#[test]
fn triggers_on_pull_request_ignores_comments_and_other_triggers() {
    let scheduled = concat!(
        "on:\n",
        "  # not a pull_request trigger, just prose about one\n",
        "  schedule:\n",
        "    - cron: \"0 6 * * 1\"\n",
        "  workflow_dispatch:\n",
        "\n",
        "jobs:\n",
    );
    assert!(!triggers_on_pull_request(scheduled));

    let gated = concat!(
        "on:\n",
        "  pull_request:\n",
        "    branches: [\"*\", \"milestone/*\"]\n",
        "  workflow_dispatch:\n",
    );
    assert!(triggers_on_pull_request(gated));
}

#[test]
fn every_pull_request_workflow_cancels_superseded_runs() {
    for (name, body) in workflows() {
        if !triggers_on_pull_request(&body) {
            continue;
        }
        let Some(settings) = concurrency(&body) else {
            panic!(
                "{name} triggers on `pull_request` but declares no `concurrency:` block — a \
                 superseded run keeps burning CI minutes after the next push"
            );
        };
        assert!(
            settings.group.contains("${{ github.ref }}"),
            "{name} concurrency group `{}` is not keyed by `${{{{ github.ref }}}}` — one PR \
             would cancel another's runs",
            settings.group
        );
        assert_eq!(
            settings.cancel_in_progress,
            Some(true),
            "{name} does not set `cancel-in-progress: true` — the superseded run is queued \
             rather than cancelled"
        );
    }
}

#[test]
fn the_scan_workflows_from_issue_85_are_covered() {
    // The three the issue names, asserted by name so the sweep above can never
    // pass vacuously if one of them stops being read.
    for name in ["dependency-review.yml", "gitleaks.yml", "semgrep.yml"] {
        let body = std::fs::read_to_string(repo_root().join(".github/workflows").join(name))
            .unwrap_or_else(|error| panic!("read {name}: {error}"));
        assert!(
            triggers_on_pull_request(&body),
            "{name} no longer triggers on `pull_request`"
        );
        let settings =
            concurrency(&body).unwrap_or_else(|| panic!("{name} declares no `concurrency:` block"));
        assert_eq!(settings.cancel_in_progress, Some(true), "{name}");
        assert!(settings.group.contains("${{ github.ref }}"), "{name}");
    }
}
