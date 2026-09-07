# Product versioning

The Edge Cargo package carries the shared ecosystem product version
`2026.36.2`. The calendar number identifies the release cohort and does not
change Edge delivery envelopes, configuration format `1`, spool format `2`, or
the pinned upstream Fleet Telemetry bridge.

`compatibility/hub.json` binds the candidate to the independently authored
`edge-delivery-v2@2.0.0` contract profile and its exact manifest hash. It has no
tested Hub versions, source fingerprints, or runtime receipts. The profile and
local Rust tests freeze the contract; they do not prove Hub application or
Edge-to-Hub runtime compatibility.
