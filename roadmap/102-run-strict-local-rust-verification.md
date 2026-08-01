---
type: wayfinder:task
status: closed
parent: 000-map
---
# Run strict local Rust verification

Blocked by: [Run metadata COPY lanes in parallel](101-run-metadata-copy-lanes-in-parallel.md).

## Question

Does the current native source-copy implementation pass strict local Rust
linting without relying on GitHub automation?

## Starting recommendation

Run all-target Clippy with warnings denied, then format and test locally.

## Resolution

Strict local verification passed: all-target Clippy with warnings denied,
format validation, all-target tests, and diff integrity checks completed without
remote automation.
