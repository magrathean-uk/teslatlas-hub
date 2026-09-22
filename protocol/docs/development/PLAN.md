# Protocol — source-published post-cleanup state

Revision 2026-09-22. The accepted current-Mac implementation is published on `main`.
The owner then requested removal of all local builds, artifacts, runtimes and VMs.

## Published result

- Accepted implementation lineage: `788cfa2fec520896e9dc4accfc9202325327a135`
- Published `main` before this cleanup metadata update: `c3ee94f2e12e22d0be7402b1ad185058aa9f0752`
- The published contracts retain the distinct reference, current-Hub, compatibility and Edge profiles, including schema 2.2 signatures and opaque cursors.

## Evidence boundary

Historical: Node, Swift and strict-browser consumers passed the recorded schema 2.2 and cursor checks. The corresponding external candidates, receipts and runtime fixtures
were deliberately deleted. Those results remain historical provenance and do not
claim that a runnable local installation exists now.

## Current state

Source and Git history are retained. Regenerable builds and dependencies are removed.
No local receipt or runtime remains; new integration claims require a fresh coordinated run.
