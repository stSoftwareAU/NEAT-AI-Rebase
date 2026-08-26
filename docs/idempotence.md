# Idempotence

> If the latest champion already contains an enhancement, the enhancement is
> simply recognised as present.

That sentence is doing a lot of work. It is what lets Rebase run on **any**
champion, from any source, without knowing which host published it.

## Why not host exclusion?

The tempting design is: "skip the creature this host just published." It fails
in every interesting case. The champion may have arrived through another host's
rebase, through the fleet's own evolution, through a replay of a shared
learnings cache, or through a path nobody recorded. A rule about *who published
it* cannot answer a question about *what it contains*.

So Rebase asks the only question that generalises: does this creature already
carry this change?

## How each kind answers it

### Forest patches — a prefix scan

The patch id is a digest of the correction itself, and the graft names every
neuron it appends `forest-<patch id>-…`. Presence is therefore a prefix scan
over the champion's neuron UUIDs.

This works on a champion that has since evolved further — the check is a name
lookup, not a checksum comparison — and it works on a champion Rebase has never
seen before, because the name was derived from the patch rather than assigned
by a run.

Two producers that discover the same correction compute the same id and land on
the same names. The second one is a no-op.

### Ockham removals — is the neuron still there?

A removal is identified by neuron UUID, and the UUID is the whole story: if it
is already absent from the champion, the enhancement is already incorporated.
It does not matter how it went — Rebase, Ockham's own re-entry path, or the
fleet independently pruning it.

## What a producer must do

1. **Preserve the exact semantic change you accepted.** Not the resulting
   creature, not a summary, not a re-derivation. The bytes of the patch, or the
   UUID and strategy of the removal.
2. **Compute the id the same way.** For a Forest patch that means
   `Patch::id()`. Rebase re-computes the id from the payload and refuses an
   envelope whose `meta.id` disagrees, so a mis-filed id fails closed rather
   than silently producing a duplicate graft.
3. **Do not rename anything.** The identity is derived from the change; if you
   normalise, reorder or round the payload before filing it, you have filed a
   different enhancement.

## What "already present" is *not*

It is **not** an error, and it is **not** a reason to retry later. It is a
clean no-op, reported as `alreadyPresent` in `rebase.json` and the journal. A
run whose whole bundle is already present exits `3` with status `nothingToDo` —
a successful outcome, and the normal one when your own work has already reached
the population.

It is also **not** a claim that the enhancement is still beneficial. It says
the structure is there, nothing more.

## The one case it cannot see

A champion that carries an *equivalent* change built by a different mechanism —
say, evolution independently growing the same correction as ordinary structure
— is not detected as already carrying the patch. Rebase will graft the patch,
build the candidate, and let the scorer notice that it adds complexity for no
gain. That is the correct outcome: the scorer prices the duplication, and no
population candidate is emitted.
