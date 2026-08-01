---
type: wayfinder:task
status: closed
parent: 000-map
---
# Clean prepublication candidate failures

Blocked by: [Clean unpublished pack failures](112-clean-unpublished-pack-failures.md).

## Question

After a complete candidate returns from capture, do later local validation or
manifest-signing failures leave unpublished pack objects behind?

## Starting recommendation

Use one reusable Hub-owned file cleanup path before every prepublication
failure exit, while retaining published packs on the successful path.

## Resolution

Completed direct and staged candidates now own their packs until the Hub
catalogue publish succeeds. Any failure while checking the fingerprint,
building/signing the manifest, or publishing it drops and removes every
candidate pack. The successful path explicitly transfers ownership to the
catalogue before later bookkeeping; the unchanged-snapshot path preserves only
already-published content-addressed objects.

Both cleanup paths and the full local Rust suite passed.
