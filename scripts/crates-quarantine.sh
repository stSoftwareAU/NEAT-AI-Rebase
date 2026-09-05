#!/usr/bin/env bash
# Publish-age quarantine for automated Cargo bumps (Issue #91).
#
# `.github/workflows/cargo-upgrade.yml` is this repository's only dependency
# update mechanism. `cargo deny check` and the test suite judge licences,
# advisories and behaviour — none of them judge *recency*, so a crate version
# published minutes ago by a compromised publishing account could be proposed
# for merge before crates.io or RustSec has had a chance to flag it. This gate
# closes that window: every crates.io version the bump newly resolves must have
# been public for at least the quarantine period before the pull request is
# opened.
#
# Mechanism:
#   * Read `[[package]]` entries out of the refreshed `Cargo.lock`, keeping
#     only those resolved from a registry source. `path` dependencies
#     (`neat-core`) and `git` dependencies carry no crates.io publish date and
#     are reported as skipped, never silently passed.
#   * Subtract the name+version pairs the baseline lockfile already had, so a
#     dependency the bump did not move is not re-judged every week.
#   * Ask crates.io for each remaining version's `created_at` and fail when the
#     version has been public for less than the quarantine window.
#
# Internal `stSoftwareAU` crates are exempt via `--exempt` (none are consumed
# from crates.io today — `neat-core` is a `path` dependency — but the flag is
# the seam for when one is).
#
# Usage:
#   crates-quarantine.sh [--lockfile PATH] [--baseline-lockfile PATH]
#                        [--hours N] [--now RFC3339] [--api-base URL]
#                        [--exempt GLOB]...
#
# Exit codes:
#   0  every newly resolved registry version is outside the quarantine window
#   1  at least one version is younger than the window — gate fails
#   2  usage / parse / fetch error (a fault is never reported as a pass)
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: crates-quarantine.sh [options]

Options:
  --lockfile PATH            Refreshed lockfile to judge (default: Cargo.lock
                             at the repository root).
  --baseline-lockfile PATH   Lockfile from before the bump; versions present
                             here are already in the tree and are not judged.
                             Omit to judge every registry version.
  --hours N                  Quarantine window in hours (default: 24).
  --now RFC3339              Instant to measure age against (default: now).
                             Must be UTC: "...Z" or "...+00:00".
  --api-base URL             crates.io API base (default:
                             https://crates.io/api/v1).
  --exempt GLOB              Crate-name glob exempt from the window; repeatable
                             (e.g. --exempt 'stsoftware-*').
  -h, --help                 Show this message.

Exits 0 when clear, 1 on a quarantined version, 2 on a usage or fetch error.
EOF
}

LOCKFILE=""
BASELINE_LOCKFILE=""
HOURS="24"
NOW=""
API_BASE="https://crates.io/api/v1"
EXEMPT=()
USER_AGENT="NEAT-AI-Rebase-crates-quarantine (https://github.com/stSoftwareAU/NEAT-AI-Rebase)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --lockfile)
      [[ $# -ge 2 ]] || { echo "Missing value for --lockfile" >&2; usage >&2; exit 2; }
      LOCKFILE="$2"; shift 2 ;;
    --baseline-lockfile)
      [[ $# -ge 2 ]] || { echo "Missing value for --baseline-lockfile" >&2; usage >&2; exit 2; }
      BASELINE_LOCKFILE="$2"; shift 2 ;;
    --hours)
      [[ $# -ge 2 ]] || { echo "Missing value for --hours" >&2; usage >&2; exit 2; }
      HOURS="$2"; shift 2 ;;
    --now)
      [[ $# -ge 2 ]] || { echo "Missing value for --now" >&2; usage >&2; exit 2; }
      NOW="$2"; shift 2 ;;
    --api-base)
      [[ $# -ge 2 ]] || { echo "Missing value for --api-base" >&2; usage >&2; exit 2; }
      API_BASE="${2%/}"; shift 2 ;;
    --exempt)
      [[ $# -ge 2 ]] || { echo "Missing value for --exempt" >&2; usage >&2; exit 2; }
      EXEMPT+=("$2"); shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ ! "$HOURS" =~ ^[0-9]+$ ]]; then
  echo "FAIL: --hours must be a whole number of hours, got '$HOURS'" >&2
  exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
if [[ -z "$LOCKFILE" ]]; then
  LOCKFILE="$REPO_ROOT/Cargo.lock"
fi

if [[ ! -f "$LOCKFILE" ]]; then
  echo "FAIL: lockfile not found: $LOCKFILE" >&2
  exit 2
fi
if [[ -n "$BASELINE_LOCKFILE" && ! -f "$BASELINE_LOCKFILE" ]]; then
  echo "FAIL: baseline lockfile not found: $BASELINE_LOCKFILE" >&2
  exit 2
fi
if ! command -v curl &>/dev/null; then
  echo "FAIL: curl is required to read publish dates from $API_BASE" >&2
  exit 2
fi

# Seconds since the Unix epoch for a UTC RFC 3339 stamp. Fractional seconds are
# dropped and a non-UTC offset is rejected: crates.io stamps every `created_at`
# in UTC, so anything else is a response this gate does not understand and must
# not guess at. Implemented in awk (Howard Hinnant's days-from-civil) rather
# than `date -d`, which is GNU-only — macOS bash 3.2 boxes run this too.
to_epoch() {
  local stamp="$1" naive
  if [[ "$stamp" == *Z ]]; then
    naive="${stamp%Z}"
  elif [[ "$stamp" == *+00:00 ]]; then
    naive="${stamp%+00:00}"
  else
    return 1 # no UTC designator — reject rather than assume a zone.
  fi
  naive="${naive%.*}" # drop fractional seconds, if any
  if [[ ! "$naive" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}$ ]]; then
    return 1
  fi
  awk -v s="$naive" '
    function days_from_civil(y, m, d,   era, yoe, doy, doe) {
      if (m <= 2) { y -= 1 }
      era = int((y >= 0 ? y : y - 399) / 400)
      yoe = y - era * 400
      doy = int((153 * (m + (m > 2 ? -3 : 9)) + 2) / 5) + d - 1
      doe = yoe * 365 + int(yoe / 4) - int(yoe / 100) + doy
      return era * 146097 + doe - 719468
    }
    BEGIN {
      split(s, parts, "T")
      split(parts[1], date, "-")
      split(parts[2], time, ":")
      print days_from_civil(date[1] + 0, date[2] + 0, date[3] + 0) * 86400 \
            + time[1] * 3600 + time[2] * 60 + time[3]
    }
  '
}

# `name version` for every package resolved from a registry, one per line.
# Packages with no `source` are `path` dependencies; `git+` sources carry no
# crates.io publish date. Both are reported separately by the caller.
registry_packages() {
  awk '
    /^\[\[package\]\]/ { name = ""; version = ""; source = ""; next }
    /^name = / { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name) }
    /^version = / { version = $0; sub(/^version = "/, "", version); sub(/"$/, "", version) }
    /^source = / { source = $0; sub(/^source = "/, "", source); sub(/"$/, "", source) }
    /^$/ { if (name != "" && version != "" && source ~ /^registry\+/) { print name, version } ; name = ""; version = ""; source = "" }
    END { if (name != "" && version != "" && source ~ /^registry\+/) { print name, version } }
  ' "$1"
}

# Every `name version` pair in a lockfile, registry or not — the baseline only
# needs to answer "was this exact version already here?".
all_packages() {
  [[ -f "$1" ]] || return 0
  awk '
    /^\[\[package\]\]/ { name = ""; version = ""; next }
    /^name = / { name = $0; sub(/^name = "/, "", name); sub(/"$/, "", name) }
    /^version = / { version = $0; sub(/^version = "/, "", version); sub(/"$/, "", version) }
    /^$/ { if (name != "" && version != "") { print name, version } ; name = ""; version = "" }
    END { if (name != "" && version != "") { print name, version } }
  ' "$1"
}

is_exempt() {
  local name="$1" pattern
  for pattern in ${EXEMPT[@]+"${EXEMPT[@]}"}; do
    # shellcheck disable=SC2053 # glob match is the point of --exempt.
    if [[ "$name" == $pattern ]]; then
      return 0
    fi
  done
  return 1
}

if [[ -n "$NOW" ]]; then
  if ! now_epoch="$(to_epoch "$NOW")"; then
    echo "FAIL: --now must be a UTC RFC 3339 stamp (e.g. 2026-01-01T00:00:00Z), got '$NOW'" >&2
    exit 2
  fi
else
  now_epoch="$(date -u +%s)"
fi

window_seconds=$((HOURS * 3600))
baseline="$(all_packages "$BASELINE_LOCKFILE")"

judged=0
quarantined=0
while read -r name version; do
  [[ -n "$name" ]] || continue

  # The lockfile is an input like any other: a name or version carrying a
  # slash, a query string or whitespace would rewrite the URL below, so both
  # are matched against the character sets crates.io actually allows.
  if [[ ! "$name" =~ ^[A-Za-z0-9_-]+$ ]] || [[ ! "$version" =~ ^[A-Za-z0-9_.+-]+$ ]]; then
    echo "FAIL: refusing to query crates.io for implausible package '$name' '$version'" >&2
    exit 2
  fi

  if printf '%s\n' "$baseline" | grep -qxF "$name $version"; then
    continue # already in the tree before the bump — not this bump's risk.
  fi
  if is_exempt "$name"; then
    echo "EXEMPT     $name $version (matches an --exempt pattern)"
    continue
  fi

  judged=$((judged + 1))
  url="$API_BASE/crates/$name/$version"
  # crates.io's crawler policy rejects an anonymous client with 403, so the
  # request identifies this gate and where to complain about it.
  if ! response="$(curl --fail --silent --show-error --location --max-time 30 \
    --user-agent "$USER_AGENT" "$url" 2>&1 </dev/null)"; then
    echo "FAIL: could not read publish date for $name $version from $url" >&2
    echo "      $response" >&2
    exit 2
  fi

  # The version endpoint wraps a single version object, so the first
  # `created_at` in the body is that version's publication instant.
  created_at="$(printf '%s' "$response" \
    | grep -o '"created_at"[[:space:]]*:[[:space:]]*"[^"]*"' \
    | head -n 1 \
    | sed 's/.*"\([^"]*\)"$/\1/')"
  if [[ -z "$created_at" ]]; then
    echo "FAIL: no created_at in the crates.io response for $name $version" >&2
    exit 2
  fi
  if ! created_epoch="$(to_epoch "$created_at")"; then
    echo "FAIL: unparseable created_at '$created_at' for $name $version" >&2
    exit 2
  fi

  # A future stamp yields a negative age and quarantines: clock skew and a
  # tampered response both fail closed.
  age=$((now_epoch - created_epoch))
  age_hours=$((age / 3600))
  if (( age < window_seconds )); then
    quarantined=$((quarantined + 1))
    echo "QUARANTINED $name $version published ${age_hours}h ago, under the ${HOURS}h window" >&2
  else
    echo "OK         $name $version published ${age_hours}h ago"
  fi
done < <(registry_packages "$LOCKFILE")

if (( quarantined > 0 )); then
  cat >&2 <<EOF
FAIL: $quarantined newly resolved crate version(s) have been public for less than ${HOURS}h.
      A version this fresh has not been exposed to crates.io/RustSec scrutiny yet,
      so this bump is not proposed. Re-run the workflow once the window has passed,
      or pin the crate deliberately in a hand-written PR.
EOF
  exit 1
fi

echo "OK   $judged newly resolved crates.io version(s) are all older than ${HOURS}h"
exit 0
