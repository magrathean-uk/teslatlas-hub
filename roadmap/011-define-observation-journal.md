---
type: wayfinder:task
status: closed
parent: 000-map
---
# Define the observation journal

Blocked by: [Choose persistence and schema compatibility](010-choose-persistence-compatibility.md).

## Question

What immutable source facts must be durably recorded before lifecycle
projection, and how can projections be rebuilt without contacting Tesla?

## Starting recommendation

Journal normalized source observations before acknowledging collection, then
derive replaceable projections deterministically.

## Resolution

The immutable journal is one bounded normalized vehicle response per source
observation. It records source and vehicle identity, observed and receipt
times, canonical-payload hash, record type, reported vehicle state, and full
normalized payload. The unique identity is source, vehicle, observation time,
and payload hash; retry returns the original fact. Projection processes the
time-and-ID order from a durable cursor, so rebuild never contacts Tesla.
Malformed or legacy missing state is `unknown`, not inferred from a later
collection. Journal rows are append-only; derived lifecycle history, manifests,
and packs remain replaceable.
