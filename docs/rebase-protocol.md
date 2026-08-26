# The rebase protocol

## The problem

A long-running optimiser opens on champion **A**, spends 45–60 minutes finding
a real improvement **Δ**, and finishes. By then the fleet has evolved to
champion **B**, which contains improvements the optimiser never saw.

Publishing `A + Δ` is the obvious move and the wrong one: it silently deletes
everything that made B better than A.

```text
A ── optimiser ──▶ A + Δ          ← the stale descendant. Do not publish this.
│
└──────── fleet evolves ────────▶ B
                                  │
                                  └─ reapply Δ ─▶ B + Δ
                                                 │
                                                 └─ authoritative scorer decides
```

Repeat that across a fleet and the population converges on whichever process
happens to finish last — a **monoculture** descended from one search process,
having discarded exactly the concurrent discoveries that made running several
different optimisers worthwhile. `rebase/tests/race_conditions.rs` exists to
keep that from creeping back.

## The stages

```text
opening ancestor  ─▶  enhancement bundle  ─▶  fresh champion  ─▶  candidate cohort  ─▶  scorer verdict
      (A)                    (Δ…)                   (B)              (B, B+Δ, …)          (emit or not)
```

### 1. Opening ancestor

The producer records the checksum and authoritative score of the creature it
started from, plus the corpus identity it is measuring against. This is
provenance, not permission.

### 2. Enhancement bundle

Every locally accepted improvement is recorded as a portable
[enhancement](enhancement-format.md) — the *semantic change*, not the resulting
creature. The producer keeps its own `best.json` if it wants; Rebase does not
read it.

### 3. Fresh champion

**Fetch the current global champion again, immediately before invoking Rebase.**
Rebase loads it once and never re-reads it. Handing it a champion that is
itself an hour old just moves the race one step along.

### 4. Candidate cohort

Rebase runs the common compatibility gate, skips what is already present,
applies the rest to clones of the champion, and builds a small cohort:

| Label | What it is |
| --- | --- |
| `baseline` | the champion itself, always present, never counted against the cap |
| `bundle` | every applicable enhancement, in the producer's order |
| `single-NN` | each applicable enhancement on its own |
| `prefix-NN` | the cumulative prefixes in between |

Candidates are de-duplicated by the checksum of the resulting creature, and a
candidate identical to the champion is dropped: asking the scorer whether the
champion beats itself wastes a corpus pass.

Singles *and* combinations are built because the engine makes **no assumption
of additivity**. Two changes that each helped separately may interact badly.
Scoring both lets the scorer pick the best verified subset instead of Rebase
guessing which member carries the improvement.

### 4b. Screening, and the trap in it

Constructing a cohort is cheap; scoring it is not. A full-corpus pass over the
production corpus is minutes, so it is tempting to rank the cohort on a
sub-sample first and only pay for the promising members.

That is a good instinct with a sharp edge. **Do not select on a sample and then
judge that selection on the same sample.** With N candidates, some beat the
baseline on any given stratum by chance, and "keep the ones that won" picks
exactly those. Measured on the live fleet: choosing the best 32 of 101 patches
on a 2% stratum and re-scoring that selection on the same stratum reported
**+5.9e-04**; the full corpus scored it at **−4.3e-05**. A held-out re-screen
on a different stratum of the same size retained 16 of 29 — about what coin
flips give when the true effect is zero.

The scorer supports this directly: `--sample-phase` shifts the deterministic
stride to a different subsample of the same size. So:

1. screen on one phase;
2. re-screen the survivors on another phase, and keep the intersection;
3. **confirm on the full corpus**, which is the only step that decides
   anything.

Step 3 is what makes a verdict safe — it always was, and a candidate selected
from noise simply loses there. Steps 1 and 2 are what stop the expensive pass
being spent on noise in the first place.

### 5. Scorer verdict

The champion and the whole cohort are scored in **one** authoritative
full-corpus call, so every number is comparable. A candidate is emitted only
when it beats the champion by more than the configured `--min-improvement`.

A tie is not a win: replacing the champion with an equal-scoring creature costs
a population slot and buys nothing.

## Running it

```bash
neat_ai_rebase \
  --champion champion.json \
  --enhancements bundle.json \
  --training-data training/ \
  --scorer ../NEAT-AI-scorer/target/release/rust_scorer \
  --output-dir runs/rebase-1
```

| Output | Written when |
| --- | --- |
| `population-candidate.json` | **only** on a verified improvement |
| `rebase.json` | always — the full summary, verdict included |
| `experiments.jsonl` | always — append-only journal |

Neither the champion file nor any enhancement file is ever written to.

| Exit code | Meaning |
| --- | --- |
| `0` | a verified improvement was emitted (with `--dry-run`: candidates built and validated) |
| `3` | no improvement, or nothing left to do — **a successful, non-destructive outcome** |
| `4` | incompatible input: nothing could be attempted |
| `1` | operational or scorer failure |

`3` is deliberately not `0` so a caller can tell "published" from "correctly
published nothing" without parsing JSON. It is not an error.

## What Rebase will not do

* It will not modify the champion, or any enhancement file.
* It will not promote a candidate on the strength of a previous success.
* It will not emit anything when the scorer failed, however it failed.
* It will not guess at an enhancement kind or version it does not implement.

See [the failure model](failure-model.md) for what each refusal means and what
is safe to retry.
