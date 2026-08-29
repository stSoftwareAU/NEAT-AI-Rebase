#!/usr/bin/env bash
# Lint the GitHub Actions workflows with actionlint (Issue #56).
#
# `.github/workflows/actionlint.yml` and `quality.sh` both invoke this one
# script, so the CI gate and a local run cannot drift. With no arguments it
# lints every workflow this repository commits; pass explicit paths to lint
# something else (`rebase/tests/actionlint_gate.rs` does, to prove a broken
# workflow really is rejected).
#
# A missing linter is a failure, not a skip: reporting "nothing to check" as
# success is exactly how a workflow regression reaches Develop unnoticed.
set -euo pipefail

if ! command -v actionlint &>/dev/null; then
  echo "actionlint is required — install: https://github.com/rhysd/actionlint/blob/main/docs/install.md" >&2
  exit 1
fi

targets=()
if [ "$#" -gt 0 ]; then
  targets=("$@")
else
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  while IFS= read -r workflow; do
    targets+=("$workflow")
  done < <(
    find "$repo_root/.github/workflows" -type f \
      \( -name '*.yml' -o -name '*.yaml' \) | sort
  )
fi

# An empty target list would make `actionlint` lint nothing and exit 0 — a
# vacuous pass that looks identical to a clean tree.
if [ "${#targets[@]}" -eq 0 ]; then
  echo "no workflow files found — refusing to pass vacuously" >&2
  exit 1
fi

actionlint -color "${targets[@]}"
