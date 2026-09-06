//! A failed security job has to reach a human (Issue #94).
//!
//! `.github/workflows/cargo-audit.yml` runs unattended at 06:00 UTC every
//! Monday: no pull request, no author, nobody watching. When an advisory lands
//! against an already-locked dependency that run goes red and — without a
//! routing step — the only trace is the Actions tab, which no one is
//! guaranteed to open. The PR-triggered security jobs are different: a red
//! check blocks the merge in front of the author, so the schedule is the path
//! that needs a notification of its own.
//!
//! These tests read the committed workflow and `SECURITY.md` and assert both
//! halves: the failure opens or updates a GitHub issue, and the tree documents
//! who triages it. The notifying job is also held to least privilege — the
//! audit job itself must stay read-only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

/// A committed file, read from the working tree.
///
/// Panics when the file is absent: a missing workflow or policy is the failure
/// this suite exists to catch, never a skip.
fn committed(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be committed: {error}", path.display()))
}

/// One entry under a workflow's top-level `jobs:` key.
#[derive(Debug, Default)]
struct Job {
    /// The job id, as written under `jobs:`.
    id: String,
    /// The job-level `if:` condition, absent when unconditional.
    condition: Option<String>,
    /// The job-level `needs:` value, verbatim.
    needs: Option<String>,
    /// The job-level `permissions:` block, scope to level.
    permissions: BTreeMap<String, String>,
    /// Every line of the job below its id, verbatim.
    body: String,
}

impl Job {
    /// Does this job run *because* another job failed?
    fn routes_a_failure(&self) -> bool {
        self.needs.is_some()
            && self
                .condition
                .as_deref()
                .is_some_and(|condition| condition.contains("failure()"))
    }
}

/// Every job of a workflow, in file order.
///
/// Only the block mapping form GitHub's own documentation uses is understood:
/// a job id at two columns, its keys at four, and a `permissions:` map at six.
/// Comments and blank lines are ignored, and the whole job body is kept so a
/// test can assert on the commands a step runs.
fn jobs(workflow: &str) -> Vec<Job> {
    let mut jobs: Vec<Job> = Vec::new();
    let mut in_jobs = false;
    let mut in_permissions = false;

    for line in workflow.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            // A new top-level key ends whatever block was being read.
            in_jobs = trimmed == "jobs:";
            in_permissions = false;
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
                in_permissions = false;
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
            in_permissions = false;
            match key {
                "if" => job.condition = Some(value.to_string()),
                "needs" => job.needs = Some(value.to_string()),
                "permissions" if value.is_empty() => in_permissions = true,
                _ => {}
            }
        } else if in_permissions && indent == 6 {
            job.permissions.insert(key.to_string(), value.to_string());
        }
    }

    jobs
}

/// The single job of a workflow whose id matches, or `None` when absent.
fn job<'a>(jobs: &'a [Job], id: &str) -> Option<&'a Job> {
    jobs.iter().find(|job| job.id == id)
}

#[test]
fn a_failed_cargo_audit_is_routed_to_a_notification_job() {
    let jobs = jobs(&committed(".github/workflows/cargo-audit.yml"));
    let notify = jobs.iter().find(|job| job.routes_a_failure()).expect(
        "cargo-audit.yml must declare a job guarded by `failure()` — the scheduled \
         Monday run has no pull request and no author, so a red job with no routing \
         reaches nobody",
    );

    assert!(
        notify
            .needs
            .as_deref()
            .is_some_and(|needs| needs.contains("audit")),
        "the notifying job must depend on the `audit` job it reports on, found {:?}",
        notify.needs
    );
}

#[test]
fn the_notification_opens_or_updates_a_github_issue() {
    let jobs = jobs(&committed(".github/workflows/cargo-audit.yml"));
    let notify = jobs
        .iter()
        .find(|job| job.routes_a_failure())
        .expect("cargo-audit.yml declares a failure-routed job");

    assert!(
        notify.body.contains("gh issue create"),
        "the failure has to land somewhere a maintainer is notified — opening an \
         issue is that channel"
    );
    assert!(
        notify.body.contains("gh issue comment"),
        "a weekly failure must update the open triage issue rather than filing a \
         duplicate every Monday"
    );
    assert!(
        notify.body.contains("set -euo pipefail"),
        "a notification that fails quietly is worse than none: the step must exit \
         non-zero when `gh` fails"
    );
}

#[test]
fn only_the_notification_job_may_write_and_it_writes_issues_alone() {
    let jobs = jobs(&committed(".github/workflows/cargo-audit.yml"));

    let audit = job(&jobs, "audit").expect("cargo-audit.yml declares the `audit` job");
    assert!(
        !audit
            .permissions
            .values()
            .any(|level| level == "write" || level == "write-all"),
        "the auditing job reads the checkout and writes nothing back, found {:?}",
        audit.permissions
    );

    let notify = jobs
        .iter()
        .find(|job| job.routes_a_failure())
        .expect("cargo-audit.yml declares a failure-routed job");
    assert_eq!(
        notify.permissions.get("issues").map(String::as_str),
        Some("write"),
        "opening an issue needs `issues: write` scoped to this job",
    );
    let extra: Vec<&String> = notify
        .permissions
        .iter()
        .filter(|(scope, level)| scope.as_str() != "issues" && level.as_str() != "none")
        .map(|(scope, _)| scope)
        .collect();
    assert!(
        extra.is_empty(),
        "the notifying job needs nothing beyond `issues: write`, found {extra:?}"
    );
}

#[test]
fn security_policy_documents_who_triages_a_failed_security_job() {
    let policy = committed("SECURITY.md");
    let headings: Vec<&str> = policy
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .map(str::trim)
        .collect();

    let escalation = headings
        .iter()
        .find(|heading| heading.to_ascii_lowercase().contains("escalation"))
        .unwrap_or_else(|| {
            panic!(
                "SECURITY.md must carry an internal escalation section — it covers \
                 external reporting only, found headings {headings:?}"
            )
        });

    let section: String = policy
        .lines()
        .skip_while(|line| line.strip_prefix("## ").map(str::trim) != Some(escalation))
        .skip(1)
        .take_while(|line| !line.starts_with("## "))
        .collect::<Vec<&str>>()
        .join("\n");

    assert!(
        section.contains("@stSoftwareAU/developers"),
        "the escalation section must name the triage owner, not just the process"
    );
    assert!(
        section.contains("cargo-audit.yml"),
        "the escalation section must name the job whose failures it routes"
    );
}

#[test]
fn a_workflow_whose_only_job_can_fail_unnoticed_is_rejected() {
    // The parser must not report a pass for the shape this suite exists to
    // catch: one job, no dependent job guarded by `failure()`.
    let unrouted = "name: Cargo Audit\n\
                    \n\
                    jobs:\n\
                    \x20 audit:\n\
                    \x20   runs-on: ubuntu-latest\n\
                    \x20   permissions:\n\
                    \x20     contents: read\n\
                    \x20   steps:\n\
                    \x20     - run: cargo audit\n";

    let parsed = jobs(unrouted);
    assert_eq!(parsed.len(), 1, "one job is declared");
    assert!(
        !parsed[0].routes_a_failure(),
        "a lone job with no `needs:` cannot be reporting another job's failure"
    );
    assert_eq!(
        parsed[0].permissions.get("contents").map(String::as_str),
        Some("read"),
        "the permissions map is read from the job, not from the workflow root"
    );
    assert!(
        parsed[0].body.contains("cargo audit"),
        "the job body keeps the commands its steps run"
    );
}
