# Third-party notices

## Tesla Fleet Telemetry receiver

The optional `teslatlas-fleet-telemetry` sidecar is built from Tesla Fleet
Telemetry v0.9.4, revision
`d64c73ab65e7c5fb5fc12b35fe507e2c6054227b`, under the Apache License 2.0.

Source: <https://github.com/teslamotors/fleet-telemetry>

Bundled upstream licence: [Apache License 2.0](Apache-2.0-fleet-telemetry.txt)

Teslatlas changes are recorded in
`packaging/fleet-telemetry-bridge/0001-teslatlas-http-dispatcher.patch`.
They add a fixed loopback HTTP datastore and reliable acknowledgement tests;
they do not add Tesla account authentication or vehicle commands.

Distributors must ship the upstream Apache 2.0 license, preserve upstream and
modified-file notices, and provide the corresponding pinned source plus patch.
The build lock records the exact archive and patch digests.
