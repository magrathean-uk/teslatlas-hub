---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the platform and install matrix

Blocked by: [Design upgrade, downgrade, and rollback](075-design-upgrade-rollback.md).

## Question

Which clean install, reboot, upgrade, restore, filesystem, architecture, and
resource-class combinations must pass before release?

## Starting recommendation

Require native amd64 and arm64 Debian proof, small and large hosts, clean and
upgrade paths, and one Raspberry Pi-class reliability run.

## Resolution

Release needs separate native Apple-silicon macOS, Debian amd64, and Debian
arm64 proof across installation, supervision, storage, credentials, reboot,
upgrade/rollback, restore, corpus, and fault paths. A Pi-class arm64 run proves
bounded recovery; the ten-minute claim stays limited to qualifying baseline
hosts.
