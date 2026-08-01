# Completion evidence matrix

This matrix is the release ledger. A green unit test proves only its stated
boundary; it cannot close a row that requires a corpus, native host, or live
owner-authorized rehearsal.

| Release claim | Reference and required evidence | Exit evidence | Gate |
| --- | --- | --- | --- |
| Reference lock | TeslaMate `4.1.0-dev` at `7054517c10475f39f480edeae8f90c6f717985a3` | Clean source identity and pinned differential fixture record | 001, 071 |
| Backend behavior | Pinned TeslaMate modules and tests | Differential fixture results for state, drive, position, charge, update, metadata, anomalies, geocoding, geofences, terrain, energy, and costs | 030-042, 071 |
| Durable correctness | Hub journal, identity, schema, indexes, durability, retention, repair, backup, and startup designs | Injected crash, replay, corrupt-store, backup/restore, and reconciliation reports; no lost durable acknowledgement or duplicate projected fact | 011-024, 070 |
| Token safety and no wake | Tesla API compatibility surface and read-only policy | Negative request audit, credential exposure scan, TLS validation, sleep/offline trace, and security review | 025-029, 073 |
| TeslaMate migration | TeslaMate schema/version and representative source corpus | Read-only preflight, consistent capture, typed bounded parallel binary COPY, transform report, source counts/checksums, destination reconciliation, and sealed audit report | 053-060, 066, 072 |
| Migration speed | Representative roughly 10-million-row database on baseline host | Timed end-to-end run under 10 minutes; 30-minute hard-ceiling run; host and profile recorded | 067-069, 072 |
| Phone data contract | `teslatlas-sync` v1 and projection schema 2.0 | Manifest/signature, resumable-download, atomic-swap, pairing, background-refresh, and provenance-switch tests on a native client | 046-052 |
| Freshness and recovery | Locked driving 5s, charging 10s, online 75s, readiness within 60s | Instrumented state traces and restart runs; asleep/offline last-observation age only; no wake traffic | 005, 029-037, 045, 070 |
| Live owner proof | Explicit owner-authorized vehicle and physical wake action | One-minute live-data probe, paired device receipt, redacted logs, and operator acknowledgement | 062 |
| Native platforms | Apple-silicon macOS; Debian amd64 and arm64 | Fresh install, upgrade/downgrade, service supervision, credential handling, and uninstall-preservation proof for each platform | 074-077 |
| Cutover safety | Source remains read-only; operator owns service mutation | Side-by-side, verification-window, cutover, rollback, and runbook rehearsal reports | 061, 063-065, 078-079 |
| Final release decision | All rows above | Signed final signoff with complete artifact index and every deviation accepted explicitly | 080 |

## Evidence record rule

Every completed row records the pinned source revision, Hub revision, command,
fixture or corpus identity, host and runtime profile, UTC time, expected and
actual result, and redacted artifacts. A deviation is closed only with its
user-approved scope, impact, and replacement proof. TeslaMate source,
database, services, containers, and configuration stay read-only throughout.

## Current truth

Existing unit and local contract tests are implementation evidence only.
No row requiring the representative corpus, all native platforms, an
owner-authorized live probe, or an operator cutover rehearsal is complete yet.
