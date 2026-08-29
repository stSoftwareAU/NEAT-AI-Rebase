# Add Cargo Security Audit workflow

## Summary

Adds `.github/workflows/cargo-audit.yml`, which audits the committed
`Cargo.lock` against the RustSec advisory database on every pull request and at
06:00 UTC every Monday, failing on any advisory affecting a locked dependency.
Closes #17.

This overlaps the `cargo deny check` step in `ci.yml` on purpose, and the
overlap is not the point — the **schedule** is. `cargo deny check` only runs
when someone opens a PR, so an advisory published against a dependency that is
already resolved and locked goes unnoticed until the next PR happens to land.
This job finds it on the following Monday with no code change at all.

| | `cargo deny check` (ci.yml) | `cargo audit` (this PR) |
| --- | --- | --- |
| Trigger | PR / push to `Develop` | PR **and** weekly cron |
| Scope | licences, bans, sources, advisories | RustSec advisories, yanked crates |
| Needs a resolved build | yes (sibling checkout) | no — reads `Cargo.lock` |

Deliberate deviations from the template in the issue, all matching conventions
`ci.yml`, `dependency-review.yml` and `markdown-lint.yml` already set:

- **`taiki-e/install-action` with `tool: cargo-audit@0.22.2`, not
  `cargo install cargo-audit`.** This is exactly how `ci.yml` installs
  cargo-deny. `cargo install` rebuilds the auditor from source on every run,
  and — being unpinned — would let a future cargo-audit release change what
  this gate enforces without a diff.
- **Toolchain pinned to `1.98.0` at the same `dtolnay/rust-toolchain` SHA
  `ci.yml` uses**, not `stable` at a second SHA. `rust-toolchain.toml` pins the
  channel for anything run inside the checkout, so installing `stable` would
  only have made rustup download 1.98.0 a second time.
- **No `setup-neat-core` sibling checkout.** `cargo audit` parses `Cargo.lock`
  and never resolves or builds the workspace, so the `../NEAT-AI-core` path
  dependency `ci.yml` needs is irrelevant here. Verified below rather than
  assumed.
- **`persist-credentials: false`**, **`timeout-minutes: 15`**, a
  **`concurrency` group** and **`workflow_dispatch`** — the job reads the
  working tree and writes nothing back, a wedged download cannot hold a runner,
  and superseded pushes cancel.

Pins were verified against the GitHub API before committing, not copied on
trust:

| Pin | Verified |
| --- | --- |
| `actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1` | `git/ref/tags/v7.0.1` resolves to that commit; v7.0.1 is the latest release |
| `dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8` | commit exists in that repository; identical to the pin already in `ci.yml` |
| `taiki-e/install-action@7a79fe8c3a13344501c80d99cae481c1c9085912` | `git/ref/tags/v2.81.10` resolves to that commit; identical to the pin already in `ci.yml`, so both gates install through the same code rather than introducing a second version of one action |
| `cargo-audit@0.22.2` | latest release, published 2026-06-05, outside the 24h quarantine; present in the pinned install-action's `manifests/cargo-audit.json` with a per-platform hash |

`CONTRIBUTING.md` now lists five CI-only gates instead of four, and says how to
reproduce this one and what to do with a finding.

```mermaid
flowchart TD
    A[pull_request] --> C
    B[cron 06:00 Mon UTC] --> C[checkout<br/>no credentials]
    C --> D[rust-toolchain 1.98.0]
    D --> E[install-action<br/>cargo-audit@0.22.2 prebuilt]
    E --> F[cargo audit<br/>reads Cargo.lock only]
    F --> G{RustSec advisory on a<br/>locked dependency?}
    G -- yes --> H[exit 1 — PR blocked /<br/>scheduled run fails]
    G -- no --> I[exit 0]
```

## Evidence

No web interface to screenshot — this is a CI configuration change, verified by
running the pinned auditor itself rather than by reading its YAML.

**Workflow lints clean:**

```text
$ actionlint .github/workflows/cargo-audit.yml
actionlint: OK
```

**Positive path — this repository, exit 0.** cargo-audit at the pinned 0.22.2,
run over this checkout:

```text
$ cargo audit
    Fetching advisory database from `https://github.com/RustSec/advisory-db.git`
      Loaded 1226 security advisories
    Scanning Cargo.lock for vulnerabilities (56 crate dependencies)
EXIT=0
```

**The sibling checkout really is unnecessary** — the failure mode worth ruling
out, since `neat-core` is a `path` dependency and `ci.yml` has to check it out.
The same binary was run against a directory containing only `Cargo.toml`,
`rebase/Cargo.toml` and `Cargo.lock`, with no `../NEAT-AI-core` anywhere:

```text
$ cargo-audit audit          # /tmp/isolated — no sibling, no sources, no target/
    Scanning Cargo.lock for vulnerabilities (56 crate dependencies)
EXIT=0
```

**Negative path — a real advisory, exit 1.** A workflow that never fails is not
a gate, so the same binary was pointed at a lockfile pinning `time 0.1.44`:

```text
$ cargo-audit audit
Crate:     time
Version:   0.1.44
Title:     Potential segfault in the time crate
ID:        RUSTSEC-2020-0071
Severity:  6.2 (medium)
Solution:  Upgrade to >=0.2.23
error: 1 vulnerability found!
EXIT=1
```

**The documented escape hatch works, and it is `.cargo/audit.toml` — not
`deny.toml`.** Re-running the vulnerable lockfile with
`[advisories] ignore = ["RUSTSEC-2020-0071"]` in `.cargo/audit.toml` suppresses
the finding and exits 0, which is why `CONTRIBUTING.md` tells contributors to
add such an ID in **both** files rather than assuming cargo-audit reads the
cargo-deny config.

**The install path resolves.** `manifests/cargo-audit.json` at the pinned
install-action SHA lists `latest -> 0.22.2` with per-platform checksums
(including `x86_64_linux_musl`, what `ubuntu-latest` installs), so the pinned
action can install the pinned version without falling back to a source build.

**Repository gate is green** — `./quality.sh < /dev/null` ends with
`All quality checks passed!` (bash syntax, shellcheck, cargo-deny,
`cargo fmt --check`, clippy with `-D warnings`, 15 workspace tests plus 2
doctests, doc build). `markdownlint-cli2` reports 0 issues over 8 files,
including the edited `CONTRIBUTING.md`.

## Test Plan

No Rust tests were added, following the precedent set by
`docs/archive/pr-summaries/pr-summary-13.md` through `pr-summary-16.md`: the
deliverable is a GitHub Actions configuration file, and a Rust test asserting on
its YAML text would inspect source rather than verify behaviour — it would pass
on a workflow that never runs and break on a harmless edit. The behaviour was
pinned by executing the real auditor instead:

- `actionlint .github/workflows/cargo-audit.yml` — parses and validates the
  workflow, its expressions and all three action references.
- cargo-audit 0.22.2 against this checkout — 56 dependencies scanned, 0
  advisories, exit 0.
- The same binary against a `Cargo.lock`/`Cargo.toml` pair with no sibling
  `NEAT-AI-core` and no sources — exit 0, proving the workflow needs no build.
- The same binary against a lockfile pinning `time 0.1.44` — RUSTSEC-2020-0071
  reported, exit 1. The gate blocks.
- The same lockfile with the advisory ignored in `.cargo/audit.toml` — exit 0,
  confirming the documented exception mechanism.
- Registry verification of all three action pins via `git/ref/tags` on the
  GitHub API, and of `cargo-audit@0.22.2` in the pinned install-action manifest.
- `markdownlint-cli2@0.23.2` — 0 issues.
- `./quality.sh < /dev/null` — full repository gate, passing.
