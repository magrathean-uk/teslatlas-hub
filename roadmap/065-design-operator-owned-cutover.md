---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design operator-owned cutover and rollback

Blocked by: [Design the read-only verification window](064-design-read-only-verification-window.md).

## Question

What exact manual commands and evidence let the operator cut over or roll back
without giving the migration tool authority over TeslaMate?

## Starting recommendation

Keep TeslaMate and its source database untouched. Print but never execute exact
operator commands, prerequisites, consequences, and reversal steps. Export any
Hub-only interval before returning collection ownership.

## Resolution

Hub prints a signed, single-use, machine-specific operator plan but never
executes TeslaMate actions. Operator confirmation gates Hub-only activation.
Rollback first stops Hub, seals every Hub-only interval, then returns source
ownership through separately printed operator commands; no history merges.
