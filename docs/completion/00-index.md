# Teslatlas Hub completion dossier

## Purpose

This is the current route to a real backend replacement for the pinned
TeslaMate reference. It is not a release signoff and does not authorize a
TeslaMate cutover.

The dossier is current as of 2026-07-31. It records facts separately from
planned work. Existing detailed contracts remain authoritative.

## Read this order

1. [Current status](01-current-status.md)
2. [Definition of 100 percent](02-definition-of-100-percent.md)
3. [Remaining engineering](03-remaining-engineering.md)
4. [Migration and cutover](04-migration-and-cutover.md)
5. [macOS arm64](05-platform-macos-arm64.md)
6. [Debian arm64](06-platform-debian-arm64.md)
7. [Verification and evidence](07-verification-and-evidence.md)
8. [Execution order](08-execution-order.md)
9. [Known risks](09-known-risks.md)

## Status vocabulary

| Label | Meaning |
| --- | --- |
| Proven | Current, scoped evidence exists for the exact claim. |
| Local | Exercised locally, not a release or native-platform claim. |
| Mock | Simulator, fixture, or fake-provider result only. |
| Live | Real source or owner vehicle was observed. Scope and time still matter. |
| Unverified | Code, design, or intent without a qualifying evidence record. |
| Blocked | Cannot close until the named external input or human action occurs. |

## Boundaries

TeslaMate source, database, services, containers, Docker, schedules,
configuration, and credentials are read-only inputs. Hub must never mutate
them. Cutover is a later manual operator decision.

The reference is TeslaMate `4.1.0-dev` at
`7054517c10475f39f480edeae8f90c6f717985a3`. Scope is Rust backend parity for
Teslatlas. Phoenix, Grafana, TeslaMate web UI, and vehicle commands are out of
scope. MQTT and updater are explicit product decisions, not hidden omissions.

## Existing authorities

- [Evidence matrix](../COMPLETION_EVIDENCE_MATRIX.md)
- [Final signoff rules](../FINAL_SIGNOFF.md)
- [Platform matrix](../PLATFORM_INSTALL_MATRIX.md)
- [Full rehearsal](../FULL_PARITY_REHEARSAL.md)
- [Roadmap map](../../roadmap/000-map.md)
