# Threat model

Disclosure channels and scope are in [SECURITY.md](../SECURITY.md). The
enhancement contract is in
[docs/enhancement-format.md](../docs/enhancement-format.md), the protocol in
[docs/rebase-protocol.md](../docs/rebase-protocol.md) and the failure model in
[docs/failure-model.md](../docs/failure-model.md). This file is the short brief
for the scanner.

## What this project does and where untrusted input enters

NEAT-AI-Rebase is an experimental, offline Rust CLI (`neat_ai_rebase`) that
replays improvements found by other NEAT-AI optimisers (Forests patches, Ockham
removals) onto the latest champion creature, then keeps a candidate only when
the external NEAT-AI-scorer binary (`rust_scorer`) says it is fitter. It opens
no network connections and holds no credentials. Treat as untrusted:

- enhancement bundles, single enhancements or directories of them
  (`--enhancements`), filed by other machines (`src/enhancement.rs`,
  `src/patch.rs`), whose payloads drive graph construction;
- creature JSON: the champion (`--champion`) and the creature a bundle is
  harvested from (`--harvest-from`, `src/harvest.rs`), both fetched from a
  repository shared by many machines;
- the training corpus directory of `.bin` files (`--training-data`);
- `experiments.jsonl` journals read back by `report`, and the scorer's output.

The scorer path, `--scorer-arg` values and the output directory are chosen by
the operator and are trusted.

## Components that matter most / least

Most important: enhancement and patch parsing, the compatibility gate
(`src/compat.rs`), the Forests and Ockham adapters (`src/forest.rs`,
`src/ockham.rs`) that rewrite the champion, the validation gate in
`src/creature.rs`, and every file name built from input data under the output
directory. Lower priority: `src/report.rs` and the research drivers under
`rebase/examples/`. `scripts/` and `.github/` are CI tooling.

## How to exercise it

From `/src`: `cargo test --workspace --all-features` runs the suite;
`rebase/tests/race_conditions.rs` drives the engine end to end against a
scripted scorer. `rebase/src/fixtures.rs` builds small creatures, and
`cargo run --example make_fixture` / `--example validate` produce and check
inputs. `rust_scorer` is on `PATH` when its optional install succeeded.

## How you rate severity

This is a research tool run by its own maintainers on their own hosts. The
crate has no `unsafe` code of its own.

- High: memory unsafety reachable from input, writing outside the output
  directory (path traversal), running a command other than the operator's
  scorer, or an enhancement that emits a candidate without an authoritative
  scorer verdict or without passing `creature_validate`.
- Medium: panics, unbounded memory or CPU, or hangs on malformed enhancements,
  creature JSON, corpus files or journals.
- Low: wrong statistics in reports, or issues that need the operator's own
  arguments to be hostile.

## Anything to leave alone

- Bugs in `neat-core` (NEAT-AI-core) or `rust_scorer` belong in those
  repositories.
- An enhancement that scores worse after rebasing is the expected outcome the
  scorer gate exists for, not a vulnerability.
