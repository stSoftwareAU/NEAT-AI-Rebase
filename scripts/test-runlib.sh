#!/usr/bin/env bash
# Hermetic tests for the copied scripts/runlib.sh (Issues #108, #107).
#
# `scripts/runlib.sh` is NEAT-AI-core's canonical copy and is never edited here,
# so these tests assert the contract this repository depends on: the crate's
# install names (`neat_ai_rebase` beside the stamp `.neat-ai-rebase.version`),
# the already-installed skip that runs no cargo command at all, and a failed
# run that leaves the installed artefacts untouched.
#
# Never runs a real cargo build. Shims answer for `rustc` and log every `cargo`
# invocation, so "ran no cargo command" is asserted rather than assumed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
RUNLIB="${SCRIPT_DIR}/runlib.sh"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT
REAL_PATH="${PATH}"

# The names the canonical runlib.sh derives from the crate: the bin target
# carries `-` -> `_`, the stamp keeps the crate name as written.
CRATE="neat-ai-rebase"
BIN_NAME="neat_ai_rebase"

PASSED=0
FAILED=0

if [[ ! -x "${RUNLIB}" ]]; then
  echo "FAIL: runlib not found or not executable: ${RUNLIB}" >&2
  exit 2
fi

assert_eq() {
  local desc="$1" expected="$2" actual="$3"
  if [[ "${expected}" == "${actual}" ]]; then
    echo "  PASS: ${desc}"
    PASSED=$((PASSED + 1))
  else
    echo "  FAIL: ${desc}"
    echo "    expected: '${expected}'"
    echo "    actual:   '${actual}'"
    FAILED=$((FAILED + 1))
  fi
}

# `cargo` logs every invocation, answers the metadata queries the install path
# makes, and refuses `build` — so a run that reaches the compiler is proved by
# the log rather than assumed. `rustc` answers a version and a host target, so
# the toolchain gate neither downloads rustup nor reaches for a real compiler.
install_shims() {
  local bin_dir="$1" log="$2" manifest="$3" target_dir="$4"
  mkdir -p "${bin_dir}"
  cat >"${bin_dir}/cargo" <<EOF
#!/usr/bin/env bash
printf 'cargo %s\n' "\$*" >>"${log}"
case "\${1:-}" in
  metadata)
    # The workspace member, for the shape and version checks. The graph query
    # (--filter-platform) answers no packages, so the toolchain gate asks for
    # nothing.
    if [[ "\$*" == *--no-deps* ]]; then
      printf '%s\n' '{"packages":[{"name":"${CRATE}","version":"${VERSION}","manifest_path":"${manifest}","targets":[{"kind":["bin"],"name":"${BIN_NAME}"}]}],"target_directory":"${target_dir}"}'
    else
      printf '%s\n' '{"packages":[]}'
    fi
    exit 0
    ;;
esac
echo "UNEXPECTED cargo: \$*" >&2
exit 99
EOF
  cat >"${bin_dir}/rustc" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-vV" ]]; then
  echo "rustc 1.98.0 (0000000 2026-01-01)"
  echo "host: x86_64-unknown-linux-gnu"
  exit 0
fi
echo "rustc 1.98.0 (0000000 2026-01-01)"
EOF
  chmod +x "${bin_dir}/cargo" "${bin_dir}/rustc"
}

CARGO_HOME="${WORK_DIR}/cargo-home"
export CARGO_HOME
BIN_DIR="${CARGO_HOME}/bin"
STAMP="${BIN_DIR}/.${CRATE}.version"
CARGO_LOG="${WORK_DIR}/cargo.log"
mkdir -p "${BIN_DIR}"

# The version the manifest declares — the same value the fleet's stamp carries.
VERSION="$("${SCRIPT_DIR}/auto-version.sh" --print "${REPO_ROOT}/rebase/Cargo.toml")"

install_shims "${WORK_DIR}/shim" "${CARGO_LOG}" \
  "${REPO_ROOT}/rebase/Cargo.toml" "${WORK_DIR}/target"

echo "=== already installed: no cargo command, bin path on stdout ==="
printf 'fake\n' >"${BIN_DIR}/${BIN_NAME}"
chmod +x "${BIN_DIR}/${BIN_NAME}"
printf '%s\n' "${VERSION}" >"${STAMP}"
: >"${CARGO_LOG}"

OUT="$(cd "${REPO_ROOT}" && PATH="${WORK_DIR}/shim:${REAL_PATH}" bash "${RUNLIB}" 2>"${WORK_DIR}/already.err")" && RC=0 || RC=$?
assert_eq "already-installed exits 0" "0" "${RC}"
assert_eq "already-installed stdout is the CLI path" \
  "${BIN_DIR}/${BIN_NAME}" "${OUT}"
assert_eq "already-installed names the crate and version on stderr" "0" \
  "$(grep -q "\[${CRATE}\] already installed v${VERSION}" "${WORK_DIR}/already.err" && echo 0 || echo 1)"
assert_eq "already-installed ran no cargo command" "" "$(cat "${CARGO_LOG}")"

echo ""
echo "=== stale stamp: a build is attempted and the install is left untouched ==="
printf '0.0.0-stale\n' >"${STAMP}"
: >"${CARGO_LOG}"

OUT="$(cd "${REPO_ROOT}" && PATH="${WORK_DIR}/shim:${REAL_PATH}" bash "${RUNLIB}" 2>"${WORK_DIR}/rebuild.err")" && RC=0 || RC=$?
assert_eq "stale stamp fails loud rather than reporting an install" "1" \
  "$([[ "${RC}" -ne 0 ]] && echo 1 || echo 0)"
assert_eq "stale stamp is not reported as already installed" "1" \
  "$(grep -q "already installed" "${WORK_DIR}/rebuild.err" && echo 0 || echo 1)"
assert_eq "stale stamp runs cargo build for the crate's own bin" "0" \
  "$(grep -q "^cargo build --release --package ${CRATE} --bin ${BIN_NAME}\$" "${CARGO_LOG}" && echo 0 || echo 1)"
assert_eq "the failed run left the stamp alone" "0.0.0-stale" "$(cat "${STAMP}")"
assert_eq "the failed run left the installed binary alone" "fake" \
  "$(cat "${BIN_DIR}/${BIN_NAME}")"

echo ""
echo "=== summary: ${PASSED} passed, ${FAILED} failed ==="
[[ "${FAILED}" -eq 0 ]]
