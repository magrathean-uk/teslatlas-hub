---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design upgrade, downgrade, and rollback

Blocked by: [Design native packaging and supervision](074-design-native-packaging.md).

## Question

How are binary, configuration, schema, credential, protocol, and pack-format
changes staged, health-gated, rolled back, or refused?

## Starting recommendation

Take verified backups, stage compatibility checks before restart, and refuse
unsafe downgrades rather than attempting lossy reverse migrations.

## Resolution

Hub-only upgrades stage and gate binary, configuration, schema, protocol, pack,
and credential compatibility against a verified backup. Migrations are
forward-only; unsafe downgrade refuses, while incompatible rollback restores a
matching fresh private backup generation. TeslaMate is never changed.
