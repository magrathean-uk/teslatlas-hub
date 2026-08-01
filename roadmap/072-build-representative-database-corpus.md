---
type: wayfinder:task
status: closed
parent: 000-map
---
# Build the representative database corpus

Blocked by: [Design differential conformance](071-design-differential-conformance.md).

## Question

Which anonymized TeslaMate databases cover old schemas, multiple cars, huge
positions, incomplete sessions, corruption, unusual charging, and custom settings?

## Starting recommendation

Create provenance-recorded synthetic and redacted fixtures with expected
validation findings; never depend on a production database for repeatable proof.

## Resolution

The versioned corpus manifest defines deterministic synthetic and reviewed
redacted fixtures for schema, scope, huge-position, incomplete, corrupt,
charging, and settings cases. It binds each fixture to migration identity,
expected validation, normalized outcome, and reproducible provenance; the
approximately ten-million-row case generates locally for benchmark proof.
