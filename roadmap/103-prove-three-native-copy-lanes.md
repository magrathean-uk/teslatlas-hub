---
type: wayfinder:task
status: closed
parent: 000-map
---
# Prove three native COPY lanes

Blocked by: [Run strict local Rust verification](102-run-strict-local-rust-verification.md).

## Question

Can three concurrent attached read-only lanes each return the same typed binary
result from one exported PostgreSQL snapshot?

## Starting recommendation

Attach three native lanes concurrently, run the reviewed binary car stream in
each, and require identical typed output before all lanes roll back.

## Resolution

Native proof passed: three concurrent read-only attached lanes each returned
the same typed binary selected-car fact from one exported snapshot.
