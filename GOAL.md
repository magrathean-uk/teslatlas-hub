# Teslatlas Hub goal

Do not start this goal until the user explicitly submits it.

## Mandatory question policy

Roadmap questions are AFK investigations, not questions for the user. Resolve
them from the pinned TeslaMate source and tests, Hub and Teslatlas source,
official contracts, measurements, or a conservative documented inference.
TeslaMate behavior is the default. Never ask the user to invent freshness,
timeout, retry, recovery, throughput, memory, or disk numbers that can be
derived or measured.

Ask only when evidence cannot answer a genuine product preference, when a
deliberate deviation from TeslaMate is proposed, or when credentials, physical
action, production authority, or destructive/external mutation are actually
required. Before asking, give the evidence, recommended answer, and consequence.
Ask one question only.

Locked targets are 5-second driving freshness, 10-second charging freshness,
75-second ordinary-online freshness, no asleep/offline freshness promise with
last-observation age exposed, no wake, and crash-to-truthful-readiness within
60 seconds on a supported baseline host.

```text
/goal Complete Teslatlas Hub as a production Rust backend for Teslatlas, matching the pinned TeslaMate 4.1 development backend while excluding Grafana, Phoenix, assets, and web UI. Follow roadmap/000-map.md and roadmap/EXECUTION_POLICY.md in strict order. Support Apple-silicon macOS and Debian on amd64 and arm64. Treat TeslaMate, its PostgreSQL database, services, Docker deployment, containers, configuration, schedules, and credentials as an absolute read-only source: never write, stop, start, restart, pause, enable, disable, reload, reconfigure, schedule, rotate, revoke, or otherwise mutate them. The migration may use read-only inspection and one consistent PostgreSQL snapshot, copy history and legacy or Fleet credentials only into Hub, start only Hub-owned services, validate source and destination comprehensively, ask the user to wake the car, and prove after one minute that Hub durably collected new data. It must never automate TeslaMate cutover; it produces a readiness report and exact manual operator instructions. Build migration for speed using parallel typed PostgreSQL binary COPY streams, bounded direct Rust decoding, bulk unpublished staging, set-based validation, and final reconciliation. Target under 10 minutes for the existing roughly 10-million-row representative database on a supported baseline host; 30 minutes is the hard ceiling and any design approaching one hour must be replaced. Work one agent-sized roadmap step at a time: one narrow end-to-end behavior, normally no more than three production files and one focused test file, at most one schema change, one focused validation, independently reversible, and at most 25,000 working tokens. Split before reading broadly or editing if larger. All roadmap questions are AFK investigations by default. Resolve them from pinned TeslaMate and project evidence, official contracts, measurements, or conservative documented inference; do not ask the user for derivable target numbers. Ask only for a deliberate product deviation, unavailable access or credentials, physical action, production authority, or destructive/external mutation, and only when that boundary is actually reached. Never use GitHub CI, build, or test automation. Completion requires differential TeslaMate parity, zero unexplained database differences, crash and corruption recovery proof, read-only migration and restore rehearsal, one-minute live collection proof, adaptive-profile evidence, and native proof on Apple-silicon macOS plus Debian amd64 and arm64.
```
