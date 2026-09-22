# Hub — accepted current-Mac redesign

Revision 2026-09-22. **COMPLETE for the authorized current-Mac source-run scope.**

## Accepted product

- UI source: `478a9139bfe11385bf2c9a3faad947f4419510f6`
- UI tree: `306511db848c28f8f3c9e06db6b840826e0d241c`
- Branch: `codex/hub-macos-sidebar-redesign`
- Source-bound app:
  `/Users/bolyki/dev/teslatlas-lab/candidates/hub-redesign-478a913-r1/artifacts/Teslatlas Hub.app`
- App executable SHA-256:
  `c5d80683e802f86ff09a8f81f8372f5c304513440371e9db2018fe8c28a47984`
- Retained backend source: `5a201fbb57c96635398578d2e83d62e1e38b8c93`
- Retained backend SHA-256:
  `203eef628ad14f00721f0dac3f32c54cc56c9ed242de7458f31daa4da3b794df`

The onboarding shell now keeps the normal unified title-bar geometry, hides the
ready-state destinations and service status, omits generic step/progress chrome,
and reveals Overview selected and focused after successful completion. Actual
TeslaMate import progress remains visible where it describes real work.

## Acceptance evidence

- Independent source review: `PASS_NO_P1_P2_P3`.
- Complete native target: PASS after the final source changes.
- Source-bound visual target: PASS with 34 native captures, including Welcome,
  Choose, Overview and the supported minimum size.
- Exact candidate preview: Welcome and Choose expose no ready-state tabs or wizard
  progress; completion reveals Overview.
- Exact launcher/runtime: all six scoped development values verified, native Restart
  replaced the retained service PID, strict scoped-CA health returned HTTP 200/ok,
  and comparison Hub 21443 remained untouched.
- The backend bytes and non-onboarding behavior are unchanged from the immediately
  preceding independently reviewed candidate, so its Protocol, SDK, signed-sync,
  receiver/Edge, failure-cleanup and ordinary native-route evidence remains valid
  within those exact limits.

Primary evidence:

- `/Users/bolyki/dev/teslatlas-lab/candidates/hub-redesign-478a913-r1/build-receipt.json`
- `/Users/bolyki/dev/teslatlas-lab/reviews/hub-redesign-20260922/final-478a913-r1/evidence/exact-candidate-live-preview.json`
- `/Users/bolyki/dev/teslatlas-lab/reviews/hub-redesign-20260922/final-478a913-r1/evidence/exact-runtime-restart.json`
- Workspace report:
  `docs/development/assessments/hub-redesign-independent-review-2026-09-22.md`

## Recorded limit

The existing Lima guest currently resets SSH connections, so this final UI-only
successor has no fresh Home Assistant guest poll or restart-continuity receipt.
Earlier same-entry strict-TLS evidence recorded 13 entities and zero unavailable;
that evidence remains historical and is not relabelled as a fresh run. No source or
host-runtime defect was reproduced from the guest access failure.

## Boundary

No Hub implementation work remains in the authorized Mac-first scope. Packaging,
distribution, notarization, release binaries, real Tesla access, vehicle actions,
public ingress, Intel, Azure, other platforms, Viewer and the separate `app/` remain
deferred or separately gated.
