# Contributing to NEAT-AI-Rebase

## Before you open a PR

Run the same gate CI runs:

```bash
./quality.sh
```

It needs the sibling `../NEAT-AI-core` checkout, `shellcheck`, and (optionally)
`cargo-deny`. Everything else is plain `cargo`.

CI adds two gates `quality.sh` cannot run locally:

* `.github/workflows/gitleaks.yml` scans the PR's commit range for committed
  secrets and fails the PR if it finds one. Rotate anything it flags —
  rewriting the branch alone does not un-leak a credential.
* `.github/workflows/semgrep.yml` runs the Semgrep `p/default` static-analysis
  ruleset over the PR and fails on any blocking finding. Fix the finding; if it
  is a genuine false positive, silence that one line with a
  `# nosemgrep: <rule-id>` comment and say why in the PR description.

## What a change has to preserve

Rebase exists to stop useful discoveries being destroyed at population
re-entry. Four rules protect that, and a change that weakens any of them needs
to say so explicitly in its PR description:

1. **The scorer has the final say.** Previous success is evidence, never
   permission.
2. **The champion is never modified.** Every adapter clones.
3. **Idempotence beats host exclusion.** Presence is answered from the
   creature, not from who published it.
4. **Fail closed.** An unknown version, an unknown kind, a scorer that
   misbehaved — none of them may emit a candidate.

`rebase/tests/race_conditions.rs` encodes the regression the project exists to
prevent. If a refactor makes one of those tests awkward, that is the alarm, not
the inconvenience: the cheap way to make them pass is to republish the stale
descendant, which is the bug.

## Style

* Match the surrounding code: full `///` docs on every public item (the doc
  build runs with `-D warnings`), and comments that say *why* rather than
  restate the code.
* Tests are named for the behaviour they pin, not for the function they call.
* No application-domain terminology anywhere in the crate, its docs or its
  fixtures. Rebase is generic infrastructure.

## Dependencies

`neat-core` is an unpinned `path` dependency tracking head, gated by
`scripts/check-neat-core-version.sh` against the baseline recorded in
`neat-core.expected-version`. Handling a breaking bump means updating the code
**and** the baseline in one deliberate PR.

New third-party dependencies need a reason in the PR description and must pass
`cargo deny check`.
