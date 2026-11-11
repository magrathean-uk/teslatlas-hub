# Swift SDK — source-published post-cleanup state

Revision 2026-09-22. The accepted current-Mac implementation is published on `main`.
The owner then requested removal of all local builds, artifacts, runtimes and VMs.

## Published result

- Accepted implementation lineage: `df1410cba8770d7bdcd8838aede1dcebd177b56c`
- Published `main` before this cleanup metadata update: `7c34e8dfffecc5e1a4a3df677dee7e4693f9b662`
- The published SDK source contains the accepted claim replay, restart and signed schema 2.2 consumer behavior.

## Evidence boundary

Historical: clean-archive tests and non-empty-history semantic equality with TypeScript passed. The corresponding external candidates, receipts and runtime fixtures
were deliberately deleted. Those results remain historical provenance and do not
claim that a runnable local installation exists now.

## Current state

Source and Git history are retained. Regenerable builds and dependencies are removed.
No build archive remains; distribution and broader platform work remain deferred.
