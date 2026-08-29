//! The workflow YAML itself must be linted, and the gate must fail loudly
//! (Issue #56).
//!
//! `actionlint` is the standard linter for GitHub Actions workflows: it
//! catches invalid `${{ }}` expressions, unknown contexts, bad `runs-on`
//! labels and shell bugs inside `run:` blocks — none of which any other gate
//! in this repository sees. The checks below drive `scripts/actionlint.sh`,
//! the single script CI and `quality.sh` both invoke, so the two cannot drift.

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
    repo_root().join("scripts/actionlint.sh")
}

/// Is `actionlint` on this machine? CI installs it; a bare developer box may
/// not have it, and those checks are skipped rather than faked.
fn actionlint_installed() -> bool {
    Command::new("actionlint")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Run the gate script with `args`, returning `(success, combined output)`.
fn run_gate(args: &[&Path], path_env: Option<&str>) -> (bool, String) {
    let mut command = Command::new(gate_script());
    command.args(args).current_dir(repo_root());
    if let Some(path) = path_env {
        command.env("PATH", path);
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", gate_script().display()));
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), combined)
}

/// The first directory on `PATH` holding an executable `tool`.
fn locate(tool: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|dir| dir.join(tool))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{tool} is not on PATH"))
}

/// The gate is only a gate if a missing linter stops the build. Silently
/// passing when `actionlint` is absent would report a workflow regression as
/// success.
#[test]
fn gate_fails_loudly_when_actionlint_is_missing() {
    // A `PATH` holding just enough to run the script — the shebang's `env`
    // and `bash` itself — and deliberately no `actionlint`.
    let stripped = tempfile::tempdir().expect("create temp dir");
    for tool in ["env", "bash"] {
        std::os::unix::fs::symlink(locate(tool), stripped.path().join(tool))
            .unwrap_or_else(|error| panic!("symlink {tool}: {error}"));
    }
    let path_env = stripped.path().to_str().expect("temp path is UTF-8");
    let (success, output) = run_gate(&[], Some(path_env));

    assert!(
        !success,
        "gate passed with no actionlint on PATH; output:\n{output}"
    );
    assert!(
        output.contains("actionlint"),
        "gate failed without naming the missing tool; output:\n{output}"
    );
}

/// The committed workflows lint clean, so the gate is green on a clean tree.
#[test]
fn committed_workflows_lint_clean() {
    if !actionlint_installed() {
        eprintln!("skipping: actionlint is not installed on this machine");
        return;
    }

    let (success, output) = run_gate(&[], None);
    assert!(
        success,
        "actionlint rejected the committed workflows:\n{output}"
    );
}

/// Regression case: a workflow with an undefined `github` context property is
/// exactly the class of typo no other gate here catches, and it must fail.
#[test]
fn a_broken_workflow_fails_the_gate() {
    if !actionlint_installed() {
        eprintln!("skipping: actionlint is not installed on this machine");
        return;
    }

    let dir = tempfile::tempdir().expect("create temp dir");
    let broken = dir.path().join("broken.yml");
    std::fs::write(
        &broken,
        concat!(
            "name: Broken\n",
            "on: push\n",
            "jobs:\n",
            "  broken:\n",
            "    runs-on: ubuntu-latest\n",
            "    steps:\n",
            "      - run: echo \"${{ github.no_such_field }}\"\n",
        ),
    )
    .expect("write broken workflow");

    let (success, output) = run_gate(&[&broken], None);
    assert!(
        !success,
        "gate accepted a workflow with an undefined context property:\n{output}"
    );
    assert!(
        output.contains("no_such_field"),
        "gate failed without reporting the offending expression:\n{output}"
    );
}

/// CI must actually run the gate — a script nobody invokes gates nothing.
#[test]
fn ci_invokes_the_shared_gate_script() {
    let path = repo_root().join(".github/workflows/actionlint.yml");
    let workflow = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    assert!(
        workflow.contains("./scripts/actionlint.sh"),
        "actionlint.yml does not invoke ./scripts/actionlint.sh, so CI and \
         quality.sh can drift"
    );
}

/// `quality.sh` claims to mirror CI, so the same script has to run locally.
#[test]
fn quality_gate_invokes_the_shared_gate_script() {
    let path = repo_root().join("quality.sh");
    let quality = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    assert!(
        quality.contains("./scripts/actionlint.sh"),
        "quality.sh does not invoke ./scripts/actionlint.sh, so a workflow \
         regression only surfaces in CI"
    );
}
