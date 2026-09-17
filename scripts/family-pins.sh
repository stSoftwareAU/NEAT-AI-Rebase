#!/usr/bin/env bash
# family-pins.sh — move this checkout's NEAT-AI family git-tag pins to the
# latest release (Issue #681).
#
# COPY CONTRACT: this file has one home — `scripts/family-pins.sh` on
# NEAT-AI-core `Develop`. Every family repository copies it byte-for-byte into
# its own `scripts/family-pins.sh`; behaviour changes are made *here* and
# re-copied outward, never edited downstream, exactly as `runlib.sh` is. See
# README.md → "Canonical family-pins.sh".
#
# Run it from the repository root of a consumer. It:
#
#   * reads the root manifest and every `[workspace] members` manifest (or the
#     manifests named with --manifest) and finds each dependency declared as
#     `{ git = "https://github.com/stSoftwareAU/NEAT-AI-<x>", tag = "v<semver>" }`
#     — the inline form and the `[dependencies.<name>]` table form both;
#   * resolves that repository's newest released `v<major>.<minor>.<patch>` tag
#     with `git ls-remote --tags` (the family repositories are public, so no
#     credential is needed) and ignores pre-release tags such as `v1.0.0-rc1`;
#   * rewrites the pin in place when the remote carries a newer release, and
#     runs `cargo update --package <dep>` so `Cargo.lock` follows;
#   * prints one stderr line per pin moved and nothing on stdout.
#
# It is idempotent: a pin already on the newest release is left byte-for-byte
# alone and the run exits 0 having changed nothing.
#
# Usage:
#   family-pins.sh [--manifest FILE]... [-h|--help]
#
#   --manifest FILE  Scan FILE instead of the manifests discovered from the
#                    repository root. Repeatable.
#
# Exit codes:
#   0  every family pin is on the newest release (moved, or already there)
#   1  a remote could not be listed, a manifest could not be rewritten, or
#      `cargo update` failed — the run fails loud rather than leaving a pin
#      silently stale
#   2  usage error
set -euo pipefail

# A POSIX ERE with no backslash escapes: it is handed to awk through -v, which
# processes escapes in the value, so `[.]` — not `\.` — is what survives as a
# literal dot in both the awk and the bash reading of this pattern.
FAMILY_URL_RE='^https://github[.]com/stSoftwareAU/NEAT-AI-[A-Za-z0-9_.-]+$'

_fp_die() {
  printf 'family-pins: %s\n' "$*" >&2
  exit 1
}

_fp_usage() {
  sed -n '2,/^set -euo pipefail/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
}

# The `members` entries of the root manifest $1, one per line. Returns awk's
# own status: a reader that failed must not read as "this workspace has no
# members", which would leave every member's pin silently stale.
#
# This reader is adapted from the one in the canonical `runlib.sh`: both
# scripts are copied byte-for-byte into every sibling, so sharing a third file
# would make each copy depend on the other arriving too.
_fp_workspace_members() {
  [[ -f "$1" ]] || return 0
  awk '
    { line = $0; sub(/[[:space:]]*#.*$/, "", line) }
    line ~ /^[[:space:]]*\[/ {
      hdr = line
      gsub(/[[:space:]]/, "", hdr)
      in_ws = (hdr == "[workspace]") ? 1 : 0
      collecting = 0
      next
    }
    !in_ws { next }
    !collecting {
      k = line
      sub(/=.*$/, "", k)
      gsub(/[[:space:]]/, "", k)
      if (k != "members") next
      sub(/^[^=]*=/, "", line)
      collecting = 1
    }
    collecting {
      buf = buf " " line
      if (index(line, "]") > 0) collecting = 0
    }
    END {
      if (buf == "") exit 0
      n = split(buf, parts, /"/)
      for (i = 2; i <= n; i += 2) print parts[i]
    }
  ' "$1"
}

# Every family git-tag pin in manifest $1, one `<lineno>\t<dep>\t<url>\t<tag>`
# record per pin. The line number is the line carrying the `tag = "v…"` to
# rewrite, so the table form and the inline form are rewritten the same way.
#
# A commented-out declaration is not a pin: any line whose first non-blank
# character is `#` is skipped, so a pin left behind in a comment is never
# rewritten. A family pin spread over several lines is refused loudly — the
# reader stops with a message naming the file and the line, and the run exits 1
# — rather than passed over: a pin this reader cannot rewrite must not read as
# "already current".
_fp_pins() {
  awk -v family="$FAMILY_URL_RE" '
    function value_of(s, key,   re, rest, q) {
      re = "(^|[[:space:],{])" key "[[:space:]]*=[[:space:]]*\""
      if (!match(s, re)) return ""
      rest = substr(s, RSTART + RLENGTH)
      q = index(rest, "\"")
      if (q == 0) return ""
      return substr(rest, 1, q - 1)
    }
    function reset_table() { t_name = ""; t_url = ""; t_tag = ""; t_line = 0 }
    function flush_table() {
      if (t_name != "" && t_url ~ family && t_line > 0)
        printf "%d\t%s\t%s\t%s\n", t_line, t_name, t_url, t_tag
      reset_table()
    }
    BEGIN { reset_table() }
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*\[/ {
      flush_table()
      hdr = $0
      sub(/[[:space:]]*#.*$/, "", hdr)
      gsub(/[[:space:]]/, "", hdr)
      if (hdr ~ /^\[[A-Za-z0-9_."'"'"'()=,.-]*dependencies\.[A-Za-z0-9_-]+\]$/) {
        t_name = hdr
        sub(/\]$/, "", t_name)
        sub(/^.*\./, "", t_name)
      }
      next
    }
    t_name != "" {
      v = value_of($0, "git")
      if (v != "") t_url = v
      v = value_of($0, "tag")
      if (v != "") { t_tag = v; t_line = NR }
    }
    /^[[:space:]]*[A-Za-z0-9_.-]+[[:space:]]*=[[:space:]]*\{/ {
      url = value_of($0, "git")
      if (url !~ family) next
      if (index($0, "}") == 0) {
        printf "family-pins: %s:%d: the pin on %s is spread over several lines — put it on one line so it can be rewritten\n", FILENAME, NR, url > "/dev/stderr"
        exit 4
      }
      tag = value_of($0, "tag")
      if (tag == "") next
      name = $0
      sub(/[[:space:]]*=.*$/, "", name)
      gsub(/[[:space:]]/, "", name)
      printf "%d\t%s\t%s\t%s\n", NR, name, url, tag
    }
    END { flush_table() }
  ' "$1"
}

# Newest released tag at remote $1 — `v<major>.<minor>.<patch>` only, so a
# pre-release (`v1.0.0-rc1`) is never what a pin is moved onto. The answer
# lands in _FP_LATEST and a failure is a non-zero return, never a `$(…)` that
# dies: bash does not propagate an errexit abort out of a command substitution
# nested inside a function, so a resolver that printed its answer on stdout
# would hand the caller an empty string and a zero status — the silent failure
# this script exists to make impossible.
_fp_latest_release_tag() {
  local url="$1" listing tag line best=""
  _FP_LATEST=""
  if ! listing="$(git ls-remote --tags --refs "$url" 'v*' 2>&1)"; then
    printf 'family-pins: cannot list the tags of %s — %s\n' "$url" "$listing" >&2
    return 1
  fi
  while IFS= read -r line; do
    tag="${line##*refs/tags/}"
    [[ "$tag" != "$line" ]] || continue
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
    if [[ -z "$best" ]] || _fp_version_newer "$tag" "$best"; then
      best="$tag"
    fi
  done <<EOF
$listing
EOF
  if [[ -z "$best" ]]; then
    printf 'family-pins: %s carries no released v<major>.<minor>.<patch> tag to pin\n' "$url" >&2
    return 1
  fi
  _FP_LATEST="$best"
  return 0
}

# True when release tag $1 supersedes pin $2. Numeric triples decide it; on an
# equal triple a release supersedes a pre-release pin (v1.2.3 > v1.2.3-rc1).
_fp_version_newer() {
  local left="$1" right="$2" i x y
  local -a a b
  local IFS='.'
  read -r -a a <<<"${left#v}"
  read -r -a b <<<"${right#v}"
  for ((i = 0; i < 3; i++)); do
    x="${a[i]:-0}"
    y="${b[i]:-0}"
    # A non-numeric component (a pre-release tail, a typo) must not become an
    # arithmetic crash under `set -u`.
    x="${x%%-*}"
    y="${y%%-*}"
    case "$x" in '' | *[!0-9]*) x=0 ;; esac
    case "$y" in '' | *[!0-9]*) y=0 ;; esac
    if ((10#$x > 10#$y)); then return 0; fi
    if ((10#$x < 10#$y)); then return 1; fi
  done
  case "$right" in *-*) return 0 ;; esac
  return 1
}

# Replace the first `"<old>"` on line $2 of manifest $1 with `"<new>"`. The
# rewrite is staged and the result re-read: a manifest that did not actually
# move fails the run rather than leaving a stale pin behind a success message.
_fp_rewrite_tag() {
  local file="$1" lineno="$2" old="$3" new="$4" staged status=0
  staged="$file.family-pins.$$"
  awk -v n="$lineno" -v old="$old" -v new="$new" '
    NR == n {
      needle = "\"" old "\""
      p = index($0, needle)
      if (p == 0) exit 3
      $0 = substr($0, 1, p - 1) "\"" new "\"" substr($0, p + length(needle))
    }
    { print }
  ' "$file" >"$staged" || status=$?
  if ((status != 0)); then
    rm -f "$staged"
    _fp_die "$file:$lineno no longer carries the pin \"$old\" — refusing to rewrite it"
  fi
  mv -f "$staged" "$file" ||
    { rm -f "$staged"; _fp_die "could not rewrite $file"; }
  awk -v n="$lineno" -v new="$new" 'NR == n && index($0, "\"" new "\"") > 0 { found = 1 } END { exit found ? 0 : 1 }' "$file" ||
    _fp_die "$file:$lineno was not rewritten to \"$new\""
  return 0
}

# --- arguments ---------------------------------------------------------------
manifests=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --manifest)
      manifests+=("${2:?--manifest needs a path}")
      shift 2
      ;;
    -h | --help)
      _fp_usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      _fp_usage >&2
      exit 2
      ;;
  esac
done

REPO_ROOT="$PWD"
if ((${#manifests[@]} == 0)); then
  root_manifest="$REPO_ROOT/Cargo.toml"
  [[ -f "$root_manifest" ]] ||
    { echo "no Cargo.toml in $REPO_ROOT — run family-pins.sh from the repository root" >&2; exit 2; }
  manifests+=("$root_manifest")
  # Captured with an explicit status check, never straight into the heredoc: a
  # `$(…)` whose reader died is an empty member list and a run that reports
  # every member pin already current.
  members="$(_fp_workspace_members "$root_manifest")" ||
    _fp_die "could not read the [workspace] members of $root_manifest"
  while IFS= read -r member; do
    [[ -n "$member" ]] || continue
    # Unquoted on purpose: a `members` entry may be a glob (`crates/*`).
    # shellcheck disable=SC2086
    for candidate in "$REPO_ROOT"/$member/Cargo.toml; do
      [[ -f "$candidate" ]] || continue
      manifests+=("$candidate")
    done
  done <<EOF
$members
EOF
else
  for manifest in "${manifests[@]}"; do
    [[ -f "$manifest" ]] || { echo "manifest not found: $manifest" >&2; exit 2; }
  done
fi

# --- move every pin ----------------------------------------------------------
# bash 3.2 has no associative arrays, so the resolved-tag cache and the
# moved-dependency list are parallel arrays walked linearly.
_FP_LATEST=""
cache_urls=()
cache_tags=()
moved_deps=()
moved=0

# Resolve remote $1 once per run, leaving the answer in _FP_LATEST. Returns
# non-zero when the remote could not be listed.
_fp_resolved_tag() {
  local url="$1" i
  for ((i = 0; i < ${#cache_urls[@]}; i++)); do
    if [[ "${cache_urls[i]}" == "$url" ]]; then
      _FP_LATEST="${cache_tags[i]}"
      return 0
    fi
  done
  _fp_latest_release_tag "$url" || return 1
  cache_urls+=("$url")
  cache_tags+=("$_FP_LATEST")
  return 0
}

_fp_remember_moved() {
  local dep="$1" known
  for known in ${moved_deps[@]+"${moved_deps[@]}"}; do
    [[ "$known" != "$dep" ]] || return 0
  done
  moved_deps+=("$dep")
}

for manifest in "${manifests[@]}"; do
  pins="$(_fp_pins "$manifest")" ||
    _fp_die "could not read the family pins in $manifest"
  [[ -n "$pins" ]] || continue
  while IFS="$(printf '\t')" read -r lineno dep url tag; do
    [[ -n "${lineno:-}" ]] || continue
    _fp_resolved_tag "$url" || exit 1
    latest="$_FP_LATEST"
    if ! _fp_version_newer "$latest" "$tag"; then
      continue
    fi
    _fp_rewrite_tag "$manifest" "$lineno" "$tag" "$latest"
    printf '[family-pins] %s %s → %s (%s)\n' \
      "$dep" "$tag" "$latest" "${manifest#"$REPO_ROOT"/}" >&2
    _fp_remember_moved "$dep"
    moved=$((moved + 1))
  done <<EOF
$pins
EOF
done

if ((moved == 0)); then
  exit 0
fi

# --- let Cargo.lock follow ---------------------------------------------------
command -v cargo >/dev/null 2>&1 ||
  _fp_die "cargo not found — install the Rust toolchain from https://rustup.rs and re-run"
for dep in "${moved_deps[@]}"; do
  if ! output="$(cd "$REPO_ROOT" && cargo update --package "$dep" 2>&1)"; then
    printf '%s\n' "$output" >&2
    _fp_die "cargo update --package $dep failed — Cargo.lock does not match the moved pin"
  fi
done
printf '[family-pins] %d pin(s) moved; Cargo.lock updated\n' "$moved" >&2
