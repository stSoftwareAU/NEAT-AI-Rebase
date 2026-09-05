# Actionlint gates the PR, not the merge into `Develop`

## Summary

`.github/workflows/actionlint.yml` is a checker, not a deploy: as a required
status check it already gates every merge on the pull request, so its `push:`
trigger on `Develop` re-ran the whole lint on each merge — a duplicate of the
run that already passed, burning CI minutes and able to redden the default
branch for a check that was already green.

Dropped the `push:` block, keeping `pull_request` (including the
`milestone/*` filter) and `workflow_dispatch`, so the workflow now matches the
trigger shape `ci.yml` and `markdown-lint.yml` already use (Issues #57, #58).
`CONTRIBUTING.md` gains the same one-line note those workflows carry.

Closes #83.

```mermaid
flowchart LR
    PR[PR into Develop] --> L[Actionlint run]
    L --> M[Merge]
    M -.->|removed| D[Duplicate post-merge run]
    WD[workflow_dispatch] --> L
```

## Evidence

Backend/CI-only change — no web interface to screenshot. Evidence is the test
run: `actionlint_does_not_rerun_on_push_to_the_default_branch` failed against
the unfixed workflow —

```text
actionlint.yml triggers ["pull_request", "push", "workflow_dispatch"] still
include `push` — the gate would re-run on every merge into the default branch,
duplicating the PR run
```

— and passes after the trigger was removed. The full gate (`./quality.sh`)
passes end to end, including `committed_workflows_lint_clean`, which runs
`actionlint` over the edited workflow.

## Test Plan

- Added `rebase/tests/workflow_push_triggers.rs::actionlint_does_not_rerun_on_push_to_the_default_branch`
  — asserts the committed workflow's `on:` block has no `push` trigger.
- Added `rebase/tests/workflow_push_triggers.rs::actionlint_still_gates_pull_requests_and_stays_dispatchable`
  — asserts `pull_request` and `workflow_dispatch` survive, so removing `push`
  cannot silently ungate PRs.
- Existing `workflow_branch_filters.rs` and `actionlint_gate.rs` tests still
  pass, confirming the `milestone/*` PR filter and the lint gate itself are
  untouched.
