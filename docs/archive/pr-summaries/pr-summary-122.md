# PR Summary — Issue #122: quiet test runs in the gate and CI

## Summary

A green gate or CI run printed one line per passing test, and CI also passed
`--verbose`, so the one failure a reviewer needs was buried. Both test runs are
now quiet. A failing test still prints its name, assertion and panic message.

- `.github/workflows/ci.yml`: `cargo test --workspace --all-features --verbose` → `… -q`
- `quality.sh`: `cargo test --workspace --all-features` → `… -q`
- New `rebase/tests/quiet_test_gate.rs` reads both committed files and fails
  if any `cargo test` line in them lacks `-q`/`--quiet` or carries `-v`/`--verbose`.

Out of scope (left as is): `.github/workflows/cargo-upgrade.yml` also runs
`cargo test`, but the issue names only `ci.yml` and `quality.sh`.

Closes #122.

## Evidence

This is a CLI/config change, so there are no screenshots.

**Red.** Before the fix, the two gate tests failed on the committed files:

```text
quality.sh runs `cargo test --workspace --all-features` — a green run must be quiet: pass `-q` and drop `--verbose`
.github/workflows/ci.yml runs `cargo test --workspace --all-features --verbose` — a green run must be quiet: pass `-q` and drop `--verbose`
```

Because that red run was itself under `-q`, it also shows that failure detail
still prints in quiet mode.

**Green.** After the fix, all 4 tests pass. `./quality.sh < /dev/null` exits 0
("All quality checks passed!") and its log has **0** per-test `test … ok` lines.

## Test Plan

- [x] `the_local_quality_gate_runs_the_suite_quietly`: `quality.sh` is quiet
- [x] `the_ci_workflow_runs_the_suite_quietly`: `ci.yml` is quiet
- [x] `a_verbose_or_unflagged_run_is_not_quiet`: covers an unflagged run, `--verbose`, `-q` together with `-v`, and a `-qx` look-alike
- [x] `commands_are_found_in_steps_and_blocks_but_not_comments`: covers an inline `run:`, a `run: |` block, and ignoring comments and `echo` lines
- [x] `cargo fmt --check`, `cargo clippy -D warnings`, and the full `./quality.sh`
