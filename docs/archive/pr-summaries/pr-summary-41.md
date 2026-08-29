## Summary

Documentation only. `README.md` gains a **Where this sits in the literature**
section naming the prior art Rebase re-derives, and `docs/integration.md`
cross-references the racing citations at the point where screening enters a
producer's flow. Closes #41.

The section leads with stale-update reconciliation — Hogwild! (Recht et al.
2011), DistBelief (Dean et al. 2012) and DC-ASGD (Zheng et al. 2017), the
closest prior art, since DC-ASGD corrects a delayed update for the base having
moved and that is Rebase's problem statement with structure in place of weights.
It then covers patch transplantation (Barr et al. 2015; Petke et al. 2018),
optimistic concurrency control (Kung & Robinson 1981), asynchronous /
steady-state EAs, and racing (Maron & Moore 1994; Birattari et al. 2002;
Jamieson & Talwalkar 2016; Li et al. 2017), with the racing entry pointing at
#42 for the power problem it raises in practice. It states plainly that the
mechanism is novel *as an artefact*, not *as an idea*.

The knot stays: `README.md` still opens with **Rebase the improvement, don't
replace the champion**, and 🪢 is untouched in `rebase/src/tags.rs`.

## Evidence

No web interface to screenshot — this is a documentation change to two Markdown
files. What was run instead:

- `markdownlint-cli2` (the repo's `markdown-lint.yml` gate) — `Summary: 0 issues
  in 0 files` across 8 files.
- `./quality.sh < /dev/null` — `All quality checks passed!` (fmt, clippy
  `-D warnings`, full workspace test suite, rustdoc).
- `cargo test --workspace tags` — `tags::tests::the_rebase_tag_uses_the_knot_emoji
  ... ok`, the existing pin that proves the knot survived
  (`rebase/src/tags.rs:352`).

Both new relative links resolve against the committed tree:
`docs/integration.md` → `rebase-protocol.md#4b-screening-and-the-trap-in-it`
(heading at `docs/rebase-protocol.md:75`) and →
`../README.md#where-this-sits-in-the-literature` (heading added in this change).

## Test Plan

No tests added or modified. The change is prose; the only assertion worth making
about it — that the knot is still the `rebase` tag's emoji — is already covered
by `rebase/src/tags.rs::tests::the_rebase_tag_uses_the_knot_emoji`, which calls
the real tag builder and asserts on its output. A test that grepped `README.md`
for the new heading would be source-text inspection, which the project's testing
guidance rules out; `markdownlint-cli2` and `quality.sh` are the gates that
actually cover a docs change.
