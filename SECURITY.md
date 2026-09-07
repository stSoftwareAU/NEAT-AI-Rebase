# Security Policy

## Reporting a vulnerability

Please report security issues privately to the maintainers at
<https://github.com/stSoftwareAU/NEAT-AI-Rebase/security/advisories/new> rather
than in a public issue.

## Internal escalation — a security job that goes red

Reporting above is the inbound path. This is the outbound one: what happens when
one of this repository's own security gates fails.

**Triage owner: `@stSoftwareAU/developers`**, the CODEOWNERS default. There is no
separate on-call rota — the team that reviews a change to `.github/` is the team
that answers for its gates.

* `.github/workflows/cargo-audit.yml` runs unattended at 06:00 UTC every Monday
  with no pull request and nobody watching. When that run fails, its `notify`
  job opens an issue titled *"cargo audit failed on the scheduled run"*, or
  comments on the one already open, so the failure arrives as a notification
  rather than as a red tick on the Actions tab. Triage it in the next working
  day: upgrade past the advisory, or — when it genuinely cannot be fixed —
  ignore that one ID in `.cargo/audit.toml`, add the same ID to `deny.toml` so
  both gates agree, and say why in the pull request.
* `.github/workflows/gitleaks.yml`, `semgrep.yml`, `dependency-review.yml` and
  the pull-request run of `cargo-audit.yml` need no notification of their own: a
  failure blocks the merge in front of the author, who triages it. A gitleaks
  hit is treated as a live credential — rotate first, then clean the history.
* `.github/workflows/sbom.yml` also runs on a schedule, at 07:00 UTC every
  Monday, but needs no notification either: the same job runs on every pull
  request, so a break in SBOM generation goes red in front of an author rather
  than waiting for the weekly run to be noticed.

## Emergency override — an advisory under active exploitation

The routine dependency path is deliberately slow. `cargo-audit.yml` and
`cargo-upgrade.yml` both wake at 06:00 UTC on a Monday, every bump is judged by
`ci.yml`, `dependency-review.yml` and `cargo deny check` before it merges, and
`scripts/crates-quarantine.sh` refuses any crates.io version younger than 24
hours. That is the right default for a routine week and the wrong one when a
CVE against a locked dependency is being exploited today. This is the documented
way out of the cadence, so nobody has to invent one under pressure.

`@stSoftwareAU/developers`, the CODEOWNERS default and the triage owner named
above, decides that an advisory warrants this path. Then:

1. **Raise the bump out of cadence.** `cargo-upgrade.yml` declares
   `workflow_dispatch` for exactly this — `gh workflow run cargo-upgrade.yml`.
   It checks out `Develop` whatever ref launched it, verifies the bump with
   `cargo deny check` and the suite, and opens `chore/cargo-upgrade`: the same
   gated path as the Monday run, just today. When the advisory sits in a
   transitive dependency the blanket bump does not move, prepare the branch by
   hand instead — `cargo update -p <crate> --precise <version>` — and open the
   pull request yourself. Everything below applies either way.

2. **Expect the publish-age quarantine to stop you, and override it in the
   open.** A fix released hours ago is precisely what
   `scripts/crates-quarantine.sh` holds back: a version younger than the
   24-hour window fails the run, and no pull request is raised. That gate
   exists to catch a compromised release, and an exploited CVE is the one case
   where waiting the window out is the larger risk. Take the exception by
   preparing the branch by hand as above — never by lowering `--hours` in
   `cargo-upgrade.yml`, which would relax the window for every future weekly
   run. Record in the pull request the advisory ID, the crate and version, that
   the quarantine was overridden, and what was checked in its place: the
   upstream tag or release diff for the version being pulled in, and that it
   was published by the crate's usual account.

3. **Fast-track the review, not the gates.** Request review from
   `@stSoftwareAU/developers` and say in the pull request that the advisory is
   under active exploitation. What is accelerated is the human wait — a
   maintainer other than the author reviews it the same day rather than at the
   next convenient moment. The automated gates are not accelerated and are not
   waived: `ci.yml`, `dependency-review.yml`, `cargo-audit.yml` and
   `cargo deny check` all run on the pull request and all still have to be
   green. Do not
   merge with `gh pr merge --admin`, and do not disable a required check to get
   the merge through — shipping an unreviewed dependency in the middle of an
   incident trades one compromise for another.

4. **When no fixed version exists yet.** There is nothing to fast-track, so
   reduce exposure instead and keep the advisory visible: ignore that one ID in
   `.cargo/audit.toml`, add the same ID to `deny.toml` so both gates agree, and
   say in the pull request why it cannot be fixed and what would remove the
   ignore. An ignored ID is a tracked debt, not a resolution — the next
   scheduled `cargo-audit.yml` run stays green, so nothing else will remind you.

## Scope

NEAT-AI-Rebase is an experimental research tool. It reads creature JSON,
enhancement JSON and a binary training corpus from paths the operator supplies,
and it spawns the NEAT-AI-scorer binary the operator names. It opens no network
connections and holds no credentials.

Treat enhancement bundles from a source you do not control as untrusted input:
they are parsed, and their payloads drive graph construction. Everything they
produce still has to pass `neat_core::creature_validate` and compile before it
can be scored, and nothing is emitted without an authoritative scorer verdict —
but the parsing surface is the place to look first.

## Supported versions

The `Develop` branch is the only supported version while the project is
experimental.
