//! The repository publishes a machine-readable SBOM for the crate (Issue #96).
//!
//! `rebase/Cargo.toml` builds a binary crate in a public repository, and
//! nothing in the tree told a downstream consumer what that binary is built
//! from: no SBOM was committed and no workflow produced one, so the dependency
//! graph had to be resolved by hand from `Cargo.lock`.
//!
//! `scripts/generate-sbom.sh` closes that: it drives `cargo cyclonedx`, checks
//! what came back really is a CycloneDX document listing cargo components, and
//! collects it under one output directory that
//! `.github/workflows/sbom.yml` uploads with `actions/upload-artifact`. These
//! checks run the script against a stub generator — a generator that writes
//! nothing, writes junk, or dies must never be reported as a clean SBOM — and
//! assert the committed workflow actually runs it and uploads what it wrote.

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

fn generator_script() -> PathBuf {
    repo_root().join("scripts/generate-sbom.sh")
}

/// A committed file, read from the working tree.
///
/// Panics when the file is absent: a missing workflow is the failure this
/// suite exists to catch, never a skip.
fn committed(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be committed: {error}", path.display()))
}

/// A CycloneDX document that names one cargo component — what a healthy
/// `cargo cyclonedx` run produces.
const VALID_SBOM: &str = r#"{
  "bomFormat": "CycloneDX",
  "specVersion": "1.3",
  "version": 1,
  "components": [
    {
      "type": "library",
      "name": "clap",
      "version": "4.5.0",
      "purl": "pkg:cargo/clap@4.5.0"
    }
  ]
}"#;

/// Argument parsing shared by every stub generator: read `--manifest-path` and
/// `--override-filename` back out of the command line the gate builds.
const STUB_PREAMBLE: &str = r#"manifest=""
filename="sbom"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --manifest-path) manifest="$2"; shift 2 ;;
    --override-filename) filename="$2"; shift 2 ;;
    *) shift ;;
  esac
done
workspace="$(cd "$(dirname "$manifest")" && pwd)"
"#;

/// A workspace laid out the way Cargo leaves this repository: a root manifest
/// and one member package, with the stub SBOM landing beside the member.
fn workspace(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir.join("rebase")).expect("create the member package directory");
    fs::write(
        dir.join("Cargo.toml"),
        "[workspace]\nmembers = [\"rebase\"]\n",
    )
    .expect("write the workspace manifest");
    fs::write(
        dir.join("rebase/Cargo.toml"),
        "[package]\nname = \"neat-ai-rebase\"\nversion = \"0.1.0\"\n",
    )
    .expect("write the member manifest");
    dir.join("Cargo.toml")
}

/// Write an executable stub `cargo` whose body is `body`, and return its path.
fn stub_cargo(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("stub-cargo");
    fs::write(&path, format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}"))
        .expect("write the stub generator");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("make the stub generator executable");
    }
    path
}

/// A stub that writes `payload` as the member package's SBOM.
fn stub_writing(dir: &Path, payload: &str) -> PathBuf {
    stub_cargo(
        dir,
        &format!("{STUB_PREAMBLE}cat > \"$workspace/rebase/$filename.json\" <<'SBOM'\n{payload}\nSBOM\n"),
    )
}

/// Run the gate over the workspace at `manifest`, returning its exit code and
/// combined output.
fn run_gate(manifest: &Path, cargo: &Path, extra: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(generator_script())
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--cargo")
        .arg(cargo)
        .args(extra)
        .output()
        .expect("run the SBOM gate");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code(), combined)
}

#[test]
fn a_healthy_generator_yields_a_collected_sbom() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());
    let cargo = stub_writing(dir.path(), VALID_SBOM);

    let (code, output) = run_gate(&manifest, &cargo, &[]);
    assert_eq!(code, Some(0), "gate output:\n{output}");

    let collected = dir.path().join("sbom/rebase.cdx.json");
    let body = fs::read_to_string(&collected)
        .unwrap_or_else(|error| panic!("{} must exist: {error}", collected.display()));
    assert!(
        body.contains("pkg:cargo/clap@4.5.0"),
        "the collected SBOM is not the document the generator wrote:\n{body}"
    );
    assert!(
        !dir.path().join("rebase/sbom.json").exists(),
        "the generated file is collected, not left loose in the workspace"
    );
}

#[test]
fn a_stale_sbom_is_not_republished_as_a_fresh_one() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());
    let cargo = stub_writing(dir.path(), VALID_SBOM);

    let output_dir = dir.path().join("sbom");
    fs::create_dir_all(&output_dir).expect("create the output directory");
    let stale = output_dir.join("last-week.cdx.json");
    fs::write(&stale, "{}").expect("write a stale artefact");

    let (code, output) = run_gate(&manifest, &cargo, &[]);
    assert_eq!(code, Some(0), "gate output:\n{output}");
    assert!(
        !stale.exists(),
        "a previous run's SBOM survived into this run's artefact"
    );
    assert!(output_dir.join("rebase.cdx.json").exists());
}

#[test]
fn a_generator_that_writes_nothing_fails_loudly() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());
    // Exits 0 having produced no SBOM at all — the silent-success shape.
    let cargo = stub_cargo(dir.path(), "exit 0\n");

    let (code, output) = run_gate(&manifest, &cargo, &[]);
    assert_eq!(
        code,
        Some(1),
        "a run that produced no SBOM must fail, output:\n{output}"
    );
    assert!(
        output.contains("no SBOM"),
        "the failure must say what is missing, output:\n{output}"
    );
}

#[test]
fn a_document_that_is_not_cyclonedx_is_rejected() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());
    let cargo = stub_writing(dir.path(), "{ \"hello\": \"world\" }");

    let (code, output) = run_gate(&manifest, &cargo, &[]);
    assert_eq!(code, Some(1), "gate output:\n{output}");
    assert!(
        output.contains("CycloneDX"),
        "the failure must name the format it expected, output:\n{output}"
    );
    assert!(
        !dir.path().join("sbom/rebase.cdx.json").exists(),
        "a rejected document must not reach the artefact directory"
    );
}

#[test]
fn a_document_listing_no_cargo_components_is_rejected() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());
    let cargo = stub_writing(
        dir.path(),
        "{ \"bomFormat\": \"CycloneDX\", \"specVersion\": \"1.3\", \"components\": [] }",
    );

    let (code, output) = run_gate(&manifest, &cargo, &[]);
    assert_eq!(
        code,
        Some(1),
        "an SBOM naming no dependency is not a dependency manifest, output:\n{output}"
    );
    assert!(
        output.contains("component"),
        "the failure must say the components are missing, output:\n{output}"
    );
}

#[test]
fn a_failing_generator_is_a_fault_not_a_pass() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());
    let cargo = stub_cargo(dir.path(), "echo 'cyclonedx exploded' >&2\nexit 3\n");

    let (code, output) = run_gate(&manifest, &cargo, &[]);
    assert_eq!(
        code,
        Some(2),
        "a generator that died is a fault, not an empty SBOM, output:\n{output}"
    );
}

#[test]
fn a_missing_generator_is_a_fault() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());

    let (code, output) = run_gate(&manifest, &dir.path().join("not-installed"), &[]);
    assert_eq!(
        code,
        Some(2),
        "an uninstalled generator must fail, never report an empty SBOM, output:\n{output}"
    );
}

#[test]
fn a_missing_manifest_is_a_fault() {
    let dir = tempfile::tempdir().expect("temporary directory");
    workspace(dir.path());
    let cargo = stub_writing(dir.path(), VALID_SBOM);

    let (code, output) = run_gate(&dir.path().join("absent/Cargo.toml"), &cargo, &[]);
    assert_eq!(code, Some(2), "gate output:\n{output}");
}

#[test]
fn an_unknown_argument_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());
    let cargo = stub_writing(dir.path(), VALID_SBOM);

    let (code, output) = run_gate(&manifest, &cargo, &["--publish-everywhere"]);
    assert_eq!(code, Some(2), "gate output:\n{output}");
    assert!(output.contains("Usage"), "gate output:\n{output}");
}

#[test]
fn the_output_directory_is_selectable() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let manifest = workspace(dir.path());
    let cargo = stub_writing(dir.path(), VALID_SBOM);
    let chosen = dir.path().join("artefacts");

    let (code, output) = run_gate(
        &manifest,
        &cargo,
        &["--output-dir", chosen.to_str().expect("utf-8 path")],
    );
    assert_eq!(code, Some(0), "gate output:\n{output}");
    assert!(chosen.join("rebase.cdx.json").exists());
}

/// Every `uses:` reference in a workflow, as written.
fn action_references(workflow: &str) -> Vec<String> {
    workflow
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("- uses:").or(line.strip_prefix("uses:")))
        .map(|reference| {
            reference
                .split('#')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .collect()
}

#[test]
fn the_sbom_workflow_runs_the_gate_and_uploads_what_it_wrote() {
    let workflow = committed(".github/workflows/sbom.yml");

    assert!(
        workflow.contains("./scripts/generate-sbom.sh"),
        "the workflow must run the committed gate, not an inline command that can drift"
    );

    let uploads = action_references(&workflow)
        .into_iter()
        .find(|reference| reference.starts_with("actions/upload-artifact@"))
        .expect("the workflow uploads the SBOM with actions/upload-artifact");
    let sha = uploads
        .split_once('@')
        .expect("a pinned action reference")
        .1;
    assert!(
        sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
        "actions/upload-artifact is pinned to `{sha}`, not a 40-character commit SHA — \
         a hijacked tag would execute here"
    );
    assert!(
        workflow.contains("if-no-files-found: error"),
        "an upload that finds no SBOM must fail the job, not publish an empty artefact"
    );
}

#[test]
fn every_action_the_sbom_workflow_uses_is_pinned() {
    for reference in action_references(&committed(".github/workflows/sbom.yml")) {
        if reference.starts_with("./") {
            continue; // a local composite action is committed here, not fetched.
        }
        let (_, sha) = reference
            .split_once('@')
            .unwrap_or_else(|| panic!("`{reference}` carries no version at all"));
        assert!(
            sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()),
            "`{reference}` is not pinned to a 40-character commit SHA"
        );
    }
}

#[test]
fn the_sbom_workflow_is_scheduled_dispatchable_and_read_only() {
    let workflow = committed(".github/workflows/sbom.yml");

    assert!(
        workflow.contains("  schedule:") && workflow.contains("cron:"),
        "the SBOM must be refreshed on a schedule, not only when a PR happens to land"
    );
    assert!(
        workflow.contains("  workflow_dispatch:"),
        "the workflow must stay re-runnable by hand"
    );

    let permissions = workflow
        .lines()
        .skip_while(|line| line.trim_end() != "permissions:")
        .nth(1)
        .expect("the workflow declares a top-level `permissions:` block");
    assert_eq!(
        permissions.trim(),
        "contents: read",
        "generating an SBOM reads the checkout and writes nothing back"
    );
}
