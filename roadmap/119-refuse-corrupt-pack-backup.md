---
type: wayfinder:task
status: closed
parent: 000-map
---
# Refuse corrupt pack backup

Blocked by: [Verify backup pack digests](118-verify-backup-pack-digests.md).

## Question

Does a referenced but altered local pack fail backup without leaving a partial
restore root?

## Starting recommendation

Corrupt a Hub-owned test pack after catalogue publication, require the backup
to fail, and check the exact new backup root is removed.

## Resolution

The corruption proof alters a Hub-owned referenced pack after catalogue
publication. Backup rejects its digest mismatch and removes the exact newly
created backup root. No partial restore candidate remains.
