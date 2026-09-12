#!/usr/bin/env bash
# Hermetic tests for scripts/runlib.sh (Issue #108).
#
# Never runs a real cargo build. A cargo shim answers metadata and fails
# loud if `cargo build` is invoked — the already-installed path must not
# compile anything.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNLIB="${SCRIPT_DIR}/runlib.sh"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT
REAL_PATH="${PATH}"

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

install_cargo_shim() {
  local bin_dir="$1"
  mkdir -p "${bin_dir}"
  cat >"${bin_dir}/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "metadata" ]]; then
  printf '%s\n' '{"packages":[{"name":"neat-ai-rebase","version":"9.9.9"}]}'
  exit 0
fi
echo "UNEXPECTED cargo: $*" >&2
exit 99
EOF
  chmod +x "${bin_dir}/cargo"
}

echo "=== already installed: no cargo build, bin path on stdout ==="
HOME="${WORK_DIR}/home"
export HOME
mkdir -p "${HOME}/.cargo/bin"
printf 'fake\n' >"${HOME}/.cargo/bin/neat_ai_rebase"
chmod +x "${HOME}/.cargo/bin/neat_ai_rebase"
printf '9.9.9\n' >"${HOME}/.cargo/bin/.neat_ai_rebase.version"
install_cargo_shim "${WORK_DIR}/shim"
PATH="${WORK_DIR}/shim:${REAL_PATH}"
export PATH

OUT="$(bash "${RUNLIB}" 2>"${WORK_DIR}/already.err")"
RC=$?
assert_eq "already-installed exits 0" "0" "${RC}"
assert_eq "already-installed stdout is the CLI path" \
  "${HOME}/.cargo/bin/neat_ai_rebase" "${OUT}"
assert_eq "already-installed names the version on stderr" "0" \
  "$(grep -q '\[neat_ai_rebase\] already installed v9.9.9' "${WORK_DIR}/already.err"; echo $?)"

echo ""
echo "=== missing stamp is not treated as already installed ==="
rm -f "${HOME}/.cargo/bin/.neat_ai_rebase.version"
OUT="$(bash "${RUNLIB}" 2>"${WORK_DIR}/rebuild.err")" && RC=0 || RC=$?
assert_eq "missing stamp attempts a build and the shim refuses it" "99" "${RC}"
assert_eq "refused build names unexpected cargo" "0" \
  "$(grep -q 'UNEXPECTED cargo: build' "${WORK_DIR}/rebuild.err"; echo $?)"

echo ""
echo "=== summary: ${PASSED} passed, ${FAILED} failed ==="
[[ "${FAILED}" -eq 0 ]]
