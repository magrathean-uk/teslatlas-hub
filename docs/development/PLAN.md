# Hub — source-published post-cleanup state

Revision 2026-09-22. The accepted current-Mac implementation is published on `main`.
The owner then requested removal of all local builds, artifacts, runtimes and VMs.

## Published result

- Accepted implementation lineage: `478a9139bfe11385bf2c9a3faad947f4419510f6`
- Published `main` before this cleanup metadata update: `b86bdf17998ea34cf231fbc12089494f8dbff478`
- The accepted redesign source includes the unified title placement, hides ready-state navigation and service chrome during onboarding, removes generic wizard progress, and routes successful completion to Overview.

## Evidence boundary

Historical: complete native target, 34 visual captures, exact onboarding preview and source-bound runtime review passed for the recorded candidate. The corresponding external candidates, receipts and runtime fixtures
were deliberately deleted. Those results remain historical provenance and do not
claim that a runnable local installation exists now.

## Current state

Source and Git history are retained. Regenerable builds and dependencies are removed.
A fresh build and complete affected acceptance are required before the Hub is run or called currently accepted.
