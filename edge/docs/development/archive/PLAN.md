# Edge — source-published post-cleanup state

Revision 2026-09-22. The accepted current-Mac implementation is published on `main`.
The owner then requested removal of all local builds, artifacts, runtimes and VMs.

## Published result

- Accepted implementation lineage: `94bb86996254ed67ba90208573d5c6e0ad20c0bd`
- Published `main` before this cleanup metadata update: `d360044d18e5d540a54e156a1fdfb16e30aa1ce6`
- The published source retains fail-closed transport, pinned receiver, durable ACK, quota, rotation, spool and recovery behavior.

## Evidence boundary

Historical: the accepted Edge profile and ordinary Hub lifecycle route passed for the former runtime. The corresponding external candidates, receipts and runtime fixtures
were deliberately deleted. Those results remain historical provenance and do not
claim that a runnable local installation exists now.

## Current state

Source and Git history are retained. Regenerable builds and dependencies are removed.
No Edge or Hub listener is running; public ingress and production collection remain separately gated.
