# TypeScript SDK — source-published post-cleanup state

Revision 2026-09-22. The accepted current-Mac implementation is published on `main`.
The owner then requested removal of all local builds, artifacts, runtimes and VMs.

## Published result

- Accepted implementation lineage: `3644ad8c0a7bf10c01e524c050026ead898d47e5`
- Published `main` before this cleanup metadata update: `71340531eeacfca475ca5216cd76a9b5986050a5`
- The published SDK source contains the accepted signed schema 2.2 history and pagination behavior.

## Evidence boundary

Historical: the exact packed candidate consumed non-empty history across restart and matched Swift semantic output. The corresponding external candidates, receipts and runtime fixtures
were deliberately deleted. Those results remain historical provenance and do not
claim that a runnable local installation exists now.

## Current state

Source and Git history are retained. Regenerable builds and dependencies are removed.
No packed candidate or dependency tree remains; registry publication remains deferred.
