---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design migration transformation

Blocked by: [Design TeslaMate version compatibility](056-design-source-version-compatibility.md).

## Question

How is every source table, field, identifier, relationship, enum, unit,
timestamp, setting, and nullable value mapped into Hub?

## Starting recommendation

Maintain a versioned field-level mapping with explicit loss accounting and no
silent coercion. Decode PostgreSQL binary values directly into typed Rust rows,
use large bounded destination transactions and bulk prepared inserts, and build
expensive destination indexes only at the fastest integrity-safe point.

## Resolution

The versioned field map names every selected table and field, source evidence,
destination, units, null/enum rules, and intentional loss. Bounded typed bulk
staging validates relationships before sealing; no conversion is silent.
