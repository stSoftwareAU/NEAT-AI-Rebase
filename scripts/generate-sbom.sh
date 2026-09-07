#!/usr/bin/env bash
# CycloneDX SBOM generation for NEAT-AI-Rebase (Issue #96).
#
# `rebase/Cargo.toml` builds a binary crate in a public repository, and nothing
# in the tree told a downstream consumer what that binary is built from: the
# dependency graph had to be resolved by hand from `Cargo.lock`. This script
# produces the machine-readable manifest instead, and
# `.github/workflows/sbom.yml` uploads what it writes as a build artefact.
#
# Mechanism:
#   * Run `cargo cyclonedx`, which reads `Cargo.lock` *and* `cargo metadata`,
#     so the SBOM reflects the feature set actually resolved rather than the
#     lockfile alone. It writes one document beside each package manifest.
#   * Collect every document it wrote into a single output directory, named
#     `<package-directory>.cdx.json`, so one upload step publishes all of them.
#   * Check each document before collecting it: an empty file, something that
#     is not CycloneDX, or a bill of materials naming no cargo component is a
#     failure. A run that produced nothing is a failure too — "no output" must
#     never be reported as a clean SBOM.
#
# Usage:
#   generate-sbom.sh [--manifest-path PATH] [--output-dir DIR]
#                    [--filename BASE] [--cargo CMD]
#
# Exit codes:
#   0  at least one SBOM was generated, validated and collected
#   1  no SBOM was produced, or what was produced is not a usable SBOM
#   2  usage / environment error (a fault is never reported as a pass)
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: generate-sbom.sh [options]

Options:
  --manifest-path PATH   Workspace manifest to describe (default: Cargo.toml at
                         the repository root).
  --output-dir DIR       Directory the validated SBOMs are collected into
                         (default: <workspace>/sbom).
  --filename BASE        Base name cargo-cyclonedx writes before collection
                         (default: sbom).
  --cargo CMD            Cargo executable providing the `cyclonedx` subcommand
                         (default: $CARGO, else cargo).
  -h, --help             Show this message.

Exits 0 on a validated SBOM, 1 when none was produced or it is unusable,
2 on a usage or environment error.
EOF
}

MANIFEST_PATH=""
OUTPUT_DIR=""
FILENAME="sbom"
CARGO_CMD="${CARGO:-cargo}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest-path)
      [[ $# -ge 2 ]] || { echo "Missing value for --manifest-path" >&2; usage >&2; exit 2; }
      MANIFEST_PATH="$2"; shift 2 ;;
    --output-dir)
      [[ $# -ge 2 ]] || { echo "Missing value for --output-dir" >&2; usage >&2; exit 2; }
      OUTPUT_DIR="$2"; shift 2 ;;
    --filename)
      [[ $# -ge 2 ]] || { echo "Missing value for --filename" >&2; usage >&2; exit 2; }
      FILENAME="$2"; shift 2 ;;
    --cargo)
      [[ $# -ge 2 ]] || { echo "Missing value for --cargo" >&2; usage >&2; exit 2; }
      CARGO_CMD="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ ! "$FILENAME" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "FAIL: --filename must be a plain file name, got '$FILENAME'" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
if [[ -z "$MANIFEST_PATH" ]]; then
  MANIFEST_PATH="$REPO_ROOT/Cargo.toml"
fi

if [[ ! -f "$MANIFEST_PATH" ]]; then
  echo "FAIL: manifest not found: $MANIFEST_PATH" >&2
  exit 2
fi
WORKSPACE_DIR="$(cd "$(dirname "$MANIFEST_PATH")" && pwd)"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$WORKSPACE_DIR/sbom"
fi

if ! command -v "$CARGO_CMD" &>/dev/null; then
  echo "FAIL: '$CARGO_CMD' not found — cargo-cyclonedx is required:" >&2
  echo "      cargo install cargo-cyclonedx" >&2
  exit 2
fi

# A CycloneDX document naming at least one cargo component. Anything else is
# rejected rather than collected: an SBOM that lists nothing is worse than no
# SBOM, because it looks like a clean answer.
validate_sbom() {
  local file="$1"
  if [[ ! -s "$file" ]]; then
    echo "FAIL: $file is empty — cargo cyclonedx wrote no document" >&2
    return 1
  fi
  if ! grep -q '"bomFormat"[[:space:]]*:[[:space:]]*"CycloneDX"' "$file"; then
    echo "FAIL: $file is not a CycloneDX bill of materials" >&2
    return 1
  fi
  if ! grep -q '"specVersion"' "$file"; then
    echo "FAIL: $file declares no CycloneDX specVersion" >&2
    return 1
  fi
  if ! grep -q 'pkg:cargo/' "$file"; then
    echo "FAIL: $file lists no cargo component — an SBOM naming no dependency" >&2
    echo "      is not a dependency manifest" >&2
    return 1
  fi
  return 0
}

# Clear the previous run's collection first: a stale document left behind would
# be uploaded as though this run had produced it.
mkdir -p "$OUTPUT_DIR"
find "$OUTPUT_DIR" -maxdepth 1 -type f -name '*.cdx.json' -delete

echo "Generating CycloneDX SBOM for $MANIFEST_PATH"
if ! "$CARGO_CMD" cyclonedx \
  --manifest-path "$MANIFEST_PATH" \
  --format json \
  --all \
  --all-features \
  --override-filename "$FILENAME"; then
  echo "FAIL: cargo cyclonedx exited non-zero — no SBOM can be trusted from this run" >&2
  exit 2
fi

collected=0
while IFS= read -r produced; do
  [[ -n "$produced" ]] || continue
  if ! validate_sbom "$produced"; then
    exit 1
  fi
  label="$(basename "$(dirname "$produced")")"
  destination="$OUTPUT_DIR/$label.cdx.json"
  mv "$produced" "$destination"
  collected=$((collected + 1))
  echo "OK   $destination"
done < <(
  find "$WORKSPACE_DIR" \
    \( -type d \( -name target -o -path "$OUTPUT_DIR" \) -prune \) -o \
    -type f -name "$FILENAME.json" -print | sort
)

if (( collected == 0 )); then
  cat >&2 <<EOF
FAIL: cargo cyclonedx reported success but no SBOM was written under
      $WORKSPACE_DIR. Nothing is published from this run — an absent artefact
      must not be reported as a generated one.
EOF
  exit 1
fi

echo "OK   $collected SBOM document(s) collected into $OUTPUT_DIR"
exit 0
