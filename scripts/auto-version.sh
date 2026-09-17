#!/usr/bin/env bash
# auto-version.sh — keep the crate version moving so unattended machines rebuild.
#
# The unattended machines key their rebuild off the crate version: a binary is
# rebuilt only when the version recorded beside it differs from the version in
# the manifest (the contract NEAT-AI-Discovery's `scripts/runlib.sh` uses). A PR
# that changes code without changing the version therefore ships nothing — the
# machines keep running the stale binary. CI runs this script on every PR so the
# bump happens whether or not the author remembered it (Issue #45).
#
# Usage:
#   auto-version.sh --print <manifest>
#       Print the [package] version declared in <manifest>.
#
#   auto-version.sh <manifest> <base-version> [lockfile]
#       Compare <manifest>'s version with the base branch's <base-version>:
#         head < base  → fail loud; a downgrade reuses a version the machines
#                        have already built, so the new binary never installs
#         head > base  → leave it alone; this PR has already bumped it
#         head == base → bump the patch in <manifest>, and in <lockfile> when
#                        one is given
#       The effective version is printed to stdout either way.

set -euo pipefail

die() {
  echo "auto-version.sh: $1" >&2
  exit 1
}

# Echo the first `<key> = "<value>"` of the manifest's leading [package] table.
manifest_field() {
  local file="$1" key="$2" value
  [ -f "$file" ] || die "no such manifest: $file"
  value="$(awk -v key="$key" '
    $0 ~ "^" key " = \"" { sub("^" key " = \"", ""); sub("\"$", ""); print; exit }
  ' "$file")"
  [ -n "$value" ] || die "no [package] $key in $file"
  printf '%s\n' "$value"
}

require_semver() {
  printf '%s' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
    || die "malformed version (expected x.y.z): $1"
}

# Echo 1, 0 or -1 for $1 greater than, equal to, or less than $2.
version_cmp() {
  local a_major a_minor a_patch b_major b_minor b_patch
  IFS='.' read -r a_major a_minor a_patch <<<"$1"
  IFS='.' read -r b_major b_minor b_patch <<<"$2"
  local -a a=("$a_major" "$a_minor" "$a_patch")
  local -a b=("$b_major" "$b_minor" "$b_patch")
  local i
  for i in 0 1 2; do
    if [ "${a[$i]}" -gt "${b[$i]}" ]; then
      echo 1
      return
    fi
    if [ "${a[$i]}" -lt "${b[$i]}" ]; then
      echo -1
      return
    fi
  done
  echo 0
}

# Rewrite the version of a single [[package]] entry in a Cargo.lock.
update_lockfile() {
  local lock="$1" pkg="$2" new="$3" tmp
  [ -f "$lock" ] || die "no such lockfile: $lock"
  tmp="$(mktemp)"
  awk -v pkg="$pkg" -v new="$new" '
    /^\[\[package\]\]/ { in_pkg = 0 }
    $0 == "name = \"" pkg "\"" { in_pkg = 1 }
    in_pkg && /^version = / { $0 = "version = \"" new "\""; in_pkg = 0; hits++ }
    { print }
    END { if (hits != 1) exit 1 }
  ' "$lock" >"$tmp" || {
    rm -f "$tmp"
    die "expected exactly one [[package]] entry for '$pkg' in $lock"
  }
  mv "$tmp" "$lock"
}

if [ "${1:-}" = "--print" ]; then
  [ "$#" -eq 2 ] || die "usage: auto-version.sh --print <manifest>"
  version="$(manifest_field "$2" version)"
  require_semver "$version"
  printf '%s\n' "$version"
  exit 0
fi

[ "$#" -eq 2 ] || [ "$#" -eq 3 ] \
  || die "usage: auto-version.sh <manifest> <base-version> [lockfile]"

manifest="$1"
base="${2#v}"
lockfile="${3:-}"

current="$(manifest_field "$manifest" version)"
package="$(manifest_field "$manifest" name)"
require_semver "$base"
require_semver "$current"

case "$(version_cmp "$current" "$base")" in
  -1)
    die "version downgraded: $base -> $current ($manifest must never go backwards vs the base branch — the machines rebuild off this version)"
    ;;
  1)
    echo "auto-version.sh: already ahead of the base branch ($base -> $current) — no bump needed" >&2
    printf '%s\n' "$current"
    exit 0
    ;;
esac

IFS='.' read -r major minor patch <<<"$current"
new="$major.$minor.$((patch + 1))"

tmp="$(mktemp)"
awk -v old="$current" -v new="$new" '
  !done && $0 == "version = \"" old "\"" { $0 = "version = \"" new "\""; done = 1 }
  { print }
  END { if (!done) exit 1 }
' "$manifest" >"$tmp" || {
  rm -f "$tmp"
  die "could not rewrite the [package] version in $manifest"
}
mv "$tmp" "$manifest"

if [ -n "$lockfile" ]; then
  update_lockfile "$lockfile" "$package" "$new"
fi

echo "auto-version.sh: bumped $package $current -> $new" >&2
printf '%s\n' "$new"
