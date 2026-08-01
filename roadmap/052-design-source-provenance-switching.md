---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design source provenance and switching

Blocked by: [Design background refresh](051-design-background-refresh.md).

## Question

How does Teslatlas distinguish Hub, TeslaMate, and other histories, prevent
cross-source row collisions, and switch sources without silent replacement?

## Starting recommendation

Bind every generation to installation, account, vehicle, and source identities
and require an explicit atomic source switch.

## Resolution

Source and vehicle identifiers are namespaced and durable. A switch is an
owner-approved full replacement with verified candidate staging; it never
merges histories, changes provenance, or silently overwrites the old mirror.
