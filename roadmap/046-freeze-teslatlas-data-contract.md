---
type: wayfinder:task
status: closed
parent: 000-map
---
# Freeze the complete Teslatlas data contract

Blocked by: [Design operational observability](045-design-operational-observability.md).

## Question

What versioned backend contract gives Teslatlas every parity field, provenance
fact, integrity marker, and recovery signal it needs?

## Starting recommendation

Use a versioned typed contract independent of the internal database and require
backward-compatible reads across at least one release transition.

## Resolution

The paired signed projection contract is frozen independently of Hub storage.
Current typed tables, integrity binding, recovery behavior, parity extension
families, and strict major/minor evolution are defined in
`docs/DATA_CONTRACT.md`. Readers receive one complete compatible release
transition; unknown unsafe data fails closed without partial activation.
