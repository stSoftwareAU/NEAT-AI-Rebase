# Integrate Ockham: emit successful removals and rebase at re-entry

## Summary

Ockham had a consumer-side adapter (`rebase/src/ockham.rs`, Issue #3) but no
**producer-side** path: a run could prove a prune and had nowhere portable to
put it, so the discovery lived only inside a local descendant — and publishing
that descendant is exactly the stale republish this project exists to stop.

This change adds `rebase/src/prune_log.rs`. `PruneLog` records the run's
opening facts once — the ancestor's SHA-256, its authoritative score, the
corpus identity and the creature's widths — and stamps them on every accepted
prune, which becomes a v1 `ockhamRemoval` enhancement in acceptance order.
`write_bundle` files them; re-entry is then the ordinary Rebase path with a
**freshly fetched** champion, so Ockham's local incumbent is never assumed to
still be globally current.

Filing fails closed on the mistakes that would otherwise surface an hour later
as an un-replayable bundle: a non-finite score or mean, a UUID the opening
creature never carried, a target that is not hidden, and the same removal filed
twice. It deliberately does *not* re-derive the removal against the ancestor —
replay happens on the fresh champion by definition, and an already-accepted
prune must not be lost to a re-derivation on the wrong creature.

Ockham's own replay path is untouched and stays available behind its switch;
`docs/integration.md` now states what evidence retires that duplication. Wiring
NEAT-AI-Ockham itself remains deliberately separate, as the README has always
said: it changes a running optimiser and lands with its own feature switch and
its own evidence.

Closes #8.

## Evidence

Backend/CLI change with no web interface, so no screenshot applies. The
evidence is the test suite: `rebase/tests/ockham_reentry.rs` drives the real
CLI over real creature JSON, a real `.bin` corpus and its computed identity,
the real engine and the real staging/emission path — only the scorer is
scripted.

```mermaid
sequenceDiagram
    autonumber
    participant O as Ockham run
    participant L as PruneLog
    participant P as Population
    participant R as Rebase
    participant S as Scorer
    O->>P: fetch champion → A
    O->>L: opening(producer, A, baseScore, corpusIdentity)
    Note over P: the fleet evolves A → B independently
    O->>L: accept("h1", meanAblation{mean}, improvedScore)
    O->>L: write_bundle(bundle.json)
    O->>P: fetch champion again → B
    O->>R: --champion B --enhancements bundle.json
    R->>R: absent already? → alreadyPresent; else replay onto a clone of B
    R->>S: score B and every rebased candidate
    S-->>R: verdict
    R-->>O: population-candidate.json, only when B + Δ beat B
```

Full gate, clean:

```text
$ ./quality.sh < /dev/null
…
test result: ok. 120 passed; 0 failed  (unit)
test result: ok. 7 passed; 0 failed    (tests/ockham_reentry.rs)
test result: ok. 15 passed; 0 failed   (tests/race_conditions.rs)
test result: ok. 2 passed; 0 failed    (doc-tests)
All quality checks passed!
```

Each acceptance criterion maps to a test that asserts on the run's real
outputs — `population-candidate.json`, `rebase.json` and `experiments.jsonl` —
not on how the result was reached:

| Acceptance criterion | Test |
| --- | --- |
| Race fixture preserves the unrelated improvement and the compatible prune | `a_prune_filed_on_a_replays_onto_the_fresh_champion_and_keeps_the_fleets_work` |
| An already-removed UUID neither fails nor duplicates work | `a_uuid_the_fleet_already_pruned_is_already_incorporated_and_costs_nothing` |
| A conflicting prune fails closed and does not replace the fresh champion | `a_conflicting_prune_fails_closed_and_the_fresh_champion_stands`, `a_conflicting_prune_does_not_take_the_compatible_one_with_it` |
| Results journalled with clear provenance | `every_outcome_is_journalled_with_provenance`, `the_filed_bundle_carries_the_opening_checksum_score_and_corpus_identity` |

Two details worth a reviewer's eye:

* "no duplicate work" is asserted by handing those runs a scorer that **errors
  if it is invoked at all**, so a run that spent a corpus pass on an
  already-absent UUID fails the test rather than passing quietly.
* the race test asserts `openingChecksum != championChecksum`, and that the
  opening checksum is the ancestor the log recorded — the machine-checkable
  form of "re-entry used a freshly fetched champion, not the local incumbent".

## Test Plan

Added:

* `rebase/src/prune_log.rs` unit tests (8) — the opening facts land on every
  filed prune; acceptance order and the documented `ockhamRemoval` wire form
  survive a JSON round trip; a duplicate removal, an unknown UUID, a non-hidden
  target and non-finite scores/means each fail closed; an empty run writes no
  bundle; and a filed bundle replays through the consumer-side engine.
* `rebase/tests/ockham_reentry.rs` (7) — the end-to-end suite in the table
  above, including that the champion is scored authoritatively alongside the
  rebased candidates before anything is emitted.

Unchanged: no existing test was modified or removed; `race_conditions.rs` and
every other suite still pass.

Documentation updated in the same change: `docs/integration.md` (Ockham worked
example rewritten around `PruneLog`, with a sequence diagram and the
keep-your-old-path-behind-a-switch evidence bar), `docs/enhancement-format.md`
(how removals are filed) and `README.md` (status).

## Pre-PR security self-check

* Input validation: `PruneLog::opening`/`accept` validate every externally
  supplied value — finite scores and means, a UUID that exists in the opening
  creature, a neuron that is hidden — before anything is filed.
* Secrets: none staged; the change touches source, tests and docs only.
* Injection surface: no new SQL, shell, or HTTP calls. The one new filesystem
  write is `write_bundle`, to a caller-supplied path, with the error surfaced
  as `PruneLogError::Write` rather than swallowed.
* Error handling: every failure path returns a typed error naming the fault; no
  `unwrap` on caller input, and no silent fallback.
* Dependencies: none added.
