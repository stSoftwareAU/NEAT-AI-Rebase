//! The `neat-core` pin and the family-sync step that moves it (Issue #107).
//!
//! `neat-core` is consumed as a git dependency pinned to a NEAT-AI-core
//! *release* tag, not as a sibling `path` dependency: the workspace builds with
//! no checkout beside it, and a core release reaches this repository only when
//! `scripts/family-pins.sh` moves the pin on one of this repository's own pull
//! requests. Both halves fail silently when they drift — a path dependency
//! creeping back builds green on a developer's machine and nowhere else, and a
//! `Cargo.lock` left behind the manifest pins the build to a tag the manifest
//! no longer names. These tests read the committed files and assert both.
//!
//! The copied helpers themselves are NEAT-AI-core's: they are never edited
//! here, so what is asserted is their argument contract, not their text.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Repository root — `CARGO_MANIFEST_DIR` is `<root>/rebase`.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has a parent")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The value of `key = "<value>"` inside the single-line declaration `line`.
fn inline_value(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key} = \"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The `neat-core = { … }` declaration of `rebase/Cargo.toml`.
fn neat_core_declaration(manifest: &str) -> String {
    manifest
        .lines()
        .find(|line| line.trim_start().starts_with("neat-core = "))
        .unwrap_or_else(|| panic!("rebase/Cargo.toml declares a `neat-core` dependency"))
        .to_string()
}

fn is_release_tag(tag: &str) -> bool {
    let Some(numbers) = tag.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<&str> = numbers.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

#[test]
fn inline_value_reads_one_key_of_a_declaration() {
    let line = r#"neat-core = { git = "https://example.test/repo", tag = "v1.2.3" }"#;
    assert_eq!(
        inline_value(line, "git").as_deref(),
        Some("https://example.test/repo")
    );
    assert_eq!(inline_value(line, "tag").as_deref(), Some("v1.2.3"));
    assert_eq!(inline_value(line, "path"), None);
}

#[test]
fn release_tags_exclude_pre_releases_and_branches() {
    assert!(is_release_tag("v0.22.5"));
    assert!(is_release_tag("v1.0.0"));
    assert!(!is_release_tag("v1.0.0-rc1"));
    assert!(!is_release_tag("Develop"));
    assert!(!is_release_tag("v1.0"));
}

#[test]
fn neat_core_is_pinned_to_a_core_release_tag() {
    let declaration = neat_core_declaration(&read("rebase/Cargo.toml"));
    assert!(
        inline_value(&declaration, "path").is_none(),
        "neat-core is declared as a path dependency ({declaration}) — the \
         workspace would need a sibling NEAT-AI-core checkout to build"
    );
    assert_eq!(
        inline_value(&declaration, "git").as_deref(),
        Some("https://github.com/stSoftwareAU/NEAT-AI-core"),
        "neat-core must come from the NEAT-AI-core repository: {declaration}"
    );
    let tag = inline_value(&declaration, "tag")
        .unwrap_or_else(|| panic!("neat-core carries no `tag =` pin: {declaration}"));
    assert!(
        is_release_tag(&tag),
        "neat-core is pinned to '{tag}', which is not a v<major>.<minor>.<patch> \
         release tag — a branch or a pre-release would track head again"
    );
}

#[test]
fn cargo_lock_resolves_the_pinned_tag() {
    let tag = inline_value(&neat_core_declaration(&read("rebase/Cargo.toml")), "tag")
        .expect("neat-core carries a `tag =` pin");
    let lock = read("Cargo.lock");
    let source = lock
        .lines()
        .skip_while(|line| line.trim() != r#"name = "neat-core""#)
        .find(|line| line.trim_start().starts_with("source = "))
        .unwrap_or_else(|| panic!("Cargo.lock records a source for neat-core"));
    let expected = format!("git+https://github.com/stSoftwareAU/NEAT-AI-core?tag={tag}#");
    assert!(
        source.contains(&expected),
        "Cargo.lock resolves neat-core from {source}, which is not the manifest \
         pin {tag} — the lockfile and the manifest disagree about what is built"
    );
}

/// Position of the step whose body carries `command`, counted in declared
/// steps. Comment lines are skipped, so the prose at the top of a workflow —
/// which names every command the job runs — can never stand in for the step
/// that runs it.
fn step_running(workflow: &str, command: &str) -> Option<usize> {
    let mut step = 0usize;
    let mut seen_a_step = false;
    for line in workflow.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("- name:") {
            if seen_a_step {
                step += 1;
            }
            seen_a_step = true;
        }
        if seen_a_step && trimmed.contains(command) {
            return Some(step);
        }
    }
    None
}

#[test]
fn step_running_ignores_the_header_prose() {
    let workflow = concat!(
        "# This job runs ./scripts/family-pins.sh before the bump.\n",
        "jobs:\n",
        "  bump:\n",
        "    steps:\n",
        "      - name: Bump\n",
        "        run: ./scripts/auto-version.sh manifest\n",
        "      - name: Move the pin\n",
        "        run: ./scripts/family-pins.sh\n",
    );
    assert_eq!(step_running(workflow, "./scripts/auto-version.sh"), Some(0));
    assert_eq!(step_running(workflow, "./scripts/family-pins.sh"), Some(1));
    assert_eq!(step_running(workflow, "./scripts/absent.sh"), None);
}

#[test]
fn version_increment_syncs_the_helpers_before_it_bumps() {
    let workflow = read(".github/workflows/version-increment.yml");
    let fetch = step_running(&workflow, "NEAT-AI-core/Develop/scripts/runlib.sh")
        .expect("version-increment.yml has a step fetching core's runlib.sh");
    let fetch_pins = step_running(&workflow, "NEAT-AI-core/Develop/scripts/family-pins.sh")
        .expect("version-increment.yml has a step fetching core's family-pins.sh");
    let sync = step_running(&workflow, "./scripts/family-pins.sh")
        .expect("version-increment.yml has a step running ./scripts/family-pins.sh");
    let bump = step_running(&workflow, "./scripts/auto-version.sh rebase/Cargo.toml")
        .expect("version-increment.yml has a step running the bump");
    assert!(
        fetch <= sync && fetch_pins <= sync,
        "version-increment.yml refreshes the copies (steps {fetch}/{fetch_pins}) \
         after running the pin mover (step {sync}) — the PR would move its pin \
         with a stale copy of the script"
    );
    assert!(
        sync < bump,
        "version-increment.yml moves the neat-core pin (step {sync}) after the \
         bump (step {bump}) — the moved pin would land at an unchanged crate \
         version"
    );
}

#[test]
fn version_increment_commits_the_refreshed_copies_with_the_bump() {
    let workflow = read(".github/workflows/version-increment.yml");
    let staged = workflow
        .split("SYNCED_PATHS=(")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("version-increment.yml lists the paths its commit stages");
    for path in [
        "rebase/Cargo.toml",
        "Cargo.lock",
        "scripts/runlib.sh",
        "scripts/family-pins.sh",
    ] {
        assert!(
            staged.contains(path),
            "version-increment.yml stages {staged:?}, which omits {path} — that \
             change would be made on the runner and thrown away"
        );
    }
}

/// Both copied helpers refuse an argument they do not know, rather than
/// silently doing their default work: a caller that mistyped a flag must not
/// get a full build or a rewritten manifest.
#[test]
fn copied_helpers_refuse_an_unknown_argument() {
    for helper in ["scripts/runlib.sh", "scripts/family-pins.sh"] {
        let output = Command::new("bash")
            .arg(repo_root().join(helper))
            .arg("--not-a-real-flag")
            .current_dir(repo_root())
            .output()
            .unwrap_or_else(|error| panic!("run {helper}: {error}"));
        assert_eq!(
            output.status.code(),
            Some(2),
            "{helper} exited {:?} for an unknown argument; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{helper} wrote to stdout while refusing an unknown argument: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
