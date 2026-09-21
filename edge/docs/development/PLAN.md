# Edge — accepted retained current-state route

Revision 2026-09-21. **COMPLETED** for MF-1/MF-3.

## Accepted result

Exact source `94bb86996254ed67ba90208573d5c6e0ad20c0bd` remains published on
`main`. The retained fail-closed transport route is controlled by the ordinary
Hub UI and runs under the exact accepted Hub candidate on Mac loopback port 21444.
Its existing pinned-receiver, encrypted-spool, durable-ACK, quota, rotation and
recovery evidence remains bounded to that profile.

## Boundary

No public ingress, production collection or vehicle action is accepted. Broader
platform and release work remains deferred.
