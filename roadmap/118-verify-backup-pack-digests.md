---
type: wayfinder:task
status: closed
parent: 000-map
---
# Verify backup pack digests

Blocked by: [Rehearse native migration restore](117-rehearse-native-migration-restore.md).

## Question

Can a corrupt or substituted immutable pack enter a completed backup root?

## Starting recommendation

Stream-hash each referenced source and copied pack against the catalogue digest,
then reject and clean the new backup root on mismatch.

## Resolution

Every copied immutable backup pack is now streamed and SHA-256 checked against
the copied catalogue digest after its canonical name and declared byte count
are checked. A mismatch fails the backup and removes only its newly created
root.

Focused backup-pack proof passed.
