---
type: wayfinder:task
status: closed
parent: 000-map
---
# Set account and vehicle scope

Blocked by: [Set the host and resource envelope](006-set-host-resource-envelope.md).

## Question

Must one Hub support multiple Tesla accounts, multiple vehicles per account,
disabled vehicles, and independent collection ownership?

## Starting recommendation

Match TeslaMate multi-vehicle behavior and model account ownership explicitly,
without letting one failing vehicle stall another.

## Resolution

Hub supports many vehicles for each registered Source and preserves them as
independent histories. The pinned TeslaMate model has one configured Tesla
account with many per-car settings; Hub therefore supports one active live
owner credential per collector instance in this stage, while allowing many
independent imported Sources in the destination database. Multi-account live
collection is not silently implied and needs an explicit credential-mode
extension in ticket 025.

A Hub vehicle identity is the stable `(source_id, source_vehicle_key)` pair.
VIN and display name are mutable metadata, never a cross-source merge key.
Each vehicle has independent observation identity, lifecycle recovery state,
manifest sequence, publication, freshness, and failure reporting. A failure to
fetch one vehicle must retain its last good history and not block durable work
for another vehicle in the same discovery result.

Discovery retains disabled vehicles and their history, but disables new
collection and freshness promises for that vehicle only. Re-enabling resumes
from durable state; it does not create a new identity, delete history, or
replay unrelated vehicles. The Hub publishes each eligible vehicle separately;
Teslatlas selects one published vehicle per paired source profile.

Source migration stays selected-car scoped for this phase. Repeating it for a
second selected source car creates a separate Hub vehicle under the same
source, and never interleaves histories. Future source switching requires the
provenance rules in ticket 052 rather than VIN-based guessing.
