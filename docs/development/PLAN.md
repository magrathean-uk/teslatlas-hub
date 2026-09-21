# Hub — accepted current-Mac source-run product

Revision 2026-09-21. **COMPLETED** for MF-0 through MF-3 and GR-F1 through GR-F6.

## Accepted result

Exact source `c209eed0d6c860fbbe3ceaa617c26fd40e0b4ad6` is published on
`codex/hub-macos-sidebar-redesign`. The exact archived candidate passed the full
native test target, independent review (`ACCEPT_NO_P1_P2_P3`), ordinary UI
Start/Restart/Stop, and the occupied-port cleanup case. The retained LaunchAgent
runs that candidate on Mac loopback port 21444.

The coordinated immutable receipt is
`/Users/bolyki/dev/teslatlas-lab/runtime-fixtures/mf-final-20260921-r1/evidence/coordinated-runtime-receipt.json`.

## Boundary

This accepts the source-run working product on the current Apple-silicon Mac.
Packaging, distribution, notarization, release binaries, real Tesla access,
vehicle actions, public ingress, Intel, Azure and other platforms remain deferred
or separately gated. `app/` and Viewer remain out of scope.
