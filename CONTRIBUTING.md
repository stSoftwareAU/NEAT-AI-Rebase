# Contributing to NEAT-AI-Rebase

## Before you open a PR

Run the same gate CI runs:

```bash
./quality.sh
```

It needs `shellcheck`, `actionlint`, `jq` (`scripts/runlib.sh` parses
`cargo metadata` with it), and (optionally) `cargo-deny`. Everything else is
plain `cargo`: `neat-core` is a git dependency pinned to a NEAT-AI-core release
tag, so no sibling checkout is required.

`.github/workflows/ci.yml` runs that same gate on every PR into `Develop` and
on the sub-issue PRs that target a shared `milestone/<slug>` branch: the filter
lists `milestone/*` alongside `Develop`, because a workflow glob `*` stops at a
`/`. It has no `push:` trigger: as a required status check it already gates
every merge on the PR, so re-running it on the push to `Develop` would only
duplicate that run (Issue #57). Re-run it by hand with `workflow_dispatch` when
you need a fresh result on the default branch.

`.github/workflows/actionlint.yml` lints the workflow YAML itself with
[`actionlint`](https://github.com/rhysd/actionlint) — syntax, undefined
`${{ }}` context properties, bad `runs-on` labels, and shell bugs inside `run:`
blocks. It runs `./scripts/actionlint.sh`, the same script `quality.sh` calls,
so a workflow regression fails locally before it reaches CI. Every PR is
linted, including the sub-issue PRs that target a shared `milestone/<slug>`
branch: the filter lists `milestone/*` alongside `*`, because a workflow glob
`*` stops at a `/`. It has no `push:` trigger either, for the same reason
`ci.yml` has none — the PR run already gates the merge (Issue #83); re-run it
by hand with `workflow_dispatch` when you need a fresh result on the default
branch. Install the linter with
`go install github.com/rhysd/actionlint/cmd/actionlint@latest` or the
[documented download](https://github.com/rhysd/actionlint/blob/main/docs/install.md);
a missing `actionlint` fails the gate rather than skipping it.

CI adds seven gates `quality.sh` cannot run locally:

* `.github/workflows/gitleaks.yml` scans the PR's commit range for committed
  secrets and fails the PR if it finds one. Every PR is scanned, including the
  sub-issue PRs that target a shared `milestone/<slug>` branch: the filter
  lists `milestone/*` alongside `*`, because a workflow glob `*` stops at a
  `/`. Rotate anything it flags — rewriting the branch alone does not un-leak
  a credential.
* `.github/workflows/semgrep.yml` runs the Semgrep `p/default` static-analysis
  ruleset over the PR and fails on any blocking finding. Every PR is scanned,
  including the sub-issue PRs that target a shared `milestone/<slug>` branch:
  the filter lists `milestone/*` alongside `*`, because a workflow glob `*`
  stops at a `/`. Fix the finding; if it is a genuine false positive, silence
  that one line with a `# nosemgrep: <rule-id>` comment and say why in the PR
  description.
* `.github/workflows/dependency-review.yml` diffs the dependencies the PR adds
  against GitHub's advisory database and fails on any advisory, at any
  severity. It overlaps `cargo deny check` deliberately: cargo-deny audits the
  whole resolved graph from RustSec, this reports only what the PR introduces,
  and pinned GitHub Actions are covered too. Every PR is reviewed, including
  the sub-issue PRs that target a shared `milestone/<slug>` branch: the filter
  lists `milestone/*` alongside `*`, because a workflow glob `*` stops at a
  `/`. Upgrade past the advisory; if it
  is genuinely inapplicable, allow that one ID with `allow-ghsas` in the
  workflow and say why in the PR description.
* `.github/workflows/markdown-lint.yml` runs `markdownlint-cli2` over every
  Markdown file and fails on any violation. It needs no configuration flags —
  the globs, ignores and rule set all live in `.markdownlint-cli2.jsonc`, so
  `npx markdownlint-cli2@0.23.2` reproduces the CI result exactly. Every PR is
  linted, including the sub-issue PRs that target a shared `milestone/<slug>`
  branch: the filter lists `milestone/*` alongside `*`, because a workflow glob
  `*` stops at a `/`. It has no `push:` trigger: as a required status check it
  already gates every merge on the PR, so re-running it on the push to
  `Develop` would only duplicate that run (Issue #58) — use
  `workflow_dispatch` when you need a fresh result on the default branch. Fix
  the finding; disable a rule in that config only when it is genuinely noisy
  for this repository, and say why in the PR description.
* `.github/workflows/cargo-audit.yml` audits the committed `Cargo.lock`
  against the RustSec advisory database on every PR **and** at 06:00 UTC every
  Monday. The schedule is what `cargo deny check` cannot give you: an advisory
  published against a dependency that is already locked surfaces on the next
  Monday instead of waiting for someone to open a PR. "Every PR" includes the
  sub-issue PRs that target a shared `milestone/<slug>` branch: the filter
  lists `milestone/*` alongside `*`, because a workflow glob `*` stops at a
  `/`. Reproduce it with
  `cargo install cargo-audit --version 0.22.2 && cargo audit` — it reads
  `Cargo.lock` only, so it needs no build. Upgrade past the advisory; if it
  genuinely cannot be fixed, ignore that one ID in `.cargo/audit.toml`
  (cargo-audit does not read `deny.toml`),
  add the same ID to `deny.toml` so both gates agree, and say why in the PR
  description. A failed *scheduled* run has no PR to fail, so its `notify` job
  opens — or comments on — an issue titled "cargo audit failed on the scheduled
  run" instead of leaving a red tick nobody is paged for (Issue #94). Who
  triages it, and how fast, is the "Internal escalation" section of
  `SECURITY.md`; `rebase/tests/security_alerting.rs` holds both halves.
* `.github/workflows/sbom.yml` publishes a CycloneDX bill of materials for the
  crate on every PR **and** at 07:00 UTC every Monday, uploading it as the
  `sbom-<sha>` build artefact so a downstream consumer of this public
  repository has a machine-readable dependency manifest without resolving one
  by hand (Issue #96). The SBOM is deliberately not committed: it is derived
  from `Cargo.lock` and `cargo metadata`, so a checked-in copy is stale the
  moment a dependency moves. Reproduce it with
  `cargo install cargo-cyclonedx --version 0.5.9 && ./scripts/generate-sbom.sh`
  — that is the same script the workflow runs, and it needs no sibling
  checkout: `cargo cyclonedx` resolves the workspace through `cargo metadata`,
  which fetches the pinned `neat-core` tag like any other dependency. It writes
  `sbom/<package-directory>.cdx.json` and
  exits non-zero when nothing was produced or what was produced is not a
  CycloneDX document naming cargo components; `rebase/tests/sbom_gate.rs` drives
  every one of those paths.
* `.github/workflows/version-increment.yml` runs `scripts/auto-version.sh` on
  every PR that touches `rebase/src/**`, `rebase/Cargo.toml`, `Cargo.lock`, the
  bump script or the workflow itself, and commits the patch bump back onto the
  PR branch when the version is still level with the base branch (Issue #106).
  A version already ahead is left alone; a version *behind* the base fails the
  job, because the fleet rebuilds off the crate version and a downgrade reuses
  a version it has already built. Reproduce the decision locally with
  `./scripts/auto-version.sh rebase/Cargo.toml <base-version> Cargo.lock`.
  `.github/workflows/release.yml` is the other half and runs after the merge,
  not on the PR: a push to `Develop` touching `rebase/Cargo.toml` cuts tag
  `v<version>` and a GitHub release when neither exists yet, so a re-run is a
  no-op. `rebase/tests/auto_version.rs` and
  `rebase/tests/version_release_workflows.rs` hold both halves.

Every workflow that triggers on `pull_request` declares a `concurrency:` group
keyed by `${{ github.ref }}` with `cancel-in-progress: true`, so pushing again
to a PR cancels the run it supersedes instead of paying for a result nobody
will read (Issue #85). `rebase/tests/workflow_concurrency.rs` holds this for
every PR-triggered workflow, so a new gate cannot be added without it.

A docs-only PR does not pay for the Rust build, the audits or the scanners
(Issue #123). Each PR checker except gitleaks opens with a small `changes` job
(a SHA-pinned `dorny/paths-filter` step with `pull-requests: read` and nothing
else), and its expensive jobs `need` that job and run only when the verdict is
not `'false'`. Anything outside `docs/**` and `**/*.md` counts as code.
`markdown-lint.yml` inverts the check: it lints only when a Markdown file, its
`.markdownlint-cli2.jsonc` config or the workflow itself changes. The gate is a
job-level `if:`, never a workflow-level `paths:` filter. A skipped job still
reports success, so a required check is satisfied, whereas a workflow that never
runs reports nothing and blocks the merge. The gate also fails open: on a
schedule, on `workflow_dispatch`, or when the classification itself fails, the
output is empty and every job runs. Gitleaks stays on for every PR, because a
secret can land in a docs page as easily as in code.
`rebase/tests/workflow_change_gates.rs` holds this, including the file lists
each filter must and must not fire on.

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

`neat-core` is a git dependency pinned to a NEAT-AI-core **release tag**
(Issue #107), so a core release never reaches this repository unannounced. The
pin moves only through this repository's own PRs: the family-sync step in
`.github/workflows/version-increment.yml` runs `scripts/family-pins.sh`, which
rewrites the tag to core's newest release and re-locks `Cargo.lock`, and the
same job's single commit carries the moved pin and the crate-version bump. A
breaking core release turns that PR's CI build red, which is where it is
handled — with `ACTIONS_PUSH` configured the sync commit is built by its own
run, and without it by the next push to the PR, because GitHub suppresses the
runs a `GITHUB_TOKEN` push would start.

`scripts/runlib.sh` and `scripts/family-pins.sh` are owned by NEAT-AI-core: each
is a byte-identical copy of the file at the same path on core's `Develop`.
Never edit them here — make the change in NEAT-AI-core, and the family-sync step
copies it back on the next gated PR.

New third-party dependencies need a reason in the PR description and must pass
`cargo deny check`.

Third-party crates are refreshed for you: `.github/workflows/cargo-upgrade.yml`
runs `cargo upgrade --incompatible=ignore --pinned=ignore` plus `cargo update`
at 06:00 UTC every Monday and opens `chore/cargo-upgrade` against `Develop`
with the result. Reproduce it with
`cargo install cargo-edit --version 0.13.13 --locked` and the same two
commands. Two things it deliberately does not do: it never bumps a
semver-incompatible requirement (a major bump is a code change, so it stays a
hand-written PR — the run logs the crate as `incompatible`), and it never
rewrites the `neat-core` git tag, which moves only through
`scripts/family-pins.sh`.
The scheduled run verifies its own bump with `cargo deny check` and the test
suite before raising the PR, so a broken upgrade fails the run instead of
arriving as a pull request.

It also refuses to propose a crate version published in the last 24 hours.
`scripts/crates-quarantine.sh` reads the `created_at` of every crates.io
version the bump newly resolved and fails the run when one is younger than the
window — `cargo deny check` judges advisories, not recency, so without it a
compromised release could be proposed before anyone had a chance to flag it.
Run it by hand the same way the workflow does:

```bash
git show HEAD:Cargo.lock > /tmp/Cargo.lock.baseline
./scripts/crates-quarantine.sh \
  --baseline-lockfile /tmp/Cargo.lock.baseline --lockfile Cargo.lock --hours 24
```

It exits 0 when clear, 1 on a quarantined version and 2 when it could not read
a publish date — an unreachable crates.io is never reconciled as a pass.
Internal `stSoftwareAU` crates are exempt via `--exempt`; none are consumed
from crates.io today, since `neat-core` is a `path` dependency.

The weekly slot and that 24-hour window are the routine cadence, not a law: an
advisory under active exploitation needs a same-day bump, and the emergency
override in `SECURITY.md` is the documented way to take one (Issue #95). It
dispatches this same workflow by hand, says how to take the quarantine
exception on a single bump rather than by widening the window, and holds the
PR gates — none of them are waived to go faster.

`.github/dependabot.yml` registers the cargo ecosystem with Dependabot on the
same weekly slot (Issue #93). It is the committed anchor for the *alerting*
channel — GitHub's own advisory feed, surfaced on the repository's Security tab
— which `cargo-audit.yml` cannot give you: that job scans `Cargo.lock` against
RustSec and reports in a CI log. Whether alerts are enabled is a repository
setting no file can prove, so the configuration is the only signal a reviewer
can read from the tree. It deliberately sets `open-pull-requests-limit: 0`:
`cargo-upgrade.yml` is the single bump path because it is the one that applies
the 24-hour publish-age quarantine, and a second weekly bumper without that
window would propose exactly the release the gate holds back. It also sets
`cooldown.default-days: 7` — Dependabot's own publish-age window, wider than
the script's 24 hours — so the quarantine still holds should that limit ever be
raised. Dependabot
security updates are a separate repository switch and are not governed by that
limit. `rebase/tests/dependabot_config.rs` holds both halves, so the
configuration cannot be dropped or quietly turned into a competing bumper.
