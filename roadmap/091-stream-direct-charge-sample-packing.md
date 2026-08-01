---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream direct charge sample packing

Blocked by: [Stream direct charge summary pass](090-stream-direct-charge-summary-pass.md).

## Question

Can the sample pack pass consume the same binary source stream while preserving
parent checks, bounded fragments, and zero duplicate projected facts?

## Starting recommendation

Replace only the second charge traversal. Keep existing parent lookup,
fragment construction, and hard per-pass row limit unchanged.

## Resolution

Direct charge sample packing now reads binary COPY rows. Parent checks,
fragment bounds, and the independent hard sample-pass limit are unchanged, so
missing processes or oversized source histories fail closed.
