# The v1 enhancement format

An **enhancement** is a portable description of something an optimiser
changed. It is not a creature, and it is not a diff between creatures. It is
the semantic change, written down so that it can be replayed onto a champion
the producer has never seen.

Everything here is stable API. A producer writing this format today will be
read by future versions of Rebase, or refused loudly — never half-understood.

## The envelope

```json
{
  "meta": {
    "version": 1,
    "id": "f6d7ec825b32febe",
    "producer": "neat-ai-forests/0.1.17",
    "baseChecksum": "9a2f7c4e18b0d35a6f1c9e2b7d4a8305c6e1f0b9d2a7c4e18b0d35a6f1c9e2b7",
    "baseScore": 0.81234,
    "improvedScore": 0.812905,
    "corpusIdentity": "3f2a1b0c9d8e7f65",
    "inputCount": 42,
    "outputCount": 1
  },
  "payload": { "kind": "…", "…": "…" }
}
```

| Field | Meaning |
| --- | --- |
| `version` | Envelope format version. Must be `1`. Anything else is refused. |
| `id` | Stable identity of the semantic change — see [Identity](#identity). |
| `producer` | Who produced it, `name/version`. Evidence only. |
| `baseChecksum` | SHA-256 of the JSON bytes of the creature the producer opened on. |
| `baseScore` | Authoritative score of that opening creature. |
| `improvedScore` | Authoritative score the producer measured **after** applying this change. |
| `corpusIdentity` | Identity of the corpus both scores were measured on. |
| `inputCount` / `outputCount` | Widths of the opening creature. |

`baseScore` and `improvedScore` are the producer's own numbers on the
producer's own creature. Rebase records them, reports the difference in its
journal, and **never uses either to promote anything**.

## Bundles

A run files its accepted changes as an ordered bundle:

```json
{
  "version": 1,
  "producer": "neat-ai-forests/0.1.17",
  "baseChecksum": "9a2f…e2b7",
  "baseScore": 0.81234,
  "corpusIdentity": "3f2a1b0c9d8e7f65",
  "enhancements": [ { "meta": {…}, "payload": {…} } ]
}
```

Order is the order the producer accepted them, and Rebase preserves it. The
cumulative prefixes it builds only mean something if "the first two" means the
same thing at both ends.

## Payloads

### `forestPatch`

A NEAT-AI-Forests residual-correction tree for one output, in the Forests wire
format — the same bytes Forests writes to its own journal.

```json
{
  "kind": "forestPatch",
  "patch": {
    "version": 1,
    "output": 0,
    "root": {
      "kind": "split",
      "condition": { "terms": [ { "feature": 17, "weight": 1.0 } ], "threshold": 0.25 },
      "left":  { "kind": "leaf", "correction": 0.0 },
      "right": { "kind": "leaf", "correction": 0.011 }
    },
    "provenance": {
      "strategy": "histogram-stump",
      "backend": "cpu",
      "predictedGain": 4.271,
      "affectedRecords": 18204,
      "searchRecords": 250000,
      "incumbentChecksum": "9a2f…e2b7"
    }
  }
}
```

* `output` is an output-neuron **index**, not a UUID: the champion's UUIDs are
  its own business, and an index survives the fleet renaming things.
* `feature` is an input observation index.
* A condition is `Σ weight·x > threshold` → right branch, accumulated in `f32`
  in term order with `1.0 · −threshold` added last. `NaN` therefore always
  falls left, exactly as it does inside the creature's own `IF` kernel.
* `provenance` is documentation. It is excluded from the id.

Producers do not have to build this envelope by hand: `PatchLog` (see
[`integration.md`](integration.md)) records the opening facts once and stamps
them on every accepted patch, filing the patch **as accepted** so the id stays
the one the graft uses. It refuses a non-finite score, a patch version it does
not implement, a non-finite weight, threshold or leaf, a bare-leaf root, an
output or feature index the opening creature does not have, a condition that
names one feature twice, and the same patch filed twice.

### `ockhamRemoval`

A scorer-proven NEAT-AI-Ockham removal, by neuron UUID plus the strategy needed
to reproduce it.

```json
{
  "kind": "ockhamRemoval",
  "neuronUuid": "b7c1f0d2-3e4a-4b5c-8d9e-0f1a2b3c4d5e",
  "strategy": "meanAblation",
  "mean": 0.03125
}
```

| `strategy` | Extra field | What is reproduced |
| --- | --- | --- |
| `meanAblation` | `mean` | `bias_j += mean · w_ij` for every outgoing synapse, then cascade cleanup. Approximate. |
| `identityCollapse` | — | Fold a hidden IDENTITY neuron's bias downstream and bypass `x → y → z` as `x → z` with the product weight, merging into a parallel synapse where one exists. Exact. |

The strategy is part of the enhancement because the two produce **different
creatures**. Rebase reproduces the one that was accepted, or refuses. It never
substitutes the other.

Producers do not have to build this envelope by hand: `PruneLog` (see
[`integration.md`](integration.md)) records the opening facts once and stamps
them on every accepted prune, refusing a non-finite score or mean, a UUID the
opening creature never carried, a target that was not hidden, and a removal
filed twice.

## Identity

`meta.id` names the *change*, not the run that found it. It is derived from the
payload alone:

| Kind | Derived from | Excluded |
| --- | --- | --- |
| `forestPatch` | `output` and the correction `root` | `provenance`, scores, producer, base checksum, corpus |
| `ockhamRemoval` | `neuronUuid` and the strategy name | the measured `mean`, scores, producer, base checksum, corpus |

Two producers that discover the same correction file the same id. That is the
whole basis of [idempotence](idempotence.md).

The forest id is the first 16 hex characters of the SHA-256 of
`serde_json::to_string(&(output, root))`, which is byte-identical to
`forests::patch::Patch::id`. It has to be: the graft names every neuron it
appends `forest-<id>-…`, so if the two ever disagree an already-applied patch
stops being recognised and gets grafted twice.

The `mean` is excluded because it is a *measurement of one corpus pass*, not
part of what was decided. The same neuron removed by the same strategy is the
same enhancement even when a later run measures a different mean.

Rebase checks `meta.id` against the id the payload actually has, before
anything is applied. A mis-filed id fails closed rather than defeating
idempotence.

## What v1 deliberately omits

No generic bias, weight or squash mutation. Their rebase semantics are not
explicit yet: "set weight *w* to 0.31" says nothing useful about a champion
that has independently retrained that weight, and guessing would produce
candidates whose meaning nobody can state. They stay out until they can be
defined and scorer-tested.

## Determinism

Field order is fixed by the struct definitions, ids are computed over a fixed
tuple rather than over a serialised document, and directories of enhancements
are read in file-name order. The same bundle therefore always produces the same
ids, the same cohort and the same labels — which is what makes a fixture worth
checking in.

Unknown fields are ignored, so a producer may add its own annotations without
breaking older readers. Unknown `kind` values and unknown `version` values are
**not** ignored: both fail closed.

## Regenerating the examples

The JSON above is printed by the crate itself, so it cannot drift from the
code:

```bash
cargo run --example print_bundle
```
