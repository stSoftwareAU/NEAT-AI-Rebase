#!/usr/bin/env bash
# Canonical build → install → clean helper for the NEAT-AI Rust siblings.
#
# COPY CONTRACT (Issue #680): this file has one home — `scripts/runlib.sh` on
# NEAT-AI-core `Develop`. Every Rust sibling copies it byte-for-byte into its
# own `scripts/runlib.sh`; behaviour changes are made *here* and re-copied
# outward, never edited downstream. See README.md → "Canonical runlib.sh".
#
# Run it from the repository root of the sibling, or source it and call
# `runlib_install`. With no argument it:
#
#   * resolves the single workspace member — the root crate, or the one
#     `[workspace] members` entry — and fails loud on zero or more than one;
#   * installs the bin target named after the crate to `$CARGO_HOME/bin/<crate>`
#     and a `cdylib` target to `$CARGO_HOME/lib/lib<crate>.{so,dylib}`, both
#     with `-` → `_` in the crate name (CARGO_HOME defaults to `~/.cargo`). A
#     crate carrying both targets installs both. The installed names come from
#     the *crate*, never from the cargo target name, so the skip below looks
#     the artefacts up under the same names the install wrote;
#   * writes the stamp `.<crate>.version` — the crate semver — beside every
#     artefact it installs, and writes it *last*;
#   * runs **no** `cargo` command at all when artefact and stamp already match,
#     printing exactly one stderr line `[<crate>] already installed v<x>`.
#     There is no force flag: delete the stamp to force a rebuild;
#   * removes the checkout's `target/` after a successful install and names the
#     path removed and the bytes freed on stderr; a build directory outside the
#     checkout (a shared `CARGO_TARGET_DIR`) is kept, and that is reported. Any
#     failure keeps `target/` and leaves the installed artefacts and their
#     stamps untouched — every artefact is staged and moved into place only
#     once all of them are ready;
#   * honours the caller's RUSTFLAGS unchanged and sets no flags of its own;
#   * prints the installed bin path — or the lib path when there is no bin —
#     on stdout, and nothing else on stdout.
#
# `--toolchain-only` (Issue #701) is the other entry point — sourced, that is
# `runlib_ensure_toolchain`. It runs the same chain (the rustup bootstrap
# below, the unrunnable-rustc repair, the resolved dependency graph and the
# toolchain gate), builds nothing and installs nothing, and prints the override
# toolchain the gate selected on stdout — a bare name such as `1.93.1`, or an
# empty line when the active toolchain already satisfies the requirement — and
# nothing else on stdout, for a caller that runs cargo itself and exports it as
# `RUSTUP_TOOLCHAIN`. The already-installed skip is never consulted: a caller
# running its own cargo needs a good toolchain whatever the stamp says. Every
# failure exits non-zero with install mode's stderr messages, and every other
# argument exits 2 with a one-line usage on stderr, having run no cargo at all.
#
# With no `rustc` on PATH the toolchain is bootstrapped here (Issue #699):
# rustup is installed from the pinned `rustup-init` for the host target, whose
# SHA-256 must match the digest inlined below before the downloaded file is
# executed. It is never `curl https://sh.rustup.rs | sh` — that runs whatever
# the distribution point served, and what it installs then compiles every
# `build.rs` in the dependency graph. Every verification failure — an unknown
# host target, no pinned digest, no SHA-256 tool, a failed download, a digest
# mismatch — exits non-zero naming the cause, having executed nothing. With
# `rustc` already present nothing is downloaded. `jq` stays a plain
# precondition: a missing one exits non-zero naming it.
#
# To bump rustup, change `_RUNLIB_RUSTUP_VERSION` and every digest in
# `_runlib_pinned_rustup_digest` together — they are one pin, and a version
# moved without its digests can only fail closed.
#
# The toolchain gate (Issue #700) runs on the build path only. The required
# version is the highest `rust-version` across the crate's resolved dependency
# graph — `cargo metadata --filter-platform <host>` — including the crate's
# own, because a dependency can demand a newer rustc than any family crate
# declares. A rustc at or above it passes with no `rustup` call at all, and is
# never downgraded. Below it, and with `rustup` on PATH: an unpinned crate gets
# `rustup update stable`, and a crate pinned to a *channel* — `stable`,
# `nightly`, or a two-part `1.93` rustup resolves to the newest 1.93.x — gets
# `rustup update <channel>`, because a moving pin is moved rather than swapped
# for an exact version. An *exact* `rust-toolchain.toml` pin below the
# requirement gets `rustup toolchain install <required>` plus a
# `RUSTUP_TOOLCHAIN` override handed to that one `cargo build` and never
# exported — the pin file is a repository commit and is left untouched, with
# one stderr line naming the bump. Below it *without* rustup — or still below
# after the update — exits non-zero naming the required version and
# https://rustup.rs; a distro toolchain is never replaced. Every value handed
# to rustup is validated as a plain toolchain name first: a
# `rust-toolchain.toml` channel and `cargo metadata` are repository input.
#
# Run it as a subprocess — `path="$(./scripts/runlib.sh)"`. It can also be
# sourced, but note that sourcing applies `set -euo pipefail` to the calling
# shell, prepends `$CARGO_HOME/bin` to its PATH, clears any EXIT/INT/TERM trap
# the caller had set, and that any failure exits that shell rather than
# returning to it.
set -euo pipefail

_runlib_die() {
  printf 'runlib: %s\n' "$*" >&2
  exit 1
}

# Cargo's home, honouring CARGO_HOME exactly as rustup and cargo do.
_runlib_cargo_home() {
  printf '%s' "${CARGO_HOME:-$HOME/.cargo}"
}

# Value of `key` inside `[section]` of the TOML file $1, unquoted. Prints
# nothing when the file, the section or the key is absent.
_runlib_toml_value() {
  local file="$1" section="$2" key="$3"
  [[ -f "$file" ]] || return 0
  awk -v want="[$section]" -v key="$key" '
    {
      line = $0
      sub(/[[:space:]]*#.*$/, "", line)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
    }
    line ~ /^\[/ {
      hdr = line
      gsub(/[[:space:]]/, "", hdr)
      cur = hdr
      next
    }
    cur != want { next }
    {
      eq = index(line, "=")
      if (eq == 0) next
      k = substr(line, 1, eq - 1)
      gsub(/[[:space:]]/, "", k)
      if (k != key) next
      v = substr(line, eq + 1)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", v)
      gsub(/^"|"$/, "", v)
      print v
      exit
    }
  ' "$file"
  return 0
}

# Every `name` a `[[bin]]` table of manifest $1 declares, one per line. A crate
# that ships a CLI beside bench binaries (NEAT-AI-scorer) declares several, and
# only the whole list answers "is one of them named after the crate?".
_runlib_bin_table_names() {
  [[ -f "$1" ]] || return 0
  awk '
    {
      line = $0
      sub(/[[:space:]]*#.*$/, "", line)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
    }
    line ~ /^\[/ {
      hdr = line
      gsub(/[[:space:]]/, "", hdr)
      in_bin = (hdr == "[[bin]]") ? 1 : 0
      next
    }
    !in_bin { next }
    {
      eq = index(line, "=")
      if (eq == 0) next
      k = substr(line, 1, eq - 1)
      gsub(/[[:space:]]/, "", k)
      if (k != "name") next
      v = substr(line, eq + 1)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", v)
      gsub(/^"|"$/, "", v)
      print v
    }
  ' "$1"
  return 0
}

# The `members` entries of the root manifest $1, one per line.
_runlib_workspace_members() {
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
  return 0
}

# Manifest path of the single workspace member, resolved without running cargo.
# Returns non-zero — silently — whenever the shape is anything but the two
# unambiguous ones, leaving `cargo metadata` on the build path to be the
# authority that fails loud.
_runlib_fast_member_manifest() {
  local repo_root="$1" root_manifest="$1/Cargo.toml"
  [[ -f "$root_manifest" ]] || return 1

  local has_package=0
  if grep -qE '^[[:space:]]*\[package\]' "$root_manifest"; then
    has_package=1
  fi

  local member count=0 first=""
  while IFS= read -r member; do
    [[ -n "$member" ]] || continue
    count=$((count + 1))
    first="$member"
  done <<EOF
$(_runlib_workspace_members "$root_manifest")
EOF

  if [[ "$has_package" -eq 1 && "$count" -eq 0 ]]; then
    printf '%s\n' "$root_manifest"
    return 0
  fi
  if [[ "$has_package" -eq 0 && "$count" -eq 1 ]]; then
    case "$first" in
      *'*'*) return 1 ;;
    esac
    [[ -f "$repo_root/$first/Cargo.toml" ]] || return 1
    printf '%s\n' "$repo_root/$first/Cargo.toml"
    return 0
  fi
  return 1
}

# A `[package]` field of manifest $1, following `<key>.workspace = true` into
# the `[workspace.package]` table of root manifest $2.
_runlib_crate_field() {
  local manifest="$1" root_manifest="$2" key="$3" value inherited
  value="$(_runlib_toml_value "$manifest" package "$key")"
  if [[ -z "$value" ]]; then
    inherited="$(_runlib_toml_value "$manifest" package "${key}.workspace")"
    if [[ "$inherited" == "true" ]]; then
      value="$(_runlib_toml_value "$root_manifest" workspace.package "$key")"
    fi
  fi
  printf '%s' "$value"
  return 0
}

# Shared-library extension for this platform.
_runlib_lib_extension() {
  case "$(uname -s)" in
    Darwin) printf 'dylib' ;;
    *) printf 'so' ;;
  esac
}

# Installed basenames. Both derive from the *crate* name with `-` -> `_`, never
# from the cargo target name: the skip path knows only the crate, so a crate
# whose `[lib] name` differs from its package name would otherwise be installed
# under one name and looked up under another — and rebuild on every run forever.
_runlib_bin_basename() {
  printf '%s' "${1//-/_}"
}

_runlib_lib_basename() {
  local extension
  extension="$(_runlib_lib_extension)"
  printf 'lib%s.%s' "${1//-/_}" "$extension"
}

# Returns 0 when semver $1 >= semver $2.
_runlib_version_ge() {
  local left="$1" right="$2" i x y
  local -a a b
  local IFS='.'
  read -r -a a <<< "${left%%-*}"
  read -r -a b <<< "${right%%-*}"
  for ((i = 0; i < ${#a[@]} || i < ${#b[@]}; i++)); do
    x="${a[i]:-0}"
    y="${b[i]:-0}"
    # A non-numeric component (a build-metadata tail, a typo in `rust-version`)
    # must not become an arithmetic crash under `set -u`.
    case "$x" in ''|*[!0-9]*) x=0 ;; esac
    case "$y" in ''|*[!0-9]*) y=0 ;; esac
    if ((10#$x > 10#$y)); then return 0; fi
    if ((10#$x < 10#$y)); then return 1; fi
  done
  return 0
}

# Sets _RUNLIB_EXPECTS_BIN / _RUNLIB_EXPECTS_LIB from manifest $1 alone, for
# the fast path, and returns non-zero when the manifest does not determine the
# shape beyond doubt.
#
# Declining is the whole point. An *under*-claimed target is not the cheap
# mistake it looks like: `_runlib_report_current` only checks the targets it
# was told to expect, so an unread cdylib makes a half-installed both-crate
# report "already installed" and a fleet host keeps a missing library for ever.
# Every shape this reader cannot decode therefore falls through to
# `cargo metadata`, which is the authority — one metadata call, never a wrong
# answer.
_runlib_expected_shape() {
  local manifest="$1" crate_underscored="$2" manifest_dir crate_types
  manifest_dir="$(dirname "$manifest")"
  _RUNLIB_EXPECTS_BIN=0
  _RUNLIB_EXPECTS_LIB=0

  # A cdylib is never implicit — it exists only where `[lib] crate-type` says
  # so. `_runlib_toml_value` reads a single line, so an array split over
  # several lines arrives as a bare `[`: unreadable, not absent.
  crate_types="$(_runlib_toml_value "$manifest" lib crate-type)"
  case "$crate_types" in
    *'['*']'*) : ;;
    *'['*) return 1 ;;
  esac
  case "$crate_types" in
    *cdylib*) _RUNLIB_EXPECTS_LIB=1 ;;
  esac

  # An explicit `[[bin]]` table, or `autobins`, can rename or suppress the
  # binary cargo would otherwise name after the package. A table that names
  # the crate is still unambiguous, though — it is the shape every sibling
  # shipping a CLI writes, whether alone (NEAT-AI-Backpropagation #152) or
  # beside bench binaries (NEAT-AI-scorer #629) — so read the declared names
  # rather than paying a `cargo metadata` call on every skip. Tables that name
  # only other binaries are not conclusive (cargo may still autodiscover a bin
  # named after the package beside them), and neither is an `autobins` key:
  # both fall through to `cargo metadata`.
  local bin_tables declared_bin
  bin_tables="$(grep -cE '^[[:space:]]*\[\[bin\]\]' "$manifest" || true)"
  if grep -qE '^[[:space:]]*autobins[[:space:]]*=' "$manifest"; then
    return 1
  fi
  if [[ "$bin_tables" -ge 1 ]]; then
    while IFS= read -r declared_bin; do
      [[ "$declared_bin" == "$crate_underscored" ]] || continue
      _RUNLIB_EXPECTS_BIN=1
      return 0
    done <<EOF
$(_runlib_bin_table_names "$manifest")
EOF
    return 1
  fi
  if [[ -f "$manifest_dir/src/main.rs" ||
    -f "$manifest_dir/src/bin/${crate_underscored}.rs" ]]; then
    _RUNLIB_EXPECTS_BIN=1
  fi
  return 0
}

# True when the stamp in directory $1 matches version $3 for crate $2 and the
# artefact $4 it stands for is still there. A stamp deleted beside a surviving
# artefact is the documented way to force a rebuild, so it reads as "not
# current" rather than as "nothing was ever installed here".
_runlib_stamp_current() {
  local dir="$1" crate="$2" version="$3" artefact="$4" stamp
  stamp="$dir/.${crate}.version"
  [[ -f "$stamp" ]] || return 1
  # `-f`, not `-e`: a *directory* left at the artefact path is corruption, and
  # `-e` would let it read as a valid install for ever.
  [[ -f "$artefact" ]] || return 1
  [[ "$(cat "$stamp")" == "$version" ]] || return 1
  return 0
}

# Prints the already-installed line and the installed path and returns 0 when
# *every* artefact the crate's shape calls for is present with a matching
# stamp; returns non-zero — silently — otherwise. Runs no cargo command.
#
# Checking the whole shape is what stops a crate that ships both a bin and a
# cdylib from reporting "already installed" once one half has been removed.
_runlib_report_current() {
  local crate="$1" version="$2" expects_bin="$3" expects_lib="$4"
  local bin_dir lib_dir bin_path lib_path
  bin_dir="$(_runlib_cargo_home)/bin"
  lib_dir="$(_runlib_cargo_home)/lib"
  bin_path="$bin_dir/$(_runlib_bin_basename "$crate")"
  lib_path="$lib_dir/$(_runlib_lib_basename "$crate")"

  # An undetermined shape claims nothing: rebuilding costs a build, reporting a
  # half-installed tree as complete costs a fleet host the wrong artefact.
  [[ "$expects_bin" -eq 1 || "$expects_lib" -eq 1 ]] || return 1

  if [[ "$expects_bin" -eq 1 ]]; then
    _runlib_stamp_current "$bin_dir" "$crate" "$version" "$bin_path" || return 1
  fi
  if [[ "$expects_lib" -eq 1 ]]; then
    _runlib_stamp_current "$lib_dir" "$crate" "$version" "$lib_path" || return 1
  fi

  printf '[%s] already installed v%s\n' "$crate" "$version" >&2
  if [[ "$expects_bin" -eq 1 ]]; then
    printf '%s\n' "$bin_path"
  else
    printf '%s\n' "$lib_path"
  fi
  return 0
}

# The fast path: resolve the crate from the manifests alone and report an
# up-to-date install without running cargo at all. Returns non-zero — silently
# — whenever the repository shape is anything but the two unambiguous ones,
# leaving `cargo metadata` on the build path to be the authority.
_runlib_try_skip() {
  local repo_root="$1" root_manifest="$2" manifest crate version
  manifest="$(_runlib_fast_member_manifest "$repo_root")" || return 1
  crate="$(_runlib_crate_field "$manifest" "$root_manifest" name)"
  version="$(_runlib_crate_field "$manifest" "$root_manifest" version)"
  [[ -n "$crate" && -n "$version" ]] || return 1
  _runlib_expected_shape "$manifest" "${crate//-/_}" || return 1
  _runlib_report_current "$crate" "$version" \
    "$_RUNLIB_EXPECTS_BIN" "$_RUNLIB_EXPECTS_LIB" || return 1
  return 0
}

# The pinned rustup installer (Issue #699). Bump the version and every digest
# below together — `_runlib_pinned_rustup_digest` is the whole pin, and a
# version moved on its own can only fail closed on the mismatch.
_RUNLIB_RUSTUP_VERSION="1.29.0"
_RUNLIB_RUSTUP_BASE_URL="https://static.rust-lang.org/rustup/archive"

# The published SHA-256 of `rustup-init` $_RUNLIB_RUSTUP_VERSION for target $1,
# as served at <base>/<version>/<target>/rustup-init.sha256. Prints nothing and
# returns non-zero for a target with no pinned digest — a refusal to install,
# never a licence to skip the check.
_runlib_pinned_rustup_digest() {
  case "$1" in
    x86_64-unknown-linux-gnu)
      printf '%s' '4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10' ;;
    aarch64-unknown-linux-gnu)
      printf '%s' '9732d6c5e2a098d3521fca8145d826ae0aaa067ef2385ead08e6feac88fa5792' ;;
    x86_64-unknown-linux-musl)
      printf '%s' '9cd3fda5fd293890e36ab271af6a786ee22084b5f6c2b83fd8323cec6f0992c1' ;;
    aarch64-unknown-linux-musl)
      printf '%s' '88761caacddb92cd79b0b1f939f3990ba1997d701a38b3e8dd6746a562f2a759' ;;
    x86_64-apple-darwin)
      printf '%s' '33cf85df9142bc6d29cbc62fa5ca1d4c29622cddb55213a4c1a43c457fb9b2d7' ;;
    aarch64-apple-darwin)
      printf '%s' 'aeb4105778ca1bd3c6b0e75768f581c656633cd51368fa61289b6a71696ac7e1' ;;
    *) return 1 ;;
  esac
  return 0
}

# The rustup target triple for this host. Returns non-zero — silently — for an
# operating system or architecture with no pinned installer; the caller names
# what it saw.
_runlib_host_target() {
  local os arch libc ldd_version
  case "$(uname -s)" in
    Linux) os="unknown-linux" ;;
    Darwin) os="apple-darwin" ;;
    *) return 1 ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    arm64 | aarch64) arch="aarch64" ;;
    *) return 1 ;;
  esac
  if [[ "$os" != "unknown-linux" ]]; then
    printf '%s-%s' "$arch" "$os"
    return 0
  fi
  # `ldd --version` exits non-zero on musl, so its output is captured first:
  # piping it straight into grep under `pipefail` reports the failure rather
  # than the match, and a musl host would silently take the gnu installer.
  libc="gnu"
  ldd_version=""
  if command -v ldd >/dev/null 2>&1; then
    ldd_version="$(ldd --version 2>&1 || true)"
  fi
  if printf '%s' "$ldd_version" | grep -qi musl; then
    libc="musl"
  fi
  printf '%s-%s-%s' "$arch" "$os" "$libc"
  return 0
}

# The SHA-256 command this host provides, as a bare word. Returns non-zero —
# silently — when it has neither; both callers below refuse outright rather
# than guess, because no verification means no install.
_runlib_sha256_tool() {
  if command -v sha256sum >/dev/null 2>&1; then
    printf 'sha256sum'
  elif command -v shasum >/dev/null 2>&1; then
    printf 'shasum'
  else
    return 1
  fi
  return 0
}

# The SHA-256 of file $1 as lower-case hex. With no digest tool it dies rather
# than returning something the comparison could mistake for a digest. It dies
# from inside the caller's command substitution: that exits the substitution's
# subshell non-zero, which under `set -e` aborts the script — so the caller's
# assignment must not be a `local` declaration, which would swallow the status.
_runlib_sha256_of() {
  local file="$1" tool
  tool="$(_runlib_sha256_tool)" ||
    _runlib_die "no SHA-256 tool (sha256sum or shasum) on PATH — refusing to run an unverified rustup-init; install the Rust toolchain from https://rustup.rs and re-run"
  case "$tool" in
    sha256sum) sha256sum "$file" | awk 'NR == 1 { print $1 }' ;;
    shasum) shasum -a 256 "$file" | awk 'NR == 1 { print $1 }' ;;
  esac
  return 0
}

# Install rustup, and with it a minimal `stable`, from the pinned installer.
# The download lands in a temporary directory the cleanup trap reaps, and is
# executed only once its digest matches the pin above.
_runlib_bootstrap_rustup() {
  local target url tmp_dir installer expected actual

  if ! target="$(_runlib_host_target)"; then
    _runlib_die "no pinned rustup-init for $(uname -s)/$(uname -m) — install the Rust toolchain from https://rustup.rs and re-run"
  fi
  if ! expected="$(_runlib_pinned_rustup_digest "$target")"; then
    _runlib_die "no pinned rustup-init digest for the host target $target — install the Rust toolchain from https://rustup.rs and re-run"
  fi
  # Checked before anything is fetched: an installer that cannot be verified
  # is one this script will not run, so downloading it first would only make
  # the refusal look like a network problem.
  _runlib_sha256_tool >/dev/null ||
    _runlib_die "no SHA-256 tool (sha256sum or shasum) on PATH — refusing to download an unverifiable rustup-init; install the Rust toolchain from https://rustup.rs and re-run"
  # `curl` is checked for the same reason: without it the download below dies
  # naming the URL, which blames the network for a missing tool.
  command -v curl >/dev/null 2>&1 ||
    _runlib_die "curl not found — it is what fetches the pinned rustup-init; install curl, or install the Rust toolchain from https://rustup.rs, and re-run"
  url="$_RUNLIB_RUSTUP_BASE_URL/$_RUNLIB_RUSTUP_VERSION/$target/rustup-init"

  # Named rather than left to `set -e`: a bare abort here reports a failure
  # with no cause at all.
  tmp_dir="$(mktemp -d)" ||
    _runlib_die "could not create a temporary directory to download rustup-init into"
  _runlib_track_temp "$tmp_dir"
  trap _runlib_cleanup_temps EXIT INT TERM
  installer="$tmp_dir/rustup-init"

  printf 'runlib: installing rustup %s for %s\n' "$_RUNLIB_RUSTUP_VERSION" "$target" >&2
  curl --proto "=https" --tlsv1.2 -sSfL --retry 3 --retry-delay 2 \
    --connect-timeout 30 -o "$installer" "$url" >&2 ||
    _runlib_die "could not download the pinned rustup-init from $url — install the Rust toolchain from https://rustup.rs and re-run"

  actual="$(_runlib_sha256_of "$installer")"
  [[ "$actual" == "$expected" ]] ||
    _runlib_die "rustup-init digest mismatch for $url — expected $expected, got ${actual:-nothing}; refusing to execute the downloaded file"

  chmod +x "$installer" ||
    _runlib_die "could not make the verified $installer executable"
  "$installer" -y --no-modify-path --profile minimal >&2 ||
    _runlib_die "rustup-init failed — install the Rust toolchain from https://rustup.rs and re-run"

  _runlib_cleanup_temps
  # rustup was told not to touch the shell rc files, so the new toolchain
  # reaches this run through PATH here and nowhere else.
  PATH="$(_runlib_cargo_home)/bin:$PATH"
  export PATH
  return 0
}

# The toolchain. A missing `rustc` is bootstrapped from the digest-verified
# pinned rustup-init above; `jq` is a plain precondition this script does not
# install.
_runlib_require_toolchain() {
  PATH="$(_runlib_cargo_home)/bin:$PATH"
  export PATH
  # `rustc`, not `rustup`: a distro-packaged toolchain is a perfectly good
  # toolchain, and demanding rustup rejected it. rustup is asked for a
  # toolchain only where one is missing or too old (the gate below), so its
  # absence is never a precondition failure here — and this is also what makes
  # the bootstrap conditional on there being no compiler at all.
  if ! command -v rustc >/dev/null 2>&1; then
    _runlib_bootstrap_rustup
  fi
  command -v cargo >/dev/null 2>&1 ||
    _runlib_die "cargo not found — install the Rust toolchain from https://rustup.rs and re-run"
  command -v rustc >/dev/null 2>&1 ||
    _runlib_die "rustc not found — install the Rust toolchain from https://rustup.rs and re-run"
  command -v jq >/dev/null 2>&1 ||
    _runlib_die "jq not found — install jq (it parses \`cargo metadata\`) and re-run"
}

# The toolchain name handed to rustup for this run, empty when none was
# needed. Read it after sourcing the script to learn which toolchain the gate
# below selected.
_RUNLIB_TOOLCHAIN_OVERRIDE=""

# Everything handed to rustup is repository input — a `rust-toolchain.toml`
# channel, a `rust-version` out of `cargo metadata` — never a shell word.
# Refuse anything outside the plain toolchain-name shape before rustup runs.
_runlib_assert_toolchain_name() {
  local value="$1" what="$2"
  [[ "$value" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] ||
    _runlib_die "refusing to hand $what '$value' to rustup — that is not a toolchain name"
  return 0
}

# The `channel` of `[toolchain]` in the repository root's `rust-toolchain.toml`.
# Prints nothing when the crate is unpinned.
_runlib_pinned_channel() {
  _runlib_toml_value "$1/rust-toolchain.toml" toolchain channel
  return 0
}

# True when $1 is an *exact* version toolchain — `1.93.1` — as opposed to one
# of rustup's moving channels: `stable`, `beta`, `nightly`,
# `nightly-2025-06-01`, or a two-part `1.93`, which rustup resolves to the
# newest 1.93.x.
#
# The distinction decides the remedy. Only an exact pin can be compared with
# the requirement — `_runlib_version_ge` reads every non-numeric component as
# 0, so comparing `stable` would call a perfectly current channel pin "below
# the requirement" and swap it for an exact version nobody asked for. A moving
# channel is moved instead.
_runlib_is_exact_version() {
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

# Sets _RUNLIB_ACTIVE_RUST_VERSION from `rustc --version`, dying loud when the
# active rustc cannot be run or its version cannot be read. $1 is appended to
# both messages, so the caller says what the version was needed for.
#
# stderr is kept and the call sits in its own `if`: under `set -euo pipefail` a
# `x="$(rustc … 2>/dev/null | sed …)"` assignment aborts the whole script on a
# failing rustc, which made the guard below unreachable dead code for the very
# case it was written for — a rustup shim with no default toolchain then
# produced an empty stdout and a bare status with no diagnostic at all.
_runlib_read_rust_version() {
  local context="$1" reported version
  if ! reported="$(rustc --version 2>&1)"; then
    _runlib_die "cannot read the rustc version (rustc said: ${reported:-nothing})${context}"
  fi
  version="$(printf '%s\n' "$reported" |
    sed -n 's/^rustc \([0-9][0-9]*\.[0-9][0-9]*\(\.[0-9][0-9]*\)\{0,1\}\).*/\1/p')"
  [[ -n "$version" ]] ||
    _runlib_die "cannot read the rustc version from '${reported}'${context}"
  _RUNLIB_ACTIVE_RUST_VERSION="$version"
  return 0
}

# A rustc that cannot run at all, repaired before any cargo call: `rustc` and
# `cargo` on PATH are usually rustup proxies, and a proxy whose pinned
# toolchain is not installed cannot run `cargo metadata` either — so this has
# to come before the metadata calls, not with the gate below.
#
# Without rustup there is nothing to repair with, and the fail-loud that the
# unreadable version has always earned stands.
_runlib_ensure_toolchain() {
  local repo_root="$1" pin reported
  if reported="$(rustc --version 2>&1)"; then
    return 0
  fi
  command -v rustup >/dev/null 2>&1 ||
    _runlib_die "cannot read the rustc version (rustc said: ${reported:-nothing}) and rustup is not on PATH — install the Rust toolchain from https://rustup.rs and re-run"
  pin="$(_runlib_pinned_channel "$repo_root")"
  if [[ -n "$pin" ]]; then
    _runlib_assert_toolchain_name "$pin" "the rust-toolchain.toml channel"
    printf 'runlib: rustc is not runnable — installing the pinned toolchain %s\n' "$pin" >&2
    rustup toolchain install "$pin" >&2 ||
      _runlib_die "could not install the pinned Rust toolchain $pin with rustup — install it from https://rustup.rs and re-run"
  else
    printf 'runlib: rustc is not runnable — selecting the stable toolchain with rustup\n' >&2
    rustup default stable >&2 ||
      _runlib_die "could not select the stable Rust toolchain with rustup — install the Rust toolchain from https://rustup.rs and re-run"
  fi
  reported="$(rustc --version 2>&1)" ||
    _runlib_die "cannot read the rustc version (rustc said: ${reported:-nothing}) after asking rustup for a toolchain — install the Rust toolchain from https://rustup.rs and re-run"
  return 0
}

# The highest `rust-version` in the crate's resolved dependency graph for this
# host, including the crate's own. Prints nothing when nothing declares one.
#
# The crate's own `rust-version` is not the requirement: a dependency can
# demand a newer rustc than anything in the family declares, and that is
# exactly what stopped a Discovery build — `serial_test@4.0.1 requires rustc
# 1.93.1` with no family crate declaring `rust-version` at all. This resolves
# the graph, so it runs on the build path only, never on the skip.
_runlib_required_rust_version() {
  local manifest="$1" root_manifest="$2" verbose host graph declared candidate best=""
  local -a metadata_args
  # The host filter keeps the maximum to the platform actually being built — a
  # Windows-only dependency's `rust-version` is not this host's problem. A
  # rustc that cannot name its own host is not one this script will guess for:
  # dropping the filter silently would let a foreign dependency inflate the
  # requirement and trigger an install nobody needed, with nothing on stderr
  # to say why.
  verbose="$(rustc -vV 2>&1)" ||
    _runlib_die "cannot read the rustc host target (rustc -vV said: ${verbose:-nothing}) — the dependency graph cannot be filtered for this platform"
  host="$(printf '%s\n' "$verbose" | sed -n 's/^host: //p')"
  [[ -n "$host" ]] ||
    _runlib_die "rustc -vV named no host target — the dependency graph cannot be filtered for this platform"
  metadata_args=(metadata --format-version 1 --filter-platform "$host")
  if ! graph="$(cargo "${metadata_args[@]}")"; then
    _runlib_die "cargo metadata could not resolve the dependency graph — the highest required Rust version cannot be read"
  fi
  declared="$(printf '%s' "$graph" |
    jq -r '.packages[]? | select(.rust_version != null) | .rust_version')" ||
    _runlib_die "could not read the dependency graph's rust-version values from cargo metadata"

  # `1.85` and `1.85.0` are the same requirement: `_runlib_version_ge` reads a
  # missing component as 0, so the comparison is numeric either way.
  while IFS= read -r candidate; do
    [[ -n "$candidate" ]] || continue
    if [[ -z "$best" ]] || _runlib_version_ge "$candidate" "$best"; then
      best="$candidate"
    fi
  done <<EOF
$declared
$(_runlib_crate_field "$manifest" "$root_manifest" rust-version)
EOF

  printf '%s' "$best"
  return 0
}

# Move a channel toolchain forward and re-check it: `stable` when the crate is
# unpinned, the pinned channel itself when `rust-toolchain.toml` names one. A
# channel that is still short of the requirement afterwards fails loud — this
# script does not switch a host's default toolchain to get past it.
_runlib_update_channel() {
  local channel="$1" required="$2"
  _runlib_assert_toolchain_name "$channel" "the toolchain channel"
  printf 'runlib: rustc %s is below the required Rust %s — running: rustup update %s\n' \
    "$_RUNLIB_ACTIVE_RUST_VERSION" "$required" "$channel" >&2
  rustup update "$channel" >&2 ||
    _runlib_die "rustup update $channel failed and this build needs Rust $required — install it from https://rustup.rs and re-run"
  _runlib_read_rust_version "; this build needs Rust >= $required"
  _runlib_version_ge "$_RUNLIB_ACTIVE_RUST_VERSION" "$required" ||
    _runlib_die "rustc $_RUNLIB_ACTIVE_RUST_VERSION is still below the Rust $required this dependency graph requires after rustup update $channel — install Rust $required from https://rustup.rs and re-run"
  return 0
}

# The toolchain gate. The requirement is the highest `rust-version` across the
# dependency graph; the active rustc either meets it, or rustup is asked to
# make it so, and only then does this fail loud.
#
# It never downgrades: a rustc at or above the requirement passes with no
# rustup call at all. A pin below the requirement is honoured as a *file* —
# `rust-toolchain.toml` is a repository commit, so it is left untouched and
# this run alone is redirected with a `RUSTUP_TOOLCHAIN` override, named on
# stderr so the bump is obvious.
_runlib_check_msrv() {
  local manifest="$1" root_manifest="$2" repo_root="$3" required pin
  _RUNLIB_TOOLCHAIN_OVERRIDE=""
  # Read before the graph is resolved: a rustc whose version cannot be read is
  # a fault to name here, not one to carry into a `cargo metadata` call.
  _runlib_read_rust_version "; the toolchain gate cannot check it against the dependency graph"
  required="$(_runlib_required_rust_version "$manifest" "$root_manifest")"
  [[ -n "$required" ]] || return 0

  if _runlib_version_ge "$_RUNLIB_ACTIVE_RUST_VERSION" "$required"; then
    return 0
  fi

  # A distro-packaged toolchain is never replaced: with no rustup there is
  # nothing this script may safely install over it.
  command -v rustup >/dev/null 2>&1 ||
    _runlib_die "rustc $_RUNLIB_ACTIVE_RUST_VERSION is below the Rust $required this dependency graph requires, and rustup is not on PATH — install Rust $required from https://rustup.rs and re-run"

  pin="$(_runlib_pinned_channel "$repo_root")"
  # Unpinned, or pinned to a *channel* rather than a version: the remedy is to
  # move that channel forward, never to swap a deliberate channel pin for an
  # exact version.
  if [[ -z "$pin" ]]; then
    _runlib_update_channel stable "$required"
    return 0
  fi
  if ! _runlib_is_exact_version "$pin"; then
    _runlib_update_channel "$pin" "$required"
    return 0
  fi

  # An exact version pin is what rustup's own proxies will honour, so a pin that
  # already satisfies the requirement needs nothing from this script — but the
  # rustc this run measured does not satisfy it, so say which one is building.
  if _runlib_version_ge "$pin" "$required"; then
    printf 'runlib: rustc %s is below the required Rust %s, but rust-toolchain.toml pins %s, which satisfies it — the pinned toolchain is what this run uses\n' \
      "$_RUNLIB_ACTIVE_RUST_VERSION" "$required" "$pin" >&2
    return 0
  fi

  _runlib_assert_toolchain_name "$required" "the required Rust version"
  rustup toolchain install "$required" >&2 ||
    _runlib_die "could not install Rust $required with rustup, and this dependency graph requires it — install it from https://rustup.rs and re-run"
  # Recorded, not exported: the override belongs to the `cargo build` below
  # and nothing else. A sourced caller's shell must not come away pinned to a
  # toolchain it never asked for.
  _RUNLIB_TOOLCHAIN_OVERRIDE="$required"
  # Worded for both entry points: `--toolchain-only` builds nothing, so a line
  # claiming a build would be false in exactly the mode that only reports.
  printf 'runlib: rust-toolchain.toml pins %s, below the Rust %s this dependency graph requires — using %s for this run; bump the pin in rust-toolchain.toml\n' \
    "$pin" "$required" "$required" >&2
  return 0
}

# Remove the checkout's build directory and report what that freed.
#
# Only the checkout's own directory is removed. `CARGO_TARGET_DIR` (or
# `[build] target-dir`) can point cargo at a cache shared with other checkouts,
# and deleting that would destroy builds this script never made — so a target
# directory outside the repository is kept, and the fact is reported rather
# than passed over in silence.
_runlib_remove_target() {
  local crate="$1" target_dir="$2" repo_root="$3" kilobytes bytes resolved
  [[ -d "$target_dir" ]] || return 0
  [[ "$target_dir" == /* ]] ||
    _runlib_die "refusing to remove the relative target directory '$target_dir'"
  [[ "$target_dir" != "/" ]] ||
    _runlib_die "refusing to remove '/' as a target directory"
  # `$PWD` is the *logical* path while cargo reports `target_directory`
  # canonicalised, so comparing them literally made the checkout's own target/
  # look "outside" whenever any component was a symlink — on macOS
  # (/tmp -> /private/tmp) that is every run under /tmp. Resolve both sides
  # first; `cd … && pwd -P` is the portable reading, with no GNU `realpath`.
  if resolved="$(cd "$target_dir" 2>/dev/null && pwd -P)"; then
    target_dir="$resolved"
  fi
  if resolved="$(cd "$repo_root" 2>/dev/null && pwd -P)"; then
    repo_root="$resolved"
  fi
  case "$target_dir" in
    "$repo_root"/*) : ;;
    *)
      printf '[%s] kept %s (outside the checkout %s)\n' \
        "$crate" "$target_dir" "$repo_root" >&2
      return 0
      ;;
  esac
  # `du -sk` is the portable reading — macOS bash 3.2 has no `du -b`.
  kilobytes="$(du -sk "$target_dir" | awk 'NR == 1 { print $1 }')"
  if [[ ! "$kilobytes" =~ ^[0-9]+$ ]]; then
    _runlib_die "could not measure $target_dir before removing it"
  fi
  bytes=$((kilobytes * 1024))
  rm -rf "$target_dir"
  printf '[%s] removed %s (freed %s bytes)\n' "$crate" "$target_dir" "$bytes" >&2
  return 0
}

# macOS refuses to dlopen an unsigned dylib, and a cdylib copied out of
# target/ still carries its build-tree install_name. Both are no-ops elsewhere.
_runlib_macos_fixups() {
  local lib_path="$1" lib_file="$2"
  [[ "$(uname -s)" == "Darwin" ]] || return 0
  install_name_tool -id "@rpath/$lib_file" "$lib_path" >&2 || return 1
  codesign --force --sign - --timestamp=none "$lib_path" >&2 || return 1
  return 0
}

# Every staging temporary this run has created, for the cleanup trap below.
_RUNLIB_TEMPS=()

_runlib_track_temp() {
  _RUNLIB_TEMPS+=("$1")
}

# Remove them all. Armed as a trap around the staging block so an interrupt —
# or a `set -e` abort between staging and the commit — cannot leave
# `.runlib.<pid>` files accumulating under CARGO_HOME with nothing to reap
# them. The bash 3.2-safe expansion keeps an empty array legal under `set -u`.
_runlib_cleanup_temps() {
  local path
  for path in ${_RUNLIB_TEMPS[@]+"${_RUNLIB_TEMPS[@]}"}; do
    [[ -n "$path" ]] || continue
    # The staged artefacts are files; the rustup bootstrap tracks the `mktemp
    # -d` it downloaded the installer into, and `rm -f` would leave that — and
    # the unverified installer inside it — behind.
    if [[ -d "$path" ]]; then
      rm -rf "$path"
    else
      rm -f "$path"
    fi
  done
  _RUNLIB_TEMPS=()
}

# Move a staged artefact onto its final path, and prove it landed there.
#
# `mv -f file dir` moves the file *into* an existing directory and still exits
# 0, so a directory left at the install path would otherwise be stamped,
# followed by the target/ removal, and printed on stdout as a successful
# install. Refuse that up front and confirm a regular file afterwards: an `mv`
# that did not complain is not evidence the artefact is installed.
_runlib_commit() {
  local staged="$1" dest="$2"
  [[ ! -d "$dest" ]] || return 1
  mv -f "$staged" "$dest" || return 1
  [[ -f "$dest" ]] || return 1
  return 0
}

# Put a backed-up artefact back where it was. Best effort by definition — the
# run is already failing — but a failure to restore is still reported, never
# swallowed.
_runlib_restore() {
  local backup="$1" dest="$2"
  [[ -n "$backup" && -f "$backup" ]] || return 0
  mv -f "$backup" "$dest" ||
    printf 'runlib: could not restore %s from %s\n' "$dest" "$backup" >&2
  return 0
}

# Discard every staged artefact and backup named, then die. Empty arguments are
# skipped: a cdylib-only crate stages no binary, and `rm -f ''` is an error on
# some BSD userlands.
_runlib_abort_staged() {
  local message="$1" path
  shift
  for path in "$@"; do
    [[ -n "$path" ]] || continue
    rm -f "$path"
  done
  _runlib_die "$message"
}

# The `cargo metadata --no-deps` reply for the single workspace member, failing
# loud on zero members or on more than one. Both entry points resolve the
# member through this, so the toolchain-only mode reads the crate's own
# `rust-version` from exactly the manifest a build would have used.
#
# It dies from inside the caller's command substitution, so the caller's
# assignment must not be a `local` declaration — that would swallow the status.
_runlib_member_metadata() {
  local repo_root="$1" metadata package_count names
  metadata="$(cargo metadata --no-deps --format-version 1)"
  package_count="$(printf '%s' "$metadata" | jq '.packages | length')"
  if [[ "$package_count" -eq 0 ]]; then
    _runlib_die "cargo metadata reports no workspace member in $repo_root"
  fi
  if [[ "$package_count" -gt 1 ]]; then
    names="$(printf '%s' "$metadata" | jq -r '[.packages[].name] | join(", ")')"
    _runlib_die "expected exactly one workspace member, found $package_count: $names"
  fi
  printf '%s' "$metadata"
  return 0
}

# The toolchain-only entry point (Issue #701). It runs the same chain install
# mode runs — the rustup bootstrap, the unrunnable-rustc repair, the resolved
# dependency graph and the toolchain gate — builds nothing, installs nothing,
# and prints the selected override toolchain name on stdout: a bare `1.93.1`,
# or an empty line when the active toolchain already satisfies the requirement.
# That is the whole of its stdout, because a caller exports it as
# `RUSTUP_TOOLCHAIN` for a cargo command it runs itself.
#
# The already-installed skip is deliberately never consulted: it answers "is
# the artefact current?", and a caller running its own cargo needs a good
# toolchain whatever the answer is.
runlib_ensure_toolchain() {
  local repo_root="$PWD"
  local root_manifest="$repo_root/Cargo.toml"
  [[ -f "$root_manifest" ]] ||
    _runlib_die "no Cargo.toml in $repo_root — run runlib.sh from the repository root"

  _runlib_require_toolchain
  _runlib_ensure_toolchain "$repo_root"

  local metadata manifest
  metadata="$(_runlib_member_metadata "$repo_root")"
  manifest="$(printf '%s' "$metadata" | jq -r '.packages[0].manifest_path')"
  _runlib_check_msrv "$manifest" "$root_manifest" "$repo_root"

  printf '%s\n' "$_RUNLIB_TOOLCHAIN_OVERRIDE"
  return 0
}

runlib_install() {
  local repo_root="$PWD"
  local root_manifest="$repo_root/Cargo.toml"
  [[ -f "$root_manifest" ]] ||
    _runlib_die "no Cargo.toml in $repo_root — run runlib.sh from the repository root"

  if _runlib_try_skip "$repo_root" "$root_manifest"; then
    return 0
  fi

  _runlib_require_toolchain
  _runlib_ensure_toolchain "$repo_root"

  local metadata
  metadata="$(_runlib_member_metadata "$repo_root")"

  local crate version manifest target_dir crate_underscored bin_name lib_name
  crate="$(printf '%s' "$metadata" | jq -r '.packages[0].name')"
  version="$(printf '%s' "$metadata" | jq -r '.packages[0].version')"
  manifest="$(printf '%s' "$metadata" | jq -r '.packages[0].manifest_path')"
  target_dir="$(printf '%s' "$metadata" | jq -r '.target_directory')"
  crate_underscored="${crate//-/_}"
  bin_name="$(printf '%s' "$metadata" | jq -r --arg c "$crate_underscored" '
    .packages[0].targets
    | map(select((.kind | index("bin")) and ((.name | gsub("-"; "_")) == $c)))
    | (.[0].name // empty)')"
  lib_name="$(printf '%s' "$metadata" | jq -r '
    .packages[0].targets
    | map(select(.kind | index("cdylib")))
    | (.[0].name // empty)')"

  [[ -n "$crate" && "$crate" != "null" ]] || _runlib_die "cargo metadata named no crate"
  [[ -n "$version" && "$version" != "null" ]] || _runlib_die "cargo metadata gave no version for $crate"
  if [[ -z "$bin_name" && -z "$lib_name" ]]; then
    _runlib_die "crate '$crate' has no bin target named '$crate_underscored' and no cdylib target — nothing to install"
  fi

  # Cargo has now named the shape authoritatively, so the up-to-date check runs
  # again over it. The fast path above declines every repository layout it
  # cannot read unambiguously (a globbed `members` entry, say); without this
  # second check those layouts would rebuild on every single invocation.
  local expects_bin=0 expects_lib=0
  if [[ -n "$bin_name" ]]; then expects_bin=1; fi
  if [[ -n "$lib_name" ]]; then expects_lib=1; fi
  if _runlib_report_current "$crate" "$version" "$expects_bin" "$expects_lib"; then
    return 0
  fi

  # A rebuild is due, so the toolchain has to be good enough for one. The gate
  # resolves the whole dependency graph, which is why it sits here rather than
  # beside the `--no-deps` call above: an install that is already current
  # costs no graph resolve at all.
  _runlib_check_msrv "$manifest" "$root_manifest" "$repo_root"

  local -a build_args
  build_args=(build --release --package "$crate")
  if [[ -n "$lib_name" ]]; then
    build_args+=(--lib)
  fi
  if [[ -n "$bin_name" ]]; then
    build_args+=(--bin "$bin_name")
  fi
  # RUSTFLAGS is the caller's: this script neither sets nor edits it. The
  # toolchain override, when the gate set one, reaches this one command and
  # goes no further.
  if [[ -n "$_RUNLIB_TOOLCHAIN_OVERRIDE" ]]; then
    RUSTUP_TOOLCHAIN="$_RUNLIB_TOOLCHAIN_OVERRIDE" cargo "${build_args[@]}" >&2
  else
    cargo "${build_args[@]}" >&2
  fi

  local bin_dir lib_dir release_dir bin_file lib_file
  local staged_bin="" staged_lib="" installed_bin="" installed_lib=""
  bin_dir="$(_runlib_cargo_home)/bin"
  lib_dir="$(_runlib_cargo_home)/lib"
  release_dir="$target_dir/release"
  bin_file="$(_runlib_bin_basename "$crate")"
  lib_file="$(_runlib_lib_basename "$crate")"

  # Everything is staged first and moved into place only once every artefact is
  # ready: a crate that ships both a bin and a cdylib must not leave the new
  # binary installed beside the old library when the library step fails.
  trap _runlib_cleanup_temps EXIT INT TERM
  if [[ -n "$bin_name" ]]; then
    local built_bin="$release_dir/$bin_name"
    [[ -f "$built_bin" ]] || _runlib_die "the build produced no binary at $built_bin"
    mkdir -p "$bin_dir"
    staged_bin="$bin_dir/$bin_file.runlib.$$"
    _runlib_track_temp "$staged_bin"
    cp "$built_bin" "$staged_bin" ||
      _runlib_abort_staged "could not stage $built_bin" "$staged_bin"
    chmod +x "$staged_bin" ||
      _runlib_abort_staged "could not make $staged_bin executable" "$staged_bin"
  fi

  if [[ -n "$lib_name" ]]; then
    local built_lib="" candidate built_file lib_extension
    lib_extension="$(_runlib_lib_extension)"
    built_file="lib${lib_name//-/_}.${lib_extension}"
    # A cdylib lands in target/release/, and in target/release/deps/ on the
    # toolchains that only hard-link the former.
    for candidate in "$release_dir/$built_file" "$release_dir/deps/$built_file"; do
      if [[ -f "$candidate" ]]; then
        built_lib="$candidate"
        break
      fi
    done
    [[ -n "$built_lib" ]] ||
      _runlib_abort_staged \
        "the build produced no $built_file in $release_dir or $release_dir/deps" \
        "$staged_bin"
    mkdir -p "$lib_dir"
    staged_lib="$lib_dir/$lib_file.runlib.$$"
    _runlib_track_temp "$staged_lib"
    cp "$built_lib" "$staged_lib" ||
      _runlib_abort_staged "could not stage $built_lib" "$staged_bin" "$staged_lib"
    _runlib_macos_fixups "$staged_lib" "$lib_file" ||
      _runlib_abort_staged "macOS signing failed for $lib_file" "$staged_bin" "$staged_lib"
  fi

  # The binary is committed before the library, so the previous binary is kept
  # aside until the library is in place too. Without that, a library step that
  # failed after the binary landed left exactly the new-bin/old-lib pair this
  # staging exists to prevent.
  local backup_bin=""
  if [[ -n "$staged_bin" ]]; then
    if [[ -f "$bin_dir/$bin_file" ]]; then
      backup_bin="$bin_dir/$bin_file.runlib-prev.$$"
      _runlib_track_temp "$backup_bin"
      cp -p "$bin_dir/$bin_file" "$backup_bin" ||
        _runlib_abort_staged "could not back up $bin_dir/$bin_file" \
          "$staged_bin" "$staged_lib" "$backup_bin"
    fi
    _runlib_commit "$staged_bin" "$bin_dir/$bin_file" ||
      _runlib_abort_staged "could not install $bin_dir/$bin_file" \
        "$staged_bin" "$staged_lib" "$backup_bin"
    installed_bin="$bin_dir/$bin_file"
  fi
  if [[ -n "$staged_lib" ]]; then
    if ! _runlib_commit "$staged_lib" "$lib_dir/$lib_file"; then
      _runlib_restore "$backup_bin" "$bin_dir/$bin_file"
      _runlib_abort_staged "could not install $lib_dir/$lib_file" \
        "$staged_lib" "$backup_bin"
    fi
    installed_lib="$lib_dir/$lib_file"
  fi
  [[ -z "$backup_bin" ]] || rm -f "$backup_bin"

  # The stamps go last: until they are written, a half-finished install still
  # reads as "needs building" rather than as an up-to-date one.
  if [[ -n "$installed_bin" ]]; then
    printf '%s\n' "$version" > "$bin_dir/.${crate}.version"
  fi
  if [[ -n "$installed_lib" ]]; then
    printf '%s\n' "$version" > "$lib_dir/.${crate}.version"
  fi

  _runlib_remove_target "$crate" "$target_dir" "$repo_root"

  # Everything is committed and stamped; nothing is left to reap.
  _RUNLIB_TEMPS=()
  trap - EXIT INT TERM

  if [[ -n "$installed_bin" ]]; then
    printf '%s\n' "$installed_bin"
  else
    printf '%s\n' "$installed_lib"
  fi
  return 0
}

# One line on stderr, and exit 2. A copy that ignored its arguments would run a
# full build for a caller that asked only for the gate, so an argument this
# script does not know is refused before any cargo command runs.
_runlib_usage() {
  printf 'usage: runlib.sh [--toolchain-only]\n' >&2
  exit 2
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  case "${1-}" in
    "")
      [[ $# -eq 0 ]] || _runlib_usage
      runlib_install
      ;;
    --toolchain-only)
      [[ $# -eq 1 ]] || _runlib_usage
      runlib_ensure_toolchain
      ;;
    *)
      _runlib_usage
      ;;
  esac
fi
