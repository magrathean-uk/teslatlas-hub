# Known risks

| Risk | Present state | Control and closure gate |
| --- | --- | --- |
| False parity claim | Core delta and import paths are locally proven, but live and platform proof is incomplete. | Keep status labels; final signoff requires every matrix row. |
| TeslaMate mutation | Migration/cutover tooling could accidentally control source runtime. | Read-only credential/session audits and disposable rehearsal; no automatic cutover action. |
| Source snapshot drift | Separate readers can see different source moments. | Exported repeatable-read lease and attached-reader native proof. |
| Partial all-car success | Implementation reports per-car success, skip, and failure; final rehearsal is open. | Sealed stages, truthful batch report, and disposable interruption rehearsal. |
| Open session race | Second-snapshot child reconciliation is implemented and fixture-covered. | Native source race rehearsal and final migration evidence. |
| Duplicate or lost facts | Delta/import/outbox transactions and replay paths are locally covered. | Crash, storage, corruption, network, and full replay matrix. |
| Secret exposure | Token handoff, logs, process lists, and reports remain sensitive surfaces. | Protected native storage, secret scans, and redacted artifact review. |
| Wake violation | A request or retry could wake a sleeping vehicle. | Request audit across sleep/offline/live traces; owner-only physical wake. |
| Missing live streaming proof | Real offline discovery exists; online, driving, charging, and stream behavior are not live-proven. | Owner wake, one-minute collection, and stream receipt. |
| Misleading speed result | WAN or warm-cache result can look like baseline proof. | Record path/cache/profile; qualify only direct low-latency baseline. |
| Storage exhaustion | Stage, pack, WAL, backup, and recovery may exceed host space. | Fresh Debian free-byte/inode reservation, database validation, and low-space fault cases. |
| Delta inconsistency | Local contiguous validation and atomic apply pass; hostile/live server conditions remain unverified. | Preserve signed lineage, digest, range, tombstone, replay, and fallback gates. |
| Package regression | `dev45` may differ from earlier verified dev11. | Fresh Debian reinstall and exact-artifact platform gates. |
| macOS evidence staleness | Earlier local proof may not match current code/artifact. | Fresh arm64 release-bound evidence. |
| Debian amd64 gap | No x86 host currently supplied. | Keep deferred, do not claim completion; run when host arrives. |
| Optional backend integrations | MQTT/updater behavior is not finally scoped. | Explicit product decision and documented proof or exclusion. |
| Human cutover error | Source change is manual by design. | Signed plan, disposable rehearsal, rollback runbook, operator acknowledgement. |

## Escalation

Stop release work immediately on source mutation, secret exposure, unexplained
source/destination difference, corrupt or unsigned artifact, lost durable fact,
duplicate projection, invalid delta lineage, failed restore, missed hard
performance limit, or unsupported platform/storage admission. Record failure;
do not hide it behind a retry.

See [final signoff](../FINAL_SIGNOFF.md) for the non-waivable release blockers.
