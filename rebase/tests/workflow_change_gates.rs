//! PR workflows skip their expensive work on a docs-only change (Issue #123).
//!
//! Every `pull_request` workflow used to run its full job on every PR, so a
//! one-line README fix paid for a Rust build, a SAST scan, an SBOM and more.
//! Each gated workflow now opens with a small `changes` job
//! (`dorny/paths-filter`, SHA-pinned) and its expensive jobs run only when that
//! verdict is not a positive "nothing relevant changed".
//!
//! The gate is job-level on purpose. A workflow-level `paths:` filter means no
//! run at all, and a required check with no run blocks the merge forever; a job
//! skipped by its `if:` reports as passed. The gate also fails open: on a
//! schedule, a manual dispatch or a failed classification the verdict is empty,
//! and empty is not `'false'`, so the job still runs.
//!
//! These tests parse the committed workflows and hold that shape, and evaluate
//! the committed filters against sample PR file lists so a filter that would
//! skip a code change is caught here rather than on a real PR.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The PR checkers whose expensive jobs wait for the `changes` verdict.
const GATED: [&str; 7] = [
    "actionlint.yml",
    "cargo-audit.yml",
    "ci.yml",
    "dependency-review.yml",
    "markdown-lint.yml",
    "sbom.yml",
    "semgrep.yml",
];

/// Workflows deliberately left ungated: a secret can land in any file,
/// including a Markdown page, so the secrets scan runs on every PR.
const ALWAYS_ON: [&str; 1] = ["gitleaks.yml"];

/// The workflow whose gate is "did any Markdown change", not "did code change".
const MARKDOWN_WORKFLOW: &str = "markdown-lint.yml";

/// The id of the change-detection job every gated workflow declares.
const CHANGES_JOB: &str = "changes";

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
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

/// The committed workflows named in `names`, each asserted to exist and to
/// still trigger on `pull_request`, so no sweep below can pass vacuously.
fn named_workflows(names: &[&str]) -> Vec<(String, String)> {
    let all = workflows();
    names
        .iter()
        .map(|name| {
            let (file, body) = all
                .iter()
                .find(|(file, _)| file == name)
                .unwrap_or_else(|| panic!("{name} must be committed"));
            assert!(
                triggers_on_pull_request(body),
                "{name} no longer triggers on `pull_request`"
            );
            (file.clone(), body.clone())
        })
        .collect()
}

/// The gated PR checkers.
fn gated_workflows() -> Vec<(String, String)> {
    named_workflows(&GATED)
}

/// The keys nested under each trigger of the `on:` block, as
/// `(trigger, key)` pairs — e.g. `("pull_request", "branches")`.
///
/// Comments and blank lines are skipped and the block ends at the next
/// top-level key. Panics when there is no `on:` block.
fn trigger_keys(workflow: &str) -> Vec<(String, String)> {
    let mut lines = workflow.lines().skip_while(|line| line.trim_end() != "on:");
    if lines.next().is_none() {
        panic!("workflow declares an `on:` block");
    }
    let mut trigger_indent = None;
    let mut current: Option<String> = None;
    let mut keys = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent == 0 {
            break;
        }
        let depth = *trigger_indent.get_or_insert(indent);
        let Some((key, _)) = trimmed.split_once(':') else {
            continue;
        };
        if indent == depth {
            current = Some(key.to_string());
            keys.push((key.to_string(), String::new()));
        } else if let Some(trigger) = &current
            && !trimmed.starts_with('-')
        {
            keys.push((trigger.clone(), key.trim().to_string()));
        }
    }
    keys
}

/// Does the workflow trigger on `pull_request`?
fn triggers_on_pull_request(workflow: &str) -> bool {
    trigger_keys(workflow)
        .iter()
        .any(|(trigger, key)| trigger == "pull_request" && key.is_empty())
}

/// Does any trigger carry a `paths:` or `paths-ignore:` filter?
fn has_workflow_path_filter(workflow: &str) -> bool {
    trigger_keys(workflow)
        .iter()
        .any(|(_, key)| key == "paths" || key == "paths-ignore")
}

/// One entry under a workflow's top-level `jobs:` key.
#[derive(Debug, Default)]
struct Job {
    /// The job id, as written under `jobs:`.
    id: String,
    /// The job-level `if:` condition, absent when unconditional.
    condition: Option<String>,
    /// The job-level `needs:` value, as a list of job ids.
    needs: Vec<String>,
    /// The job-level `permissions:` block, scope to level.
    permissions: BTreeMap<String, String>,
    /// The job-level `outputs:` block, name to expression.
    outputs: BTreeMap<String, String>,
    /// Every line of the job below its id, verbatim.
    body: String,
}

/// Split a `needs:` value — `a` or `[a, b]` — into job ids.
fn needs_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|id| id.trim().trim_matches(['"', '\'']).to_string())
        .filter(|id| !id.is_empty())
        .collect()
}

/// Every job of a workflow, in file order.
///
/// Understands the block mapping form the workflows use: a job id at two
/// columns, its keys at four, and `permissions:` / `outputs:` maps at six.
fn jobs(workflow: &str) -> Vec<Job> {
    let mut jobs: Vec<Job> = Vec::new();
    let mut in_jobs = false;
    let mut map: Option<&str> = None;

    for line in workflow.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            in_jobs = trimmed == "jobs:";
            map = None;
            continue;
        }
        if !in_jobs {
            continue;
        }
        if indent == 2 {
            if let Some(id) = trimmed.strip_suffix(':') {
                jobs.push(Job {
                    id: id.to_string(),
                    ..Job::default()
                });
                map = None;
            }
            continue;
        }

        let Some(job) = jobs.last_mut() else {
            continue;
        };
        job.body.push_str(line);
        job.body.push('\n');

        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());

        if indent == 4 {
            map = None;
            match key {
                "if" => job.condition = Some(value.to_string()),
                "needs" => job.needs = needs_list(value),
                "permissions" if value.is_empty() => map = Some("permissions"),
                "outputs" if value.is_empty() => map = Some("outputs"),
                _ => {}
            }
        } else if indent == 6 {
            match map {
                Some("permissions") => {
                    job.permissions.insert(key.to_string(), value.to_string());
                }
                Some("outputs") => {
                    job.outputs.insert(key.to_string(), value.to_string());
                }
                _ => {}
            }
        }
    }

    jobs
}

/// The `filters: |` block of a `dorny/paths-filter` step, filter name to its
/// glob patterns, in file order.
fn filters(job_body: &str) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut header_indent = None;
    let mut current: Option<String> = None;
    for line in job_body.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        match header_indent {
            None => {
                if trimmed == "filters: |" {
                    header_indent = Some(indent);
                }
            }
            Some(header) => {
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if indent <= header {
                    break;
                }
                if let Some(entry) = trimmed.strip_prefix("- ") {
                    let filter = current
                        .as_ref()
                        .expect("a pattern sits under a filter name");
                    found
                        .get_mut(filter)
                        .expect("filter was recorded")
                        .push(entry.trim().trim_matches(['"', '\'']).to_string());
                } else if let Some(name) = trimmed.strip_suffix(':') {
                    current = Some(name.to_string());
                    found.insert(name.to_string(), Vec::new());
                }
            }
        }
    }
    found
}

/// The value of a `key:` line anywhere in a job body, or `None`.
fn setting<'a>(job_body: &'a str, key: &str) -> Option<&'a str> {
    job_body.lines().find_map(|line| {
        let trimmed = line.trim().trim_start_matches("- ");
        trimmed
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::trim)
    })
}

/// The fail-open condition a job gated on `filter` must carry.
fn gate_for(filter: &str) -> String {
    format!("${{{{ !cancelled() && needs.{CHANGES_JOB}.outputs.{filter} != 'false' }}}}")
}

/// Does `pattern` match `path` under `dorny/paths-filter`'s picomatch rules
/// (`dot: true`)? `*` matches within one segment; `**` matches across them;
/// a leading `**/` also matches zero directories.
fn glob(pattern: &str, path: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix("**/") {
        return glob(rest, path)
            || path
                .char_indices()
                .filter(|(_, ch)| *ch == '/')
                .any(|(index, _)| glob(rest, &path[index + 1..]));
    }
    match pattern.find('*') {
        None => pattern == path,
        Some(star) => {
            let (literal, rest) = pattern.split_at(star);
            let Some(tail) = path.strip_prefix(literal) else {
                return false;
            };
            let (crosses, rest) = match rest.strip_prefix("**") {
                Some(rest) => (true, rest),
                None => (false, &rest[1..]),
            };
            (0..=tail.len())
                .filter(|end| tail.is_char_boundary(*end))
                .filter(|end| crosses || !tail[..*end].contains('/'))
                .any(|end| glob(rest, &tail[end..]))
        }
    }
}

/// Would a filter report `'true'` for a PR that changed `files`?
///
/// Mirrors `dorny/paths-filter`: a `!` pattern holds when the glob does not
/// match; under `every` a file counts when all patterns hold, under `some`
/// (the default) when any does; the filter is true when any file counts.
fn filter_fires(patterns: &[String], every: bool, files: &[&str]) -> bool {
    let holds = |pattern: &String, file: &str| match pattern.strip_prefix('!') {
        Some(negated) => !glob(negated, file),
        None => glob(pattern, file),
    };
    files.iter().any(|file| {
        if every {
            patterns.iter().all(|pattern| holds(pattern, file))
        } else {
            patterns.iter().any(|pattern| holds(pattern, file))
        }
    })
}

/// A committed workflow's jobs, by file name.
fn committed_jobs(name: &str) -> Vec<Job> {
    let (_, body) = workflows()
        .into_iter()
        .find(|(file, _)| file == name)
        .unwrap_or_else(|| panic!("{name} must be committed"));
    jobs(&body)
}

/// The `changes` job of a committed workflow, and its filters.
fn committed_filters(name: &str) -> (Job, BTreeMap<String, Vec<String>>) {
    let changes = committed_jobs(name)
        .into_iter()
        .find(|job| job.id == CHANGES_JOB)
        .unwrap_or_else(|| panic!("{name} declares a `{CHANGES_JOB}` job"));
    let found = filters(&changes.body);
    (changes, found)
}

// ---------------------------------------------------------------------------
// Parser checks on inline YAML, so the sweeps below cannot pass vacuously.
// ---------------------------------------------------------------------------

#[test]
fn trigger_keys_spot_a_workflow_level_path_filter() {
    let filtered = concat!(
        "on:\n",
        "  pull_request:\n",
        "    branches: [\"*\"]\n",
        "    # paths: would go here\n",
        "    paths-ignore:\n",
        "      - 'docs/**'\n",
        "  workflow_dispatch:\n",
        "\n",
        "jobs:\n",
    );
    assert!(triggers_on_pull_request(filtered));
    assert!(has_workflow_path_filter(filtered));

    let open = concat!(
        "on:\n",
        "  schedule:\n",
        "    - cron: \"0 6 * * 1\"\n",
        "  workflow_dispatch:\n",
        "\n",
        "jobs:\n",
        "  build:\n",
        "    paths: not-a-trigger-key\n",
    );
    assert!(!triggers_on_pull_request(open));
    assert!(!has_workflow_path_filter(open));
}

#[test]
fn jobs_read_needs_outputs_and_the_filter_block() {
    let workflow = concat!(
        "jobs:\n",
        "  changes:\n",
        "    permissions:\n",
        "      pull-requests: read\n",
        "    outputs:\n",
        "      code: ${{ steps.filter.outputs.code }}\n",
        "    steps:\n",
        "      - id: filter\n",
        "        uses: dorny/paths-filter@0123456789abcdef0123456789abcdef01234567 # v4\n",
        "        with:\n",
        "          predicate-quantifier: every\n",
        "          filters: |\n",
        "            code:\n",
        "              - '**'\n",
        "              - '!docs/**'\n",
        "\n",
        "  build:\n",
        "    needs: changes\n",
        "    if: ${{ !cancelled() && needs.changes.outputs.code != 'false' }}\n",
        "  report:\n",
        "    needs: [build, \"lint\"]\n",
    );
    let parsed = jobs(workflow);
    assert_eq!(parsed.len(), 3);

    let changes = &parsed[0];
    assert_eq!(
        changes.permissions.get("pull-requests").map(String::as_str),
        Some("read")
    );
    assert_eq!(
        changes.outputs.get("code").map(String::as_str),
        Some("${{ steps.filter.outputs.code }}")
    );
    assert_eq!(
        setting(&changes.body, "predicate-quantifier"),
        Some("every")
    );
    assert_eq!(setting(&changes.body, "id"), Some("filter"));
    assert_eq!(
        filters(&changes.body),
        BTreeMap::from([(
            "code".to_string(),
            vec!["**".to_string(), "!docs/**".to_string()]
        )])
    );

    assert_eq!(parsed[1].needs, vec!["changes"]);
    assert_eq!(
        parsed[1].condition.as_deref(),
        Some(gate_for("code").as_str())
    );
    assert_eq!(parsed[2].needs, vec!["build", "lint"]);
}

#[test]
fn filter_evaluation_follows_paths_filter_semantics() {
    let code: Vec<String> = ["**", "!docs/**", "!**/*.md"].map(String::from).to_vec();
    assert!(filter_fires(&code, true, &["rebase/src/lib.rs"]));
    assert!(filter_fires(&code, true, &[".github/workflows/ci.yml"]));
    assert!(filter_fires(&code, true, &["README.md", "Cargo.toml"]));
    assert!(!filter_fires(&code, true, &["README.md", "docs/a/b.png"]));
    assert!(!filter_fires(&code, true, &["rebase/src/notes.md"]));
    assert!(!filter_fires(&code, true, &[]));

    // The same patterns under `some` would fire on everything — which is why
    // the quantifier is asserted, not assumed.
    assert!(filter_fires(&code, false, &["README.md"]));

    let markdown: Vec<String> = ["**/*.md"].map(String::from).to_vec();
    assert!(filter_fires(&markdown, false, &["README.md"]));
    assert!(filter_fires(&markdown, false, &["docs/deep/page.md"]));
    assert!(!filter_fires(&markdown, false, &["rebase/src/lib.rs"]));
}

// ---------------------------------------------------------------------------
// The committed workflows.
// ---------------------------------------------------------------------------

#[test]
fn no_checker_workflow_uses_a_workflow_level_path_filter() {
    let checkers: Vec<&str> = GATED.iter().chain(ALWAYS_ON.iter()).copied().collect();
    for (name, body) in named_workflows(&checkers) {
        assert!(
            !has_workflow_path_filter(&body),
            "{name} filters its trigger by path — a skipped workflow never reports, so a \
             required check waits forever; gate jobs on the `{CHANGES_JOB}` output instead"
        );
    }
}

#[test]
fn every_gated_workflow_classifies_changes_with_a_pinned_read_only_filter() {
    for (name, body) in gated_workflows() {
        let parsed = jobs(&body);
        let changes = parsed
            .iter()
            .find(|job| job.id == CHANGES_JOB)
            .unwrap_or_else(|| panic!("{name} declares no `{CHANGES_JOB}` job"));

        let uses = setting(&changes.body, "uses")
            .unwrap_or_else(|| panic!("{name}: `{CHANGES_JOB}` runs no action"));
        let reference = uses
            .split_whitespace()
            .next()
            .and_then(|action| action.strip_prefix("dorny/paths-filter@"))
            .unwrap_or_else(|| panic!("{name}: `{CHANGES_JOB}` must use dorny/paths-filter"));
        assert!(
            reference.len() == 40 && reference.chars().all(|ch| ch.is_ascii_hexdigit()),
            "{name}: dorny/paths-filter must be pinned to a 40-character SHA, found {reference}"
        );

        assert_eq!(
            changes.permissions,
            BTreeMap::from([("pull-requests".to_string(), "read".to_string())]),
            "{name}: listing a PR's files needs `pull-requests: read` and nothing more"
        );
        assert!(
            changes.needs.is_empty() && changes.condition.is_none(),
            "{name}: `{CHANGES_JOB}` must always run — it is the verdict every job waits on"
        );
        assert_eq!(
            setting(&changes.body, "if"),
            Some("github.event_name == 'pull_request'"),
            "{name}: the filter step runs on PRs only, so a schedule or dispatch leaves the \
             verdict empty and the gated jobs run"
        );

        let declared = filters(&changes.body);
        assert!(
            !declared.is_empty(),
            "{name}: the filter declares no filters"
        );
        for filter in declared.keys() {
            assert_eq!(
                changes.outputs.get(filter).map(String::as_str),
                Some(format!("${{{{ steps.filter.outputs.{filter} }}}}").as_str()),
                "{name}: filter `{filter}` must be exposed as a job output"
            );
        }
    }
}

#[test]
fn every_job_waits_for_the_verdict_and_fails_open() {
    for (name, body) in gated_workflows() {
        let parsed = jobs(&body);
        let ids: Vec<&str> = parsed.iter().map(|job| job.id.as_str()).collect();
        let (_, declared) = committed_filters(&name);

        let mut directly_gated = 0;
        for job in parsed.iter().filter(|job| job.id != CHANGES_JOB) {
            assert!(
                !job.needs.is_empty(),
                "{name}: job `{}` does not wait for `{CHANGES_JOB}`, so it runs on a docs-only PR",
                job.id
            );
            if job.needs.iter().any(|need| need == CHANGES_JOB) {
                directly_gated += 1;
                let condition = job.condition.as_deref().unwrap_or_default();
                assert!(
                    declared.keys().any(|filter| condition == gate_for(filter)),
                    "{name}: job `{}` must carry the fail-open gate `{}` on a declared filter, \
                     found `{condition}`",
                    job.id,
                    gate_for("<filter>")
                );
            } else {
                for need in &job.needs {
                    assert!(
                        ids.contains(&need.as_str()) && need != &job.id,
                        "{name}: job `{}` needs unknown job `{need}`",
                        job.id
                    );
                }
            }
        }
        assert!(
            directly_gated > 0,
            "{name}: no job is gated on the `{CHANGES_JOB}` verdict"
        );
    }
}

#[test]
fn code_gates_skip_docs_only_changes_and_nothing_else() {
    for (name, _) in gated_workflows() {
        if name == MARKDOWN_WORKFLOW {
            continue;
        }
        let (changes, declared) = committed_filters(&name);
        assert_eq!(
            declared.get("code"),
            Some(&vec![
                "**".to_string(),
                "!docs/**".to_string(),
                "!**/*.md".to_string()
            ]),
            "{name}: the `code` filter is broad on purpose — anything outside `docs/**` \
             and `*.md` counts"
        );
        assert_eq!(
            setting(&changes.body, "predicate-quantifier"),
            Some("every"),
            "{name}: without `every` the negations are ignored and the filter always fires"
        );

        let code = &declared["code"];
        for files in [
            &["rebase/src/lib.rs"][..],
            &["Cargo.lock"],
            &[".github/workflows/ci.yml"],
            &["scripts/actionlint.sh", "README.md"],
            &["deny.toml"],
        ] {
            assert!(
                filter_fires(code, true, files),
                "{name}: a PR changing {files:?} must run the gated jobs"
            );
        }
        for files in [
            &["README.md"][..],
            &[
                "docs/archive/pr-summaries/pr-summary-1.md",
                "CONTRIBUTING.md",
            ],
            &["docs/evidence/screenshot.png"],
        ] {
            assert!(
                !filter_fires(code, true, files),
                "{name}: a docs-only PR changing {files:?} should skip the gated jobs"
            );
        }
    }
}

#[test]
fn markdown_lint_runs_when_any_markdown_or_its_config_changes() {
    let (changes, declared) = committed_filters(MARKDOWN_WORKFLOW);
    let markdown = declared
        .get("markdown")
        .unwrap_or_else(|| panic!("{MARKDOWN_WORKFLOW} declares a `markdown` filter"));
    assert!(
        setting(&changes.body, "predicate-quantifier").is_none_or(|value| value == "some"),
        "{MARKDOWN_WORKFLOW}: the markdown filter lists alternatives, so it needs `some`"
    );

    for files in [
        &["README.md"][..],
        &["docs/archive/pr-summaries/pr-summary-1.md"],
        &[".markdownlint-cli2.jsonc"],
        &[".github/workflows/markdown-lint.yml"],
    ] {
        assert!(
            filter_fires(markdown, false, files),
            "{MARKDOWN_WORKFLOW}: a PR changing {files:?} must lint"
        );
    }
    assert!(
        !filter_fires(markdown, false, &["rebase/src/lib.rs", "Cargo.toml"]),
        "{MARKDOWN_WORKFLOW}: a code-only PR has no Markdown to lint"
    );
}

#[test]
fn the_secrets_scan_stays_on_for_every_pull_request() {
    for (name, body) in named_workflows(&ALWAYS_ON) {
        let parsed = jobs(&body);
        assert!(
            parsed.iter().all(|job| job.id != CHANGES_JOB),
            "{name} is always-on: a secret can be committed in a docs or Markdown file"
        );
        for job in &parsed {
            assert!(
                job.condition.is_none() && job.needs.is_empty(),
                "{name}: job `{}` must run unconditionally on every PR",
                job.id
            );
        }
    }
}
