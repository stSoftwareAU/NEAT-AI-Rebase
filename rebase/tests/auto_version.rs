//! Behaviour of `scripts/auto-version.sh`, the CI auto-increment gate
//! (Issue #106).
//!
//! The unattended machines rebuild only when the crate version changes, so
//! every PR must leave the version ahead of the base branch. These tests drive
//! the real script over throw-away manifests and assert on exit codes, stderr
//! and the rewritten files.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/auto-version.sh")
}

fn run(args: &[&str]) -> Output {
    Command::new("bash")
        .arg(script())
        .args(args)
        .output()
        .expect("run auto-version.sh")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A manifest whose `[package]` version is `version`, plus a dependency that
/// also carries a `version` key so the parser cannot simply take the last one.
fn manifest(name: &str, version: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2024\"\n\n\
         [lib]\nname = \"{name}\"\npath = \"src/lib.rs\"\n\n\
         [dependencies]\nclap = {{ version = \"4\" }}\n"
    )
}

fn lockfile(entries: &[(&str, &str)]) -> String {
    let mut out = String::from("version = 4\n");
    for (name, version) in entries {
        out.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n"
        ));
    }
    out
}

struct Fixture {
    _dir: tempfile::TempDir,
    manifest: PathBuf,
    lock: PathBuf,
}

impl Fixture {
    fn new(name: &str, version: &str, lock_entries: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest_path = dir.path().join("Cargo.toml");
        let lock_path = dir.path().join("Cargo.lock");
        std::fs::write(&manifest_path, manifest(name, version)).expect("write manifest");
        std::fs::write(&lock_path, lockfile(lock_entries)).expect("write lockfile");
        Self {
            _dir: dir,
            manifest: manifest_path,
            lock: lock_path,
        }
    }

    fn manifest_text(&self) -> String {
        std::fs::read_to_string(&self.manifest).expect("read manifest")
    }

    fn lock_text(&self) -> String {
        std::fs::read_to_string(&self.lock).expect("read lockfile")
    }

    fn bump(&self, base: &str) -> Output {
        run(&[
            self.manifest.to_str().unwrap(),
            base,
            self.lock.to_str().unwrap(),
        ])
    }
}

#[test]
fn bumps_the_patch_when_the_pr_has_not_bumped_it() {
    let fixture = Fixture::new("neat-ai-rebase", "0.1.0", &[("neat-ai-rebase", "0.1.0")]);
    let out = fixture.bump("0.1.0");

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "0.1.1");
    assert!(fixture.manifest_text().contains("version = \"0.1.1\""));
    assert!(fixture.lock_text().contains("version = \"0.1.1\""));
}

#[test]
fn bumps_arbitrary_versions_not_just_the_current_one() {
    for (current, expected) in [
        ("1.2.9", "1.2.10"),
        ("3.0.0", "3.0.1"),
        ("0.4.19", "0.4.20"),
    ] {
        let fixture = Fixture::new("some_crate", current, &[("some_crate", current)]);
        let out = fixture.bump(current);

        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(stdout(&out), expected, "bumping {current}");
        assert!(
            fixture
                .manifest_text()
                .contains(&format!("version = \"{expected}\"")),
            "manifest not rewritten for {current}"
        );
    }
}

#[test]
fn leaves_the_version_alone_when_the_pr_already_bumped_it() {
    let fixture = Fixture::new("neat-ai-rebase", "0.2.0", &[("neat-ai-rebase", "0.2.0")]);
    let before = fixture.manifest_text();
    let out = fixture.bump("0.1.7");

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "0.2.0");
    assert_eq!(fixture.manifest_text(), before, "manifest was rewritten");
    assert!(stderr(&out).contains("already ahead"));
}

#[test]
fn rejects_a_downgrade_against_the_base_branch() {
    let fixture = Fixture::new("neat-ai-rebase", "0.1.4", &[("neat-ai-rebase", "0.1.4")]);
    let before = fixture.manifest_text();
    let out = fixture.bump("0.2.0");

    assert!(!out.status.success(), "a downgrade must fail loud");
    assert!(stderr(&out).contains("downgraded"), "{}", stderr(&out));
    assert_eq!(fixture.manifest_text(), before, "manifest was rewritten");
}

#[test]
fn rewrites_only_the_named_package_in_the_lockfile() {
    let fixture = Fixture::new(
        "neat-ai-rebase",
        "0.1.0",
        &[
            ("clap", "0.1.0"),
            ("neat-ai-rebase", "0.1.0"),
            ("serde", "0.1.0"),
        ],
    );
    let out = fixture.bump("0.1.0");

    assert!(out.status.success(), "{}", stderr(&out));
    let lock = fixture.lock_text();
    assert!(lock.contains("name = \"neat-ai-rebase\"\nversion = \"0.1.1\""));
    assert!(lock.contains("name = \"clap\"\nversion = \"0.1.0\""));
    assert!(lock.contains("name = \"serde\"\nversion = \"0.1.0\""));
}

#[test]
fn fails_loud_when_the_package_is_absent_from_the_lockfile() {
    let fixture = Fixture::new("neat-ai-rebase", "0.1.0", &[("clap", "4.5.0")]);
    let out = fixture.bump("0.1.0");

    assert!(!out.status.success(), "a stale lockfile must fail loud");
    assert!(stderr(&out).contains("neat-ai-rebase"), "{}", stderr(&out));
}

#[test]
fn print_mode_reports_the_package_version() {
    let fixture = Fixture::new("neat-ai-rebase", "1.4.2", &[("neat-ai-rebase", "1.4.2")]);
    let out = run(&["--print", fixture.manifest.to_str().unwrap()]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "1.4.2");
}

#[test]
fn rejects_a_malformed_base_version() {
    let fixture = Fixture::new("neat-ai-rebase", "0.1.0", &[("neat-ai-rebase", "0.1.0")]);
    let out = fixture.bump("not-a-version");

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("malformed version"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn rejects_a_missing_manifest() {
    let out = run(&["/nonexistent/Cargo.toml", "0.1.0"]);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no such manifest"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn rejects_wrong_usage() {
    let out = run(&["only-one-argument"]);

    assert!(!out.status.success());
    assert!(stderr(&out).contains("usage:"), "{}", stderr(&out));
}

#[test]
fn the_repository_manifest_is_ahead_of_or_level_with_the_lockfile() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let out = run(&["--print", root.join("rebase/Cargo.toml").to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let version = stdout(&out);

    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("read Cargo.lock");
    assert!(
        lock.contains(&format!(
            "name = \"neat-ai-rebase\"\nversion = \"{version}\""
        )),
        "Cargo.lock does not record neat-ai-rebase {version}"
    );
}
