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
