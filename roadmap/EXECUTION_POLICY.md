# Agent execution policy

This policy governs future work. It does not authorize development.

## Platform boundary

- Apple-silicon macOS is supported.
- Debian is supported on amd64 and arm64.
- Intel macOS is not a first-platform target.
- Platform-neutral Rust behavior is proved on macOS first; Debian-specific
  packaging and service behavior is proved separately.

## TeslaMate boundary

TeslaMate is read-only. An agent may inspect service and container metadata,
read files, open a read-only PostgreSQL snapshot, and copy credentials into Hub.
It may not:

- write, migrate, repair, vacuum, analyze, lock for writes, or configure the
  TeslaMate database;
- stop, start, restart, pause, enable, disable, or reload TeslaMate;
- run `docker exec`, create helper containers, change Compose files, or mutate
  Docker state;
- install a timer, schedule, hook, or delayed command that changes TeslaMate;
- rotate, revoke, delete, or rewrite TeslaMate credentials.

Only Hub-owned files, databases, processes, services, and credentials may be
created or changed.

## Migration speed

- No text SQL dump or JSON staging copy.
- Prefer an exported read-only PostgreSQL snapshot with parallel typed binary
  `COPY` streams.
- Decode directly into typed Rust rows and bulk-load an unpublished Hub stage.
- Validate with set-based source queries and final destination summaries.
- Initial target: under 10 minutes for the representative roughly
  10-million-row database on a supported baseline host.
- Hard ceiling: 30 minutes. A design approaching one hour must be replaced.

## Agent-sized steps

- Work one numbered roadmap file at a time.
- One implementation step delivers one narrow end-to-end behavior.
- Normally touch no more than three production files and one focused test file.
- Include at most one schema migration.
- Use one focused validation command or evidence capture.
- Keep the step independently reversible and committable.
- Target at most 25,000 total working tokens.
- Split before implementation when any limit is likely to be exceeded.
- Never combine collector logic, database migration, sync protocol, packaging,
  and platform proof in one step.
- Stop at HITL, credential, physical-device, production, or destructive
  boundaries.

## Question routing

- Roadmap `## Question` sections are agent investigations, not prompts to the
  user.
- Every ticket is AFK unless its frontmatter explicitly contains `mode: HITL`.
- Inspect the pinned TeslaMate source and tests first.
- Then inspect Hub and Teslatlas source, official contracts, and measured
  benchmark evidence.
- Match TeslaMate behavior by default. A deliberate deviation requires user
  approval; ordinary parity does not.
- If TeslaMate has no stated SLA, infer a conservative test threshold from its
  actual polling cadence, timers, retries, supervision, and tests. Record the
  evidence and continue.
- Do not ask the user to invent freshness, timeout, retry, recovery, throughput,
  memory, or disk numbers that can be derived or measured.
- Ask only for product preference that evidence cannot answer, unavailable
  access or credentials, a physical action such as waking the car, production
  authority, or destructive/external mutation.
- Before any permitted question, state the evidence, recommended answer, and
  consequence. Ask one question only.

## Locked service targets

- Driving freshness: at most 5 seconds.
- Charging freshness: at most 10 seconds.
- Ordinary online freshness: at most 75 seconds.
- Asleep or offline: no freshness promise; expose last-observation age and
  never wake.
- Crash to truthful readiness: at most 60 seconds on a supported baseline host.
- Durably acknowledged observations: zero loss.
- Retries and replay: zero duplicate projected facts.

## Phase order

1. Reference and parity boundary.
2. Reliability and supported-host contract.
3. Storage, integrity, recovery, and backup.
4. Tesla collection state-machine parity.
5. TeslaMate settings and compatibility outputs.
6. Teslatlas full and delta synchronization.
7. Read-only fast migration and credential handoff.
8. One-minute live probe and read-only verification window.
9. Adaptive performance and pressure handling.
10. Packaging, platform matrix, rehearsal, and final parity evidence.
