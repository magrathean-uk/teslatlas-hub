# Hub — redesign candidate awaiting live acceptance

Revision 2026-09-22. **NOT ACCEPTED** for the current native redesign.

## Current candidate

The UI source candidate is `5a839fe18675829206976f9d54e46872347837f9`.
Its review app embeds the unchanged backend from
`469649bfb800be8dd5f5808ad2bd4223ad15b2a8`, SHA-256
`894f84e919d178f51d142750e80fd0e7011d0a75cb83d765e970e8551e69c1aa`.
That backend passed the specified fresh synthetic Protocol, SDK, signed-sync and
receiver→Edge checks; it was not activated.
The UI-only artifact and receipt are under
`/Users/bolyki/dev/teslatlas-lab/reviews/hub-redesign-20260922/artifacts/5a839fe-ui-fixed`.

Source, static, focused, complete onboarding, backend and protocol findings found
by the independent review are repaired. This does not complete ordinary-user
acceptance. The Mac locked before the remaining live checks could run.

## Open acceptance gates

- Manually unlock the Mac and verify the real native routes, menus, focus, close
  and pending-operation ownership using the source-bound candidate.
- Record and inspect normal-motion and Reduce Motion behavior, including loading
  feedback and transitions.
- If the workspace parent authorizes activation, verify repaired-backend Home
  Assistant identity, polling and restart continuity through the ordinary route.
  The retained c209eed runtime evidence does not prove this for 469649b.
- Let the workspace parent reconcile the live evidence and make the independent
  acceptance decision. Hub source-only publication remains held until then.

The controlling workspace review is
[docs/development/assessments/hub-redesign-independent-review-2026-09-22.md](../../../docs/development/assessments/hub-redesign-independent-review-2026-09-22.md).
No current runtime activation, push, packaging, signing or distribution is
authorized by this plan.

## Historical accepted baseline

The previous `c209eed0d6c860fbbe3ceaa617c26fd40e0b4ad6` current-Mac
acceptance and its coordinated receipts remain immutable historical evidence.
They do not accept the later redesign candidate.

## Boundary

Packaging, distribution, notarization, release binaries, real Tesla access,
vehicle actions, public ingress, Intel, Azure and other platforms remain deferred
or separately gated. `app/` and Viewer remain out of scope.
