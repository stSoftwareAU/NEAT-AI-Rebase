# The failure model

Rebase fails **closed**. Every refusal below means the same thing: no
population candidate was emitted, and nothing was modified. The differences are
in what the operator should do about it.

## The three kinds of outcome

| Kind | Exit | Emitted | What it means |
| --- | --- | --- | --- |
| Success | `0` | `population-candidate.json` | the scorer confirmed an improvement over the current champion |
| Normal non-event | `3` | nothing | no improvement, or nothing left to do |
| Refusal | `4` | nothing | the input could not be attempted |
| Failure | `1` | nothing | something operational broke |

**A no-improvement verdict is a normal result.** So is `nothingToDo`. Neither
is a fault, neither should page anyone, and a `set -e` caller should treat exit
`3` as success.

## Fail-closed cases

### Unsupported envelope version

The document declares a `version` this build does not implement. Rebase does
not know what the change is, so it does not attempt it.

*Retry:* no. Upgrade Rebase, or have the producer file v1.

### Unknown payload kind

The `kind` names an operation this build does not implement — a future
`weightNudge`, say. The document does not parse.

*Retry:* no. Same remedy.

### Identity mismatch

`meta.id` is not the id the payload actually has. Idempotence relies on the id
naming the change, so a mis-filed id would let the same change be grafted
twice.

*Retry:* no. Fix the producer.

### Corpus drift

The enhancement was measured on a different corpus from the one the decision is
being made on. A score measured elsewhere is not evidence about here.

*Retry:* not against this corpus. The bundle is still valid against its own
corpus, so keep it.

### Dimension mismatch

The enhancement was written against a creature of a different input or output
width. It cannot address this champion at all.

*Retry:* no.

### Operation-specific preconditions

Reported per enhancement with an actionable reason, and the rest of the bundle
carries on without it:

* a Forest patch naming a feature or output the champion does not have;
* a Forest patch whose root is a bare leaf, or which carries a non-finite
  value;
* an anchor the walk cannot resolve — an output squash that is neither additive
  nor linear in any one source, or a clamp that selects between two neurons so
  no single branch carries the correction;
* an Ockham removal whose target is not hidden, is behind a typed synapse, or
  feeds an aggregate neighbour, so the bias fold is not a sum;
* a recorded `identityCollapse` whose target is no longer an IDENTITY neuron —
  refused rather than silently downgraded to the approximate ablation.

*Retry:* only if the champion changes. On a different champion the same
enhancement may well apply, which is the point of keeping the bundle.

### A combination that will not build

An enhancement that applies on its own can still fail inside a combination.
That chain stops and is journalled; the shorter prefixes and every single
candidate stay valid. One incompatible member never corrupts the rest.

*Retry:* nothing to do — the cohort already contains the members that did
build.

### The champion itself is refused

The supplied champion does not compile, carries a duplicate
`(from, to, type)` synapse, fails `creature_validate`, or does not round-trip.
There is no safe way to build on a creature the scorer would refuse.

*Retry:* no, not with this champion. This is a signal about the population, not
about the bundle.

### Scorer failure

Spawn failure, non-zero exit, malformed output, a missing `baseline` entry, a
missing candidate entry, or a non-finite number. All of them mean the same
thing: there is no trustworthy verdict, so nothing is emitted.

*Retry:* yes — this is the one class that is usually transient. Re-fetch the
champion first; by the time you retry it may have moved again.

### Baseline drift

The creature staged as `baseline` does not checksum to the champion the cohort
was built from, or the winning creature does not checksum to the creature that
was scored. Either means the thing measured is not the thing about to be
published.

*Retry:* yes, from a fresh fetch. If it recurs, something is rewriting files
underneath the run.

### A sampled screen offered as the verdict

Sample-mode scoring is explicitly non-authoritative and may not decide
population re-entry. Rebase refuses rather than promoting on a cheap number.

*Retry:* run the full mode.

## What is never a failure

* The champion already carrying some or all of the bundle.
* A candidate scoring worse than the champion.
* A candidate scoring exactly the same as the champion.
* An empty cohort because every enhancement was already incorporated.

## Diagnosing an unattended run

`rebase.json` carries the whole story: the opening ancestor, the fresh
champion, the corpus, every enhancement's fate with its reason, every candidate
constructed, everything dropped for the cap, and the verdict with the scorer's
own numbers and identity.

`experiments.jsonl` carries the same facts as append-only records, so a run
that died part-way still leaves everything it had decided up to that point.
