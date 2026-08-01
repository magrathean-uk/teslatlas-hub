---
type: wayfinder:task
status: closed
parent: 000-map
---
# Prove ten-million corpus load

Blocked by: [Generate large position corpus](107-generate-large-position-corpus.md).

## Question

Can the deterministic generator materialize the representative row count in
a disposable native PostgreSQL source without violating its declared shape?

## Starting recommendation

Restore the schema and complete fixture to a Hub-owned temporary PostgreSQL,
generate the full ten-million-row insert, and record only row-count and wall
time evidence.

## Resolution

The deterministic emitter restored into a disposable Hub-owned native
PostgreSQL source with 10,000,003 selected-car positions: the three complete
fixture positions plus 10,000,000 generated rows. Generation and PostgreSQL
load took 18 seconds on the local Apple-silicon host. This is corpus-load
evidence only, not an end-to-end migration performance claim.
