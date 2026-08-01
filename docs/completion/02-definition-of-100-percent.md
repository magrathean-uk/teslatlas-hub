# Definition of 100 percent

## Release meaning

For the current target, 100 percent means a production-grade Rust backend that
performs the supported TeslaMate backend behavior for Teslatlas on Apple-silicon
macOS and Debian arm64. It preserves history, collects new observations without
waking vehicles, safely migrates every selected source car including open
sessions, and supplies verified data to Teslatlas.

Debian amd64 is explicitly deferred until the user provides an x86 host. It is
not silently counted as complete. Intel macOS is unsupported.

## Required outcomes

| Outcome | Completion proof |
| --- | --- |
| Reference parity | Differential results against the pinned TeslaMate commit cover state, position, drive, charge, update, metadata, geofence, terrain, energy, cost, and anomaly behavior. Every difference is explained and accepted or removed. |
| Safe collection | 5-second driving, 10-second charging, 75-second normal-online targets; no asleep/offline freshness promise; no Hub wake traffic; truthful readiness within 60 seconds after crash. |
| Durable history | No lost fact after durable acknowledgement, no duplicate projection, repairable catalogues, verified packs, backup/restore, and corruption/crash/replay proof. |
| All-car migration | Read-only discovery selects every intended car, imports each complete history and open session, records per-car result, and makes no partial multi-car publication appear successful. |
| Source consistency | One repeatable-read source snapshot, attached bounded typed binary COPY lanes, fixed SQL, no source writes, bounded disk/RAM, and source-to-destination reconciliation. |
| Migration speed | Representative roughly 10-million-row source completes below 10 minutes on an eligible direct low-latency baseline. Thirty minutes is hard failure. |
| Credentials | Legacy or Fleet handoff protects secrets, redacts logs, and never changes source custody. |
| Teslatlas sync | Full snapshot works; delta v2 has protocol, writer, client apply, resume, atomicity, tombstones, cursor recovery, and fallback proof. |
| Native delivery | Fresh install, supervision, credentials, TLS/pairing, upgrade/refusal, restore, interruption recovery, and preserving removal are current on macOS arm64 and Debian arm64. |
| Live proof | Owner manually wakes car, Hub stores a new observation after one minute, paired client receives it, and request audit confirms no Hub wake action. |
| Rehearsal | Disposable full migration, verification window, operator-owned cutover, rollback, and fault sequence pass without TeslaMate mutation. |

## Non-negotiable rules

- TeslaMate remains read-only during discovery, migration, validation, live
  proof, rehearsal, and Hub failure handling.
- A package, unit test, mock, or old run cannot replace native current evidence.
- No automatic timer may stop, edit, or otherwise control TeslaMate.
- A signed final record is required before calling Hub complete. See
  [final signoff](../FINAL_SIGNOFF.md).

## Optional scope decision

MQTT publication and a Hub self-updater need one explicit decision: either
implement and prove a TeslaMate-visible compatible backend surface, or list
them as approved out-of-scope boundaries. They cannot remain accidental gaps.
