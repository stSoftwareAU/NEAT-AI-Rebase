#!/usr/bin/env bash
# Gate against an unhandled breaking neat-core bump.
#
# Rebase consumes neat-core through an UNPINNED `path` dependency that always
# tracks head (see `forests/Cargo.toml`). The safeguard is this CI gate: it
# fails when the sibling neat-core presents a breaking bump Rebase has not yet
# acknowledged, forcing a deliberate upgrade instead of tracking head blindly.
#
# Mechanism — version-baseline check:
#   * Rebase records the last-handled neat-core version in the checked-in
#     `neat-core.expected-version` file.
#   * CI reads neat-core's actual version from the cloned sibling
#     `../NEAT-AI-core/Cargo.toml` ([workspace.package] version).
#   * The "breaking component" is the major for >= 1.0 releases and the minor
#     for pre-1.0 (0.x) releases, per SemVer. The gate FAILS when neat-core's
#     breaking component is greater than the recorded baseline; it PASSES on
#     patch-level drift (policy) and when the two match.
#
# "Handling" a breaking bump = a deliberate Rebase PR that makes the
# corresponding code change AND bumps the recorded baseline to the new
# neat-core version.
#
# Usage:
#   check-neat-core-version.sh [--baseline PATH] [--core-manifest PATH]
#
# Defaults resolve the same paths Cargo uses: the baseline at the repo root and
# the neat-core workspace manifest at the sibling `../NEAT-AI-core/Cargo.toml`
# (the symlink CI creates makes this resolve on the runner too — see
# `.github/workflows/ci.yml`).
#
# Exit codes:
#   0  versions are compatible (match, patch drift, or core behind baseline)
#   1  breaking neat-core bump above the recorded baseline — gate fails
#   2  usage / parse error (missing file, malformed version)
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: check-neat-core-version.sh [--baseline PATH] [--core-manifest PATH]

Options:
  --baseline PATH        File recording the last-handled neat-core version
                         (default: neat-core.expected-version at the repo root).
  --core-manifest PATH   neat-core workspace Cargo.toml carrying
                         [workspace.package] version (default: the sibling
                         ../NEAT-AI-core/Cargo.toml).
  -h, --help             Show this message.

Exits 0 when compatible, 1 on an unhandled breaking bump, 2 on a usage error.
EOF
}

BASELINE=""
CORE_MANIFEST=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --baseline)
      [[ $# -ge 2 ]] || { echo "Missing value for --baseline" >&2; usage >&2; exit 2; }
      BASELINE="$2"
      shift 2
      ;;
    --core-manifest)
      [[ $# -ge 2 ]] || { echo "Missing value for --core-manifest" >&2; usage >&2; exit 2; }
      CORE_MANIFEST="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ -z "$BASELINE" ]]; then
  BASELINE="$REPO_ROOT/neat-core.expected-version"
fi
if [[ -z "$CORE_MANIFEST" ]]; then
  CORE_MANIFEST="$REPO_ROOT/../NEAT-AI-core/Cargo.toml"
fi

if [[ ! -f "$BASELINE" ]]; then
  echo "FAIL: baseline file not found: $BASELINE" >&2
  exit 2
fi
if [[ ! -f "$CORE_MANIFEST" ]]; then
  echo "FAIL: neat-core manifest not found: $CORE_MANIFEST" >&2
  echo "      Is the sibling NEAT-AI-core clone present? CI clones + symlinks it." >&2
  exit 2
fi

# First non-comment, non-blank line of the baseline file is the version.
read_baseline_version() {
  awk '
    { sub(/#.*/, "") }            # strip inline comments
    { gsub(/[[:space:]]+/, "") }  # trim all whitespace
    NF { print; exit }            # first non-empty line wins
  ' "$BASELINE"
}

# Extract [workspace.package] version from the neat-core manifest. Only the
# version key that lives under the [workspace.package] table counts — a bare
# scan would also match dependency versions.
read_core_version() {
  awk '
    /^\[/ { in_wp = ($0 ~ /^\[workspace\.package\]/) }
    in_wp && /^[[:space:]]*version[[:space:]]*=/ {
      if (match($0, /"[^"]*"/)) {
        v = substr($0, RSTART + 1, RLENGTH - 2)
        print v
        exit
      }
    }
  ' "$CORE_MANIFEST"
}

# Validate X.Y.Z (optionally with a -prerelease/+build suffix we ignore) and
# echo the bare "major minor patch" triple. Returns non-zero when malformed.
parse_semver() {
  local raw="$1" core
  core="${raw%%[-+]*}"   # drop pre-release / build metadata
  if [[ ! "$core" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    return 1
  fi
  local major minor patch
  # Scope IFS to this single read rather than tampering with it globally.
  IFS='.' read -r major minor patch <<<"$core"
  echo "$major $minor $patch"
}

baseline_raw="$(read_baseline_version)"
if [[ -z "$baseline_raw" ]]; then
  echo "FAIL: baseline file is empty: $BASELINE" >&2
  exit 2
fi

core_raw="$(read_core_version)"
if [[ -z "$core_raw" ]]; then
  echo "FAIL: no [workspace.package] version found in $CORE_MANIFEST" >&2
  exit 2
fi

if ! baseline_parts="$(parse_semver "$baseline_raw")"; then
  echo "FAIL: malformed baseline version '$baseline_raw' in $BASELINE (expected X.Y.Z)" >&2
  exit 2
fi
if ! core_parts="$(parse_semver "$core_raw")"; then
  echo "FAIL: malformed neat-core version '$core_raw' in $CORE_MANIFEST (expected X.Y.Z)" >&2
  exit 2
fi

read -r b_major b_minor b_patch <<<"$baseline_parts"
read -r c_major c_minor c_patch <<<"$core_parts"
: "$b_patch" "$c_patch"  # patch components are informational only

remediation() {
  cat >&2 <<EOF
       neat-core has presented a breaking bump Rebase has not handled.
       To clear this gate, in a single deliberate PR:
         1. Update forests for the breaking neat-core change.
         2. Bump the recorded baseline in neat-core.expected-version to $core_raw.
EOF
}

# Breaking-bump decision (SemVer): the major signals breaking changes once a
# crate reaches 1.0; before that, the minor carries that role.
if (( c_major > b_major )); then
  echo "FAIL: breaking neat-core bump: $core_raw exceeds handled baseline $baseline_raw (major increased)" >&2
  remediation
  exit 1
fi

if (( c_major == b_major )); then
  if (( b_major == 0 )); then
    # Pre-1.0: the minor is the breaking component.
    if (( c_minor > b_minor )); then
      echo "FAIL: breaking neat-core bump: $core_raw exceeds handled baseline $baseline_raw (pre-1.0 minor increased)" >&2
      remediation
      exit 1
    fi
    if (( c_minor < b_minor )); then
      echo "OK   neat-core $core_raw is behind handled baseline $baseline_raw (no breaking bump)"
      exit 0
    fi
    echo "OK   neat-core $core_raw matches handled baseline $baseline_raw (patch-level drift allowed)"
    exit 0
  fi
  # >= 1.0: same major — minor/patch drift is additive, never breaking.
  echo "OK   neat-core $core_raw within handled baseline $baseline_raw major line (patch-level drift allowed)"
  exit 0
fi

# c_major < b_major: neat-core sits below the baseline major — not a breaking
# bump Rebase needs to act on.
echo "OK   neat-core $core_raw is behind handled baseline $baseline_raw (no breaking bump)"
exit 0
