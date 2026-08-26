# NEAT-AI-Rebase

**Rebase the improvement, don't replace the champion.**

NEAT-AI-Rebase preserves useful discoveries made by independent NEAT-AI
optimisation processes when the global fittest creature changes while they are
working.

A long-running optimiser may start from creature **A**, discover an improvement
**Δ**, and finish after the fleet has already evolved to creature **B**.
Publishing `A + Δ` accidentally discards the improvements accumulated in **B**.

Rebase treats the discovered change as the valuable artefact:

```text
A ── optimiser ──▶ A + Δ
│
└──────── fleet evolves ────────▶ B
                                  │
                                  └─ reapply Δ ─▶ B + Δ
                                                │
                                                └─ authoritative scorer decides
```

The scorer remains the final authority. A rebased candidate is never assumed to
be better merely because the original change was successful.

## Status

The mechanism is implemented and tested end to end against a scripted scorer:
the v1 enhancement contract, the Forests graft adapter, the Ockham removal
adapter, the candidate-cohort engine, the authoritative scorer gate, and the
CLI. `rebase/tests/race_conditions.rs` reproduces the races the project exists
to survive.

Wiring the producers up — NEAT-AI-Forests and NEAT-AI-Ockham calling Rebase at
population re-entry — is the next step, and is deliberately separate: it
changes two running optimisers, so it lands behind their own feature switches
with their own evidence. [`docs/integration.md`](docs/integration.md) is the
checklist for it.

## Quick start

Rebase is a Rust workspace that depends on the sibling
[NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core) checkout and
invokes the [NEAT-AI-scorer](https://github.com/stSoftwareAU/NEAT-AI-scorer)
binary (`rust_scorer`) as the judge — the same layout as NEAT-AI-Forests and
NEAT-AI-Ockham:

```text
parent/
├── NEAT-AI-core/      # path dependency: ../../NEAT-AI-core/neat-core
├── NEAT-AI-scorer/    # build it: cargo build --release  →  target/release/rust_scorer
└── NEAT-AI-Rebase/
```

```bash
cargo build --release

./target/release/neat_ai_rebase \
  --champion champion.json \
  --enhancements bundle.json \
  --training-data training/ \
  --scorer ../NEAT-AI-scorer/target/release/rust_scorer \
  --output-dir runs/first
```

One command runs everything CI runs:

```bash
./quality.sh
```

Two examples build runnable fixtures without a real corpus or champion:
`cargo run --example print_bundle` prints the documented enhancement JSON, and
`cargo run --example make_fixture -- <dir>` writes a champion and a bundle for
a manual end-to-end run against `<dir>/training`.

### Command line

```text
neat_ai_rebase --champion <FILE> --enhancements <FILE-OR-DIR> \
               --training-data <DIR> --output-dir <DIR> \
               [--scorer <PATH>] [--scorer-arg <ARG>]... \
               [--min-improvement <DELTA>] [--max-candidates <N>] [--dry-run]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--champion` | — | the **freshly fetched** current global champion; read, never written |
| `--enhancements` | — | a bundle, a single enhancement, or a directory of either (read in file-name order) |
| `--harvest-from` | — | take the enhancements from this creature instead of a bundle; for a producer that does not file bundles yet |
| `--training-data` | — | the corpus the verdict is measured on, and the source of the corpus identity |
| `--scorer` | — | the `rust_scorer` binary; not required with `--dry-run` |
| `--output-dir` | — | where the three outputs are written |
| `--scorer-arg` | — | extra argument passed verbatim to the scorer, repeatable (e.g. `--scorer-arg=--gpu=off`) |
| `--min-improvement` | `1e-9` | score a candidate must beat the champion by |
| `--max-candidates` | `8` | cap on constructed candidates, excluding the baseline; `0` = uncapped |
| `--screen-sample-rate` | off | screen each enhancement on a sub-sample first and carry forward only what beats the champion alone |
| `--screen-held-out` | `true` | re-screen survivors on a second stratum and keep the intersection |
| `--dry-run` | off | build and validate candidates without scoring or emitting |

> **Fetch the champion immediately before running.** Rebase loads it once and
> never re-reads it. Handing it a champion that is already stale reintroduces
> the race it exists to remove.

### Screening

Scoring is the expensive part, and most enhancements do not earn their place.
Measured on a live fleet, of one donor's 13 patches two improved the champion
and eleven made it worse, and every cumulative prefix was worse than the best
single it contained.

`--screen-sample-rate 0.05` scores each enhancement alone on a sub-sample and
carries forward only the winners; `--screen-held-out` then confirms them on a
different stratum of the same size and keeps the intersection. On production
creatures that narrows 11 patches to 3 before the corpus is touched.

The screen only ever *narrows* what is scored authoritatively. It cannot
promote anything — a sampled mode is refused as a verdict — and selecting on
one stratum without the held-out confirmation is a trap; see
[`docs/rebase-protocol.md`](docs/rebase-protocol.md).

### Outputs and exit codes

| Output | Written when |
| --- | --- |
| `population-candidate.json` | **only** when the scorer confirmed an improvement over the champion |
| `rebase.json` | always — the full summary, verdict included |
| `experiments.jsonl` | always — append-only journal for unattended diagnostics |
| `scoring/` | always — the creature files handed to the scorer, one per cohort member, kept for diagnosis |

| Exit code | Meaning |
| --- | --- |
| `0` | a verified improvement was emitted (with `--dry-run`: candidates built and validated) |
| `3` | no improvement, or nothing left to do — **a successful, non-destructive outcome** |
| `4` | incompatible input: nothing could be attempted |
| `1` | operational or scorer failure |

## Scope

Version 1 implements the enhancement types whose semantics are clear:

1. **NEAT-AI-Forests grafts** — portable patches described by inputs, output
   and correction tree.
2. **NEAT-AI-Ockham removals** — neuron-UUID removals with the strategy needed
   to reproduce them.

Weight, bias and squash modifications are deliberately absent: their rebase
semantics are not explicit yet, and guessing would produce candidates nobody
can reason about.

## Design principles

### Semantic changes, not raw JSON diffs

Rebase understands what an optimiser changed. A raw structural diff is fragile
when the target creature has independently evolved.

### Idempotence beats host exclusion

Rebase needs no special logic to avoid the creature a host just published. If
the latest champion already contains an enhancement, it is recognised as
present — see [`docs/idempotence.md`](docs/idempotence.md).

### No assumption of additivity

Two changes that each helped separately may interact badly. Combinations are
scored, not trusted.

### The scorer has the final say

Search heuristics, previous wins and provenance are evidence. Only an
authoritative score determines population re-entry.

## Documentation

| Document | What it covers |
| --- | --- |
| [`docs/enhancement-format.md`](docs/enhancement-format.md) | the v1 envelope, both payloads, and which fields form the identity |
| [`docs/rebase-protocol.md`](docs/rebase-protocol.md) | ancestor → bundle → fresh champion → cohort → verdict |
| [`docs/idempotence.md`](docs/idempotence.md) | how producers mark enhancements and how duplicate application is avoided |
| [`docs/failure-model.md`](docs/failure-model.md) | every fail-closed case, and what is safe to retry |
| [`docs/integration.md`](docs/integration.md) | the producer checklist, with worked Forest and Ockham examples |

## Relationship to other projects

- [NEAT-AI-Forests](https://github.com/stSoftwareAU/NEAT-AI-Forests) discovers
  portable residual-correction grafts.
- [NEAT-AI-Ockham](https://github.com/stSoftwareAU/NEAT-AI-Ockham) discovers
  removable structure.
- [NEAT-AI-scorer](https://github.com/stSoftwareAU/NEAT-AI-scorer) is the
  authoritative judge.
- [NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core) supplies the
  shared creature representation, the canonical `IF` graft helper and the
  validation contract.

## Application scope

NEAT-AI and its public `NEAT-AI-*` subprojects are intentionally
**application-agnostic**. They provide general neural-network evolution,
inference, scoring and optimisation techniques; specific downstream
applications belong outside these public libraries. Rebase carries no
domain-specific dependencies or terminology, and its enhancement payloads
describe indices and UUIDs rather than anything about a particular problem.

## Pinned Rust toolchain

`rust-toolchain.toml` pins the channel so `rustup` resolves the same
`rustc`/`clippy`/`rustfmt` locally and in CI. The gate is `-D warnings`, so an
unpinned stable could break the build with no code change.
