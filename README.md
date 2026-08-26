# NEAT-AI-Rebase

**Rebase the improvement, don't replace the champion.**

NEAT-AI-Rebase is an experimental Rust project for preserving useful discoveries made by independent NEAT-AI optimisation processes when the global fittest creature changes while they are working.

A long-running optimiser may start from creature **A**, discover an improvement **Δ**, and finish after the fleet has already evolved to creature **B**. Publishing `A + Δ` can accidentally discard improvements accumulated in `B`.

Rebase instead treats the discovered change as the valuable artefact:

```text
A ── optimiser ──▶ A + Δ
│
└──────── fleet evolves ────────▶ B
                                  │
                                  └─ reapply Δ ─▶ B + Δ
                                                │
                                                └─ authoritative scorer decides
```

The scorer remains the final authority. A rebased candidate is never assumed to be better merely because the original change was successful.

## Goals

- Preserve compatible improvements discovered concurrently by different optimisation methods.
- Reapply semantic enhancements to the latest global champion rather than publishing stale descendants.
- Keep replay/rebase idempotent where possible: if an enhancement is already present, applying it again should be a no-op.
- Generate competing candidates when replay semantics are ambiguous, and let `NEAT-AI-scorer` decide.
- Fail closed on incompatible creatures, stale corpus identity, malformed enhancements, or scorer disagreement.
- Keep the mechanism generic. NEAT-AI-Rebase is not tied to any particular application domain.

## Initial scope

Version 1 deliberately starts with enhancement types whose semantics are clear:

1. **NEAT-AI-Forests grafts** — portable patches described in terms of inputs, output and correction tree.
2. **NEAT-AI-Ockham removals** — neuron UUID based removals that can be replayed only while the UUID is still present.

Weight/bias/squash modifications may follow later once their rebase semantics are explicit and scorer-tested.

## Proposed flow

1. Optimiser records the checksum and authoritative score of its opening creature.
2. Each accepted local improvement is recorded as a portable `Enhancement` rather than only as a resulting creature.
3. At population re-entry time, fetch the current global champion again.
4. Detect enhancements already present and skip them.
5. Apply the remaining enhancement(s) to the current champion.
6. Build a small candidate cohort, including useful prefixes/combinations when appropriate.
7. Score the current champion and all candidates together with `NEAT-AI-scorer`.
8. Emit a population candidate only when the scorer confirms a real improvement over the current champion.

## Design principles

### Semantic changes, not raw JSON diffs

Rebase should understand what an optimiser changed. A raw structural diff is fragile when the target creature has independently evolved.

### Idempotence beats host exclusion

Rebase should not need special logic to avoid the creature that a host just published. If the latest champion already contains an enhancement, the enhancement should simply be recognised as present.

### No assumption of additivity

Two changes that each helped separately may interact badly. Candidate combinations must be scored, not trusted.

### Scorer has the final say

Search heuristics, previous wins and provenance are evidence. Only an authoritative score determines population re-entry.

## Relationship to other projects

- [NEAT-AI-Forests](https://github.com/stSoftwareAU/NEAT-AI-Forests) discovers portable residual-correction grafts.
- [NEAT-AI-Ockham](https://github.com/stSoftwareAU/NEAT-AI-Ockham) discovers removable structure.
- [NEAT-AI-scorer](https://github.com/stSoftwareAU/NEAT-AI-scorer) is the authoritative judge.
- [NEAT-AI-core](https://github.com/stSoftwareAU/NEAT-AI-core) supplies the shared creature representation and validation logic.

## Status

Early experiment. The GitHub issues are the implementation plan.

The immediate target is a minimal end-to-end Forests rebase: capture an accepted Forest patch, fetch a newer champion, graft the patch onto it, score champion versus rebased candidate, and emit the winner safely.
