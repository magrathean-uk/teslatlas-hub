# Home Assistant — source-published post-cleanup state

Revision 2026-09-22. The accepted current-Mac implementation is published on `main`.
The owner then requested removal of all local builds, artifacts, runtimes and VMs.

## Published result

- Accepted implementation lineage: `78186795dfe2575258d4e3c2b138f263f8df21b5`
- Published `main` before this cleanup metadata update: `656c1db2891b4bf01929736f89d718af4d27208e`
- The published integration includes the accepted strict-TLS route and current dynamic vehicle continuity coverage.

## Evidence boundary

Current source validation before publication: 134 passed, 2 skipped; formatting and matrix-contract checks passed. Historical guest evidence recorded one vehicle and 13 available entities. The corresponding external candidates, receipts and runtime fixtures
were deliberately deleted. Those results remain historical provenance and do not
claim that a runnable local installation exists now.

## Current state

Source and Git history are retained. Regenerable builds and dependencies are removed.
The Lima guest and Home Assistant container were removed; HACS/store distribution and real credentials remain deferred.
