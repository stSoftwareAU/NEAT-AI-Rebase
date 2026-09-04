# NEAT-AI-Rebase

![NEAT-AI-Rebase](https://raw.githubusercontent.com/stSoftwareAU/NEAT-AI/Develop/docs/brand/social-previews/neat-ai-rebase.png)

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

Two later additions let a producer call one binary rather than file a bundle
first: `--harvest-from` reads the enhancements back out of a published creature,
and `--screen-sample-rate` narrows a cohort on a sub-sample before the corpus is
touched.

Wiring the producers up — NEAT-AI-Forests and NEAT-AI-Ockham calling Rebase at
population re-entry — lands in each optimiser's own repository, because it
changes a running optimiser: it goes behind that optimiser's feature switch
with its own evidence. [`docs/integration.md`](docs/integration.md) is the
checklist for it.

Both halves of that wiring are ready on this side. `PruneLog` turns each
accepted Ockham prune, and `PatchLog` each accepted Forest patch or verified
combo, into a v1 enhancement stamped with the opening checksum, score and
corpus identity; `rebase/tests/ockham_reentry.rs` and
`rebase/tests/forest_reentry.rs` run the whole path — file the changes, fetch a
*fresh* champion, replay, score, publish — over the real CLI against a
scripted scorer.

**NEAT-AI-Forests calls it** (Issue #65): `forests/src/enhancements.rs` opens a
`PatchLog` on the creature the run started from and files every patch the full
scorer accepts, writing `enhancements.json` beside `best.json`. It is behind
`--enhancements`, off by default, and NEAT-AI-Ockham's own call is still to
come at its acceptance point and behind its own switch.

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

Four more work against real creatures. `harvest_bundle` recovers the bundle a
Forests run would have filed, from the creature it published; `validate` checks
creature JSON against the shared NEAT-AI-core contract; and `rebase_experiment`
and `union_experiment` are the overnight harnesses — the first asks whether
rebasing a Forests creature's discoveries onto the concurrently-evolved champion
beats publishing that creature, the second grafts every scorer-verified
discovery the fittest creature is missing back onto it. Each carries its own
usage in the file header.

### Command line

```text
neat_ai_rebase --champion <FILE> \
               (--enhancements <FILE-OR-DIR> | --harvest-from <FILE>) \
               --training-data <DIR> --output-dir <DIR> \
               [--scorer <PATH>] [--scorer-arg <ARG>]... \
               [--min-improvement <DELTA>] [--max-candidates <N>] \
               [--screen-sample-rate <RATE>] [--screen-held-out <BOOL>] \
               [--dry-run]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--champion` | — | the **freshly fetched** current global champion; read, never written |
| `--enhancements` | — | a bundle, a single enhancement, or a directory of either (read in file-name order); required unless `--harvest-from` is given |
| `--harvest-from` | — | take the enhancements from this creature instead of a bundle; for a producer that does not file bundles yet. Mutually exclusive with `--enhancements` |
| `--training-data` | — | the corpus the verdict is measured on, and the source of the corpus identity |
| `--scorer` | — | the `rust_scorer` binary; not required with `--dry-run` |
| `--output-dir` | — | where the outputs below are written |
| `--scorer-arg` | — | extra argument passed verbatim to the scorer, repeatable (e.g. `--scorer-arg=--gpu=off`) |
| `--min-improvement` | `1e-9` | score a candidate must beat the champion by |
| `--max-candidates` | `8` | cap on constructed candidates, excluding the baseline; `0` = uncapped |
| `--screen-sample-rate` | off | screen each enhancement on a sub-sample first and drop what the stratum can see losing to the champion; engaged only when the cohort does not fit `--max-candidates` |
| `--screen-held-out` | `true` | re-screen survivors on a second stratum and keep the intersection; takes an explicit `true`/`false` |
| `--dry-run` | off | build and validate candidates without scoring or emitting |

> **Fetch the champion immediately before running.** Rebase loads it once and
> never re-reads it. Handing it a champion that is already stale reintroduces
> the race it exists to remove.

### Reading the journals back

```text
neat_ai_rebase report <experiments.jsonl>...
```

Every run appends to `experiments.jsonl`; `report` reads one or many of those
journals and prints what a soak actually did. It writes nothing, scores
nothing, and exits `0`.

```text
Runs by outcome
  improved                                 4
  noImprovement                            3
  nothingToDo                             16
  incompatible                             1
  dryRun                                   0
  failed                                   0
  no result recorded                       1

Best candidate vs champion
  runs scored                              7
  runs with a winner                       4
  minimum                                 -1.300e-2
  median                                  +8.000e-4
  maximum                                 +2.190e-2

Screen vs the authoritative pass
  runs screened                            8
  screen kept nothing                      1
  full pass confirmed                      4
  full pass rejected                       3
  outcome not recorded                     0
```

The outcomes are deliberately never collapsed. "23 runs, 4 wins" hides the
answer: 16 `nothingToDo` runs mean the fleet had already absorbed the work,
which says the opposite of 16 `noImprovement` runs — those would mean the
corpus kept rejecting what was rebased. The same applies to the screen: one
that never disagrees with the full pass is not earning its keep, and one that
disagrees constantly is miscalibrated.

Three rules the reader keeps:

- **A partial last line is normal**, not fatal. A run killed mid-write leaves
  one; it is counted under `unreadable lines` and shown, never hidden.
- **Absent is not zero.** A field an older record does not carry is left out of
  the numbers rather than counted as `0` — a verdict with no `delta` shows `no
  delta recorded`, not a gain of nothing.
- **An unreadable *file* still fails loudly**, exit `1`, naming the journal.

### Screening

Scoring is the expensive part, and most enhancements do not earn their place.
Measured on a live fleet, of one donor's 13 patches two improved the champion
and eleven made it worse, and every cumulative prefix was worse than the best
single it contained.

`--screen-sample-rate 0.05` scores each enhancement alone on a sub-sample and
drops the ones the stratum can see losing to the champion; `--screen-held-out`
then repeats that on a different stratum of the same size and keeps the
intersection. On production creatures that narrows 11 patches to 3 before the
corpus is touched.

Two boundaries keep it from costing more than it buys (Issue #42):

- **It engages only when it can save a corpus pass.** A cohort that already
  fits `--max-candidates` is scored in full either way, so the screen would
  spend an extra scorer invocation only to discard information. Below the cap
  the cohort goes straight to the authoritative pass, which is the better test
  and was already paid for.
- **Elimination is one-sided.** Only a candidate whose sampled score is *below*
  the baseline by more than `--min-improvement` is dropped. A graft fires on a
  subset of records; when none of them land in the stratum its sampled score is
  the baseline exactly, and that is the stratum failing to resolve the
  candidate, not the candidate failing. Ties, sub-resolution differences, and
  candidates the screen returned no score for are **undecided**, and undecided
  is carried to the corpus.

**The effect size it is powered for.** A stratum of rate `r` over `N` records
resolves a difference `d` only while `rN ≳ (σ/d)²` — the standard error of a
mean-based score falls as `1/√(rN)`, so a tenfold smaller effect needs a
hundredfold more records. At `r = 0.05` the screen is powered for **gross
losses, of order 1e-2 to 1e-3** on the sampled score: the eleven patches that
made the champion worse are exactly that shape. It is **not** powered for the
effects a good patch has — a live fleet's two accepted patches gained
**7.1e-05** together, and its discard margin the same night was **5.7e-05**,
one to two orders of magnitude below what a 5% stratum can see. That gap is why
elimination is one-sided and why the full corpus is the only thing that
promotes. Pick the rate from the effect you need it to see, not from a round
number.

The screen only ever *narrows* what is scored authoritatively. It cannot
promote anything — a sampled mode is refused as a verdict — and selecting on
one stratum without the held-out confirmation is a trap; see
[`docs/rebase-protocol.md`](docs/rebase-protocol.md).

**What a phase leaves behind.** Every phase journals a `screen` record naming,
per enhancement, its sampled score and **signed delta** against that stratum's
baseline, the verdict — `better`, `indistinguishable`, `worse`, `notScored` or
`notBuilt` — and the records the stratum actually held, and prints the same
lines on stderr:

```text
neat_ai_rebase: screen phase 0 kept 0 of 3 (baseline 0.500000 over 1000 records at rate 0.05)
neat_ai_rebase:   7b7fc3fab572a0db neat-ai-forests/0.1.17 delta -3.000e-4 worse
```

A survivor count alone cannot be diagnosed: three deltas of `-3e-4` is the
screen working, three of exactly `0.0` is a stratum that resolved nothing, and
the two call for opposite responses. Only `worse` eliminates, so the deltas also
show how close the rest came. The record count is there because the power of the
comparison depends on it, and the staging directory is deleted on the way out
(Issue #43).

### Outputs and exit codes

| Output | Written when |
| --- | --- |
| `population-candidate.json` | **only** when the scorer confirmed an improvement over the champion |
| `rebase.json` | always — the full summary, verdict included |
| `experiments.jsonl` | always — append-only journal for unattended diagnostics, read back by `neat_ai_rebase report` |
| `scoring/` | always — the creature files handed to the scorer, one per cohort member, kept for diagnosis |

| Exit code | Meaning |
| --- | --- |
| `0` | a verified improvement was emitted (with `--dry-run`: candidates built and validated) |
| `3` | no improvement, or nothing left to do — **a successful, non-destructive outcome** |
| `4` | incompatible input: nothing could be attempted |
| `1` | operational or scorer failure |

### What a run says it did

Every run writes one line describing its outcome, to the journal's `result`
record and — on a win — to the emitted creature's `rebase` tag. Downstream that
line becomes a commit subject, so it names every number's baseline rather than
leaving a reader to guess:

```text
🪢 Rebase applied · 2 enhancements from neat-ai-forests · champion 0.419407 → rebased 0.419751 (+3.44e-4) · claim delta -1.50e-3 vs claimed 0.421251
🪢 Rebase not applied · 2 enhancements from neat-ai-forests · champion 0.500000 held · best candidate 0.490000 (-1.00e-2) · claim delta -1.10e-1 vs claimed 0.600000
```

| Word | The score it names | Who measured it |
| --- | --- | --- |
| `claimed` | the producer's own figure for the creature it filed the enhancements from | the producer, on its older opening creature |
| `validated source` | the same source creature, re-scored here | this run's authoritative scorer |
| `champion` | the champion the replay was measured against | this run's authoritative scorer |
| `rebased` | the promoted candidate | this run's authoritative scorer |

The two deltas answer different questions and are never blurred together. The
arrow is the rebase's own gain — what replaying the discoveries onto the current
champion added. The **claim delta** is the mismatch between what the producer
claimed and what authoritative scoring found, and it is routinely negative
because the producer measured itself on an older, easier creature. That is a
disagreement between two measurements, not the creature getting worse, so it is
never reported as a decline (Issue #80). A source score this run measured
itself is a **source delta vs validated source**, because a claim and a
measurement are different facts.

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

## Where this sits in the literature

Rebase has no direct equivalent we can find in neuroevolution, but the pattern
it implements is orthodox in three other fields. The mechanism is novel *as an
artefact*, not *as an idea*: machine learning has been reconciling stale updates
against a moved base since at least 2011, and concurrency control has been
validating finished work against the version it actually landed on since 1981.
That framing is the stronger one — the design has thirty years of
concurrency-control thinking behind it rather than one repository's.

**Stale-update reconciliation** — the closest prior art. Asynchronous SGD
computes an update against parameters `w_t` and applies it to `w_{t+k}`.
Hogwild! (Recht et al. 2011) argues the staleness is tolerable; DistBelief (Dean
et al. 2012) measures the damage; and DC-ASGD (Zheng et al. 2017, *Asynchronous
Stochastic Gradient Descent with Delay Compensation*, ICML) explicitly corrects
a delayed update for the base having moved. That is Rebase's problem statement
with structure in place of weights: Δ was derived against **A** and is being
applied to **B**.

**Patch transplantation** — lifting a change out of one artefact and re-grafting
it into a different, independently moved one: Barr et al. 2015, *Automated
Software Transplantation* (ISSTA); Petke et al. 2018, *Genetic Improvement of
Software: A Comprehensive Survey* (IEEE TEVC). The enhancement contract is the
patch, `--harvest-from` is the donor, and the freshly fetched champion is the
host.

**Optimistic concurrency control** — Kung & Robinson 1981, *On optimistic
methods for concurrency control* (ACM TODS): read a version, work, then validate
at commit time against the version you actually landed on. `judge` is that
validation phase, and `rebase/tests/race_conditions.rs` exercises the anomalies
OCC is defined against. The git-rebase framing this project is named for is the
same idea arriving from a third field; both are worth naming.

**Asynchronous and steady-state evolutionary algorithms** — the EA-side
analogue. Workers publish into a population that keeps moving while they work,
rather than into a synchronised generation boundary.

**Sampled screening before authoritative scoring** — racing: Maron & Moore 1994
(Hoeffding races); Birattari et al. 2002 (F-Race); Jamieson & Talwalkar 2016
(successive halving); Li et al. 2017 (Hyperband). Each of those eliminates an
arm only once it is *behind*, never on a bare point comparison — and since
[#42](https://github.com/stSoftwareAU/NEAT-AI-Rebase/issues/42) so does
`--screen-sample-rate`: a candidate the stratum cannot resolve is undecided and
goes to the corpus. What Rebase does not yet take from those methods is the
confidence bound itself; elimination is thresholded on `--min-improvement`
rather than on a variance estimate.

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
