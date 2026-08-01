---
type: wayfinder:task
status: closed
parent: 000-map
---
# Choose persistence and schema compatibility

Blocked by: [Choose the process topology](009-choose-process-topology.md).

## Question

Should Hub keep its Rust-owned SQLite database, adopt PostgreSQL, or expose a
compatibility projection while retaining a separate internal store?

## Starting recommendation

Keep SQLite only if durability, concurrency, repair, and large-history evidence
meet the reliability objectives; use a separate compatibility projection when
exact TeslaMate SQL consumers are in scope.

## Resolution

Keep Rust-owned SQLite as Hub's only canonical writable store. PostgreSQL is a
read-only TeslaMate migration source, not a Hub runtime dependency. No
TeslaMate SQL compatibility projection is needed because direct SQL consumers,
Phoenix, Grafana, MQTT, and web UI are out of scope; the typed Teslatlas
contract is the compatibility boundary. WAL/full-sync integrity behavior and
later durability, repair, concurrent-access, and representative-corpus gates
remain mandatory evidence; a failed gate reopens this decision.
