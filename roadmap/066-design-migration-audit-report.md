---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the migration audit report

Blocked by: [Design operator-owned cutover and rollback](065-design-operator-owned-cutover.md).

## Question

What evidence must the migration preserve for discovery, copy, validation,
credential handoff, live proof, verification window, cutover, and rollback?

## Starting recommendation

Emit a redacted machine-readable report with command version, checksums, counts,
gate outcomes, timings, throughput, source read-only proof, timestamps, and
operator decisions, but no secrets.

## Resolution

Use one immutable canonical, checksummed, optionally signed report per attempt.
It binds all discovery, capture, validation, credential, live, window, and
operator evidence to explicit gate outcomes while redacting every secret and
source-sensitive value.
