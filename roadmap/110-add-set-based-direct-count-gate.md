---
type: wayfinder:task
status: closed
parent: 000-map
---
# Add set-based direct count gate

Blocked by: [Prove native ten-million direct migration](109-prove-native-ten-million-direct-migration.md).

## Question

Can a direct migration reject unexplained selected-car fact loss before any
candidate pack reaches publication?

## Starting recommendation

Read fixed selected-car aggregate counts on the existing snapshot and require
each source fact either to project or to be counted by its named skip reason.

## Resolution

Direct capture now reads one fixed, selected-car set-based aggregate on the
same snapshot before projection. Before returning candidate packs it requires
exact accounting for cars, drives, positions, charging processes, and charge
samples; only the named open-drive and unattached-position loss reasons are
permitted. Any other mismatch is fatal before the caller can publish.

Focused accounting tests and the native complete-corpus import passed.
