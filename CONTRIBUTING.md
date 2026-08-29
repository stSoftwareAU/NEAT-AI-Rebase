# Contributing to NEAT-AI-Rebase

## Before you open a PR

Run the same gate CI runs:

```bash
./quality.sh
```

It needs the sibling `../NEAT-AI-core` checkout, `shellcheck`, and (optionally)
`cargo-deny`. Everything else is plain `cargo`.

CI adds five gates `quality.sh` cannot run locally:

* `.github/workflows/gitleaks.yml` scans the PR's commit range for committed
  secrets and fails the PR if it finds one. Rotate anything it flags —
  rewriting the branch alone does not un-leak a credential.
* `.github/workflows/semgrep.yml` runs the Semgrep `p/default` static-analysis
  ruleset over the PR and fails on any blocking finding. Fix the finding; if it
  is a genuine false positive, silence that one line with a
  `# nosemgrep: <rule-id>` comment and say why in the PR description.
* `.github/workflows/dependency-review.yml` diffs the dependencies the PR adds
  against GitHub's advisory database and fails on any advisory, at any
  severity. It overlaps `cargo deny check` deliberately: cargo-deny audits the
  whole resolved graph from RustSec, this reports only what the PR introduces,
  and pinned GitHub Actions are covered too. Upgrade past the advisory; if it
  is genuinely inapplicable, allow that one ID with `allow-ghsas` in the
  workflow and say why in the PR description.
* `.github/workflows/markdown-lint.yml` runs `markdownlint-cli2` over every
  Markdown file and fails on any violation. It needs no configuration flags —
  the globs, ignores and rule set all live in `.markdownlint-cli2.jsonc`, so
  `npx markdownlint-cli2@0.23.2` reproduces the CI result exactly. Fix the
  finding; disable a rule in that config only when it is genuinely noisy for
  this repository, and say why in the PR description.
* `.github/workflows/cargo-audit.yml` audits the committed `Cargo.lock`
  against the RustSec advisory database on every PR **and** at 06:00 UTC every
  Monday. The schedule is what `cargo deny check` cannot give you: an advisory
  published against a dependency that is already locked surfaces on the next
  Monday instead of waiting for someone to open a PR. Reproduce it with
  `cargo install cargo-audit --version 0.22.2 && cargo audit` — it reads
  `Cargo.lock` only, so it needs neither a build nor the `../NEAT-AI-core`
  sibling. Upgrade past the advisory; if it genuinely cannot be fixed, ignore
  that one ID in `.cargo/audit.toml` (cargo-audit does not read `deny.toml`),
  add the same ID to `deny.toml` so both gates agree, and say why in the PR
  description.

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

Third-party crates are refreshed for you: `.github/workflows/cargo-upgrade.yml`
runs `cargo upgrade --incompatible=ignore --pinned=ignore` plus `cargo update`
at 06:00 UTC every Monday and opens `chore/cargo-upgrade` against `Develop`
with the result. Reproduce it with
`cargo install cargo-edit --version 0.13.13 --locked` and the same two
commands — it resolves the workspace, so the sibling `../NEAT-AI-core` checkout
must be present. Two things it deliberately does not do: it never bumps a
semver-incompatible requirement (a major bump is a code change, so it stays a
hand-written PR — the run logs the crate as `incompatible`), and it never
rewrites the `neat-core` path dependency, which cargo-edit reports as `local`.
The scheduled run verifies its own bump with `cargo deny check` and the test
suite before raising the PR, so a broken upgrade fails the run instead of
arriving as a pull request.
