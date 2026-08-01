---
type: wayfinder:task
status: closed
parent: 000-map
---
# Verify backup path locally

Blocked by: [Refuse corrupt pack backup](119-refuse-corrupt-pack-backup.md).

## Question

Do the new backup, restore, and corruption boundaries preserve the complete
local Rust contract?

## Starting recommendation

Run formatting, all-target tests, and warning-as-error clippy locally.

## Resolution

Formatting and all 131 local tests passed. Warning-as-error clippy also
passed after removing one redundant asynchronous wrapper in the native
snapshot-lane test.
