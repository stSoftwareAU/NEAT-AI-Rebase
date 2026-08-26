# Security Policy

## Reporting a vulnerability

Please report security issues privately to the maintainers at
<https://github.com/stSoftwareAU/NEAT-AI-Rebase/security/advisories/new> rather
than in a public issue.

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
