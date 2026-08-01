---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design native packaging and supervision

Blocked by: [Design the security and privacy model](073-design-security-privacy-model.md).

## Question

What package, system user, directories, permissions, systemd dependencies,
credentials, limits, sandboxing, timers, and removal behavior are required?

## Starting recommendation

Ship native Debian packages with hardened systemd units, protected persistent
data, explicit setup, safe upgrades, and data-preserving removal.

## Resolution

Debian packages contain only Hub-owned binary, configuration, hardened manual
systemd units, and tools. They use protected service identity/credentials, do
not auto-start collection or touch TeslaMate, and preserve data on removal.
Apple-silicon macOS needs the same native ownership guarantees without systemd.
