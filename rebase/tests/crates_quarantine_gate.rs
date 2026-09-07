//! An automated Cargo bump must not propose a crate published minutes ago
//! (Issue #91).
//!
//! `.github/workflows/cargo-upgrade.yml` resolves whatever crates.io offers at
//! run time. `cargo deny check` judges licences and known advisories and the
//! suite judges behaviour — neither judges *recency*, so a compromised release
//! with no advisory yet would sail through both. These checks drive
//! `scripts/crates-quarantine.sh`, the gate that closes that window, and assert
//! the committed workflow actually runs it before opening the pull request.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

fn gate_script() -> PathBuf {
    repo_root().join("scripts/crates-quarantine.sh")
}

/// Is `curl` on this machine? CI has it; a bare box may not, and those checks
/// are skipped rather than faked.
fn curl_installed() -> bool {
    Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// A lockfile holding exactly these `(name, version)` crates.io packages, plus
/// the local `path` package Cargo always records first.
fn lockfile(packages: &[(&str, &str)]) -> String {
    let mut text = String::from(
        "version = 4\n\n\
         [[package]]\n\
         name = \"neat-ai-rebase\"\n\
         version = \"0.1.0\"\n\n",
    );
    for (name, version) in packages {
        text.push_str(&format!(
            "[[package]]\n\
             name = \"{name}\"\n\
             version = \"{version}\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
             checksum = \"0000000000000000000000000000000000000000000000000000000000000000\"\n\n"
        ));
    }
    text
}

/// Write a stub crates.io version endpoint: `<dir>/api/crates/<name>/<version>`
/// carrying the response body crates.io returns for that version.
fn publish_date(dir: &Path, name: &str, version: &str, created_at: &str) {
    let crate_dir = dir.join("api/crates").join(name);
    fs::create_dir_all(&crate_dir).expect("create stub api directory");
    fs::write(
        crate_dir.join(version),
        format!(
            "{{\"version\":{{\"crate\":\"{name}\",\"num\":\"{version}\",\
             \"created_at\":\"{created_at}\",\"yanked\":false}}}}"
        ),
    )
    .expect("write stub version response");
}

/// Run the gate over `lockfile` against the stub API in `dir`, with `--now`
/// fixed so the verdict does not drift with the wall clock.
fn run_gate(dir: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(gate_script())
        .args(args)
        .arg("--api-base")
        .arg(format!("file://{}/api", dir.display()))
        .arg("--now")
        .arg("2026-01-10T00:00:00Z")
        .current_dir(repo_root())
        .output()
        .expect("run the quarantine gate");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code(), combined)
}

/// Set up a temporary directory holding `baseline.lock`, `new.lock` and the
/// stub API, returning the directory.
fn fixture(baseline: &[(&str, &str)], updated: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temp dir");
    fs::write(dir.path().join("baseline.lock"), lockfile(baseline)).expect("write baseline");
    fs::write(dir.path().join("new.lock"), lockfile(updated)).expect("write new lockfile");
    dir
}

fn lock_args(dir: &Path) -> [String; 4] {
    [
        "--baseline-lockfile".to_string(),
        dir.join("baseline.lock").display().to_string(),
        "--lockfile".to_string(),
        dir.join("new.lock").display().to_string(),
    ]
}

#[test]
fn a_version_published_inside_the_window_fails_the_gate() {
    if !curl_installed() {
        eprintln!("curl not installed — skipping");
        return;
    }
    let dir = fixture(&[("serde", "1.0.200")], &[("serde", "1.0.201")]);
    // Published two hours before `--now`: the exact case a compromised release
    // exploits.
    publish_date(dir.path(), "serde", "1.0.201", "2026-01-09T22:00:00+00:00");

    let args = lock_args(dir.path());
    let (code, output) = run_gate(
        dir.path(),
        &[&args[0], &args[1], &args[2], &args[3], "--hours", "24"],
    );

    assert_eq!(
        code,
        Some(1),
        "quarantined bump must fail the gate: {output}"
    );
    assert!(
        output.contains("QUARANTINED serde 1.0.201"),
        "the gate must name the crate it held back: {output}"
    );
}

#[test]
fn a_version_older_than_the_window_passes_the_gate() {
    if !curl_installed() {
        eprintln!("curl not installed — skipping");
        return;
    }
    let dir = fixture(&[("serde", "1.0.200")], &[("serde", "1.0.201")]);
    publish_date(dir.path(), "serde", "1.0.201", "2026-01-05T00:00:00+00:00");

    let args = lock_args(dir.path());
    let (code, output) = run_gate(
        dir.path(),
        &[&args[0], &args[1], &args[2], &args[3], "--hours", "24"],
    );

    assert_eq!(code, Some(0), "an aged version must pass: {output}");
    assert!(
        output.contains("OK         serde 1.0.201"),
        "the gate must report what it cleared: {output}"
    );
}

#[test]
fn only_versions_the_bump_moved_are_judged() {
    if !curl_installed() {
        eprintln!("curl not installed — skipping");
        return;
    }
    // `clap` is unchanged, so no publish date is stubbed for it: querying it
    // would fail the run, which is how this check proves it is not queried.
    let dir = fixture(
        &[("clap", "4.5.4"), ("serde", "1.0.200")],
        &[("clap", "4.5.4"), ("serde", "1.0.201")],
    );
    publish_date(dir.path(), "serde", "1.0.201", "2026-01-05T00:00:00+00:00");

    let args = lock_args(dir.path());
    let (code, output) = run_gate(
        dir.path(),
        &[&args[0], &args[1], &args[2], &args[3], "--hours", "24"],
    );

    assert_eq!(
        code,
        Some(0),
        "unchanged crates must not be judged: {output}"
    );
    assert!(
        !output.contains("clap"),
        "an unmoved crate must not be queried at all: {output}"
    );
}

#[test]
fn every_registry_version_is_judged_without_a_baseline() {
    if !curl_installed() {
        eprintln!("curl not installed — skipping");
        return;
    }
    let dir = fixture(&[], &[("clap", "4.5.4"), ("serde", "1.0.201")]);
    publish_date(dir.path(), "clap", "4.5.4", "2026-01-01T00:00:00Z");
    publish_date(dir.path(), "serde", "1.0.201", "2026-01-09T23:00:00Z");

    let lock = dir.path().join("new.lock").display().to_string();
    let (code, output) = run_gate(dir.path(), &["--lockfile", &lock, "--hours", "24"]);

    assert_eq!(code, Some(1), "the fresh crate must still fail: {output}");
    assert!(
        output.contains("OK         clap 4.5.4") && output.contains("QUARANTINED serde 1.0.201"),
        "both crates must be judged with no baseline: {output}"
    );
}

#[test]
fn an_unreadable_publish_date_fails_loudly_rather_than_passing() {
    if !curl_installed() {
        eprintln!("curl not installed — skipping");
        return;
    }
    // No stub written for the moved version: crates.io being unreachable must
    // never be reconciled as "outside the window".
    let dir = fixture(&[("serde", "1.0.200")], &[("serde", "1.0.201")]);

    let args = lock_args(dir.path());
    let (code, output) = run_gate(dir.path(), &[&args[0], &args[1], &args[2], &args[3]]);

    assert_eq!(code, Some(2), "a failed fetch must not pass: {output}");
    assert!(
        output.contains("could not read publish date for serde 1.0.201"),
        "the fetch failure must be named: {output}"
    );
}

#[test]
fn a_malformed_created_at_fails_loudly_rather_than_passing() {
    if !curl_installed() {
        eprintln!("curl not installed — skipping");
        return;
    }
    let dir = fixture(&[("serde", "1.0.200")], &[("serde", "1.0.201")]);
    publish_date(dir.path(), "serde", "1.0.201", "yesterday, about lunchtime");

    let args = lock_args(dir.path());
    let (code, output) = run_gate(dir.path(), &[&args[0], &args[1], &args[2], &args[3]]);

    assert_eq!(
        code,
        Some(2),
        "an unparseable stamp must not pass: {output}"
    );
    assert!(
        output.contains("unparseable created_at"),
        "the parse failure must be named: {output}"
    );
}

#[test]
fn an_exempt_crate_skips_the_window() {
    if !curl_installed() {
        eprintln!("curl not installed — skipping");
        return;
    }
    // An internal `stSoftwareAU` crate consumed from crates.io: exempt by
    // policy, and no publish date is stubbed, so the exemption is what keeps
    // the run from failing.
    let dir = fixture(&[], &[("stsoftware-neat-core", "0.4.0")]);

    let lock = dir.path().join("new.lock").display().to_string();
    let (code, output) = run_gate(
        dir.path(),
        &["--lockfile", &lock, "--exempt", "stsoftware-*"],
    );

    assert_eq!(code, Some(0), "an exempt crate must pass: {output}");
    assert!(
        output.contains("EXEMPT     stsoftware-neat-core 0.4.0"),
        "the exemption must be reported, not silent: {output}"
    );
}

#[test]
fn path_dependencies_carry_no_publish_date_and_are_not_queried() {
    if !curl_installed() {
        eprintln!("curl not installed — skipping");
        return;
    }
    // `lockfile` always emits the local `neat-ai-rebase` package with no
    // `source`; with no registry packages beside it the gate must still pass
    // without reaching the network.
    let dir = fixture(&[], &[]);

    let lock = dir.path().join("new.lock").display().to_string();
    let (code, output) = run_gate(dir.path(), &["--lockfile", &lock]);

    assert_eq!(code, Some(0), "a path-only lockfile must pass: {output}");
    assert!(
        output.contains("OK   0 newly resolved"),
        "the gate must say it judged nothing: {output}"
    );
}

#[test]
fn a_non_utc_now_is_rejected() {
    let output = Command::new(gate_script())
        .args(["--now", "2026-01-10 09:00:00 +1000"])
        .current_dir(repo_root())
        .output()
        .expect("run the quarantine gate");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a stamp with no UTC designator must be a usage error: {stderr}"
    );
    assert!(
        stderr.contains("--now must be a UTC RFC 3339 stamp"),
        "the rejection must say why: {stderr}"
    );
}

#[test]
fn the_upgrade_workflow_runs_the_quarantine_gate_before_opening_the_pull_request() {
    let path = repo_root().join(".github/workflows/cargo-upgrade.yml");
    let workflow = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    let gate = workflow.find("scripts/crates-quarantine.sh").expect(
        "cargo-upgrade.yml must run scripts/crates-quarantine.sh over the refreshed lockfile",
    );
    let create_pr = workflow
        .find("peter-evans/create-pull-request")
        .expect("cargo-upgrade.yml opens a pull request");

    assert!(
        gate < create_pr,
        "the quarantine gate must run before the pull request is opened, not after"
    );
    assert!(
        workflow.contains("--baseline-lockfile"),
        "the gate must be given the pre-bump lockfile so it judges only what the bump moved"
    );
}
