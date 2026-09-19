# Protocol post-adoption plan — 2026-09-19

Objective: Preserve the accepted exact bindings and prepare a deterministic,
unpublished Protocol developer bundle.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[coordination](../../../docs/development/COORDINATION.md),
[App v7 handoff](../../../docs/development/APP_V7_READINESS.md), and
[STATUS.json](STATUS.json).

## Current position

G3 is accepted for product `2026.36.2`. The five active compatibility records
bind `hub-http-v1@1.0.0` at SHA-256
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`
and Edge's separate `edge-delivery-v2@2.0.0` identity to accepted runtime
receipts. The scoped admission checker is fail-closed and does not inspect
Viewer.

Protocol is a source-neutral developer resource, not a daemon. Existing source,
fixture and conformance passes do not constitute a packaged distribution or
real-data parity proof. Do not change the profile or rerun admission without a
concrete contract, source, product-version or evidence delta.

## Next goal draft — not started

L3: produce a deterministic, unpublished Protocol contract bundle for the
accepted profile. Include only public schemas, OpenAPI, profile metadata,
checksum maps, redacted fixtures, conformance entry points, compatibility
record, licence and documentation needed by an independent implementer. Keep
rich Protocol and Edge delivery identities separate from the current-Hub
profile.

Acceptance requires:

- ordered member manifest and archive SHA-256 tied to exact source and G3;
- generated files and checksum maps reproducing without drift;
- a clean offline extraction/check that creates no service, listener,
  credential or Hub state;
- byte agreement with the TypeScript vendored profile where G3 requires it;
- explicit `not published` and developer-resource-only status.

This draft does not authorize source changes, archive generation, checks,
publication, or runtime work. The coordinator must create and start a new goal.

## Later work

Named-source field, unit, null/zero and passive recovery evidence may be added
only from a fresh owner-authorized real-data lane. Any real contract gap must
update all affected machine-readable artifacts, fixtures, conformance and
consumers; do not invent routes to create work.

## Boundaries

Preserve the dirty `main` checkout and source-neutral contract discipline. No
Hub implementation, App or Viewer work, x86/Intel/Azure, production or vehicle
action, commit, push, CI, release, publication, or private credential content.
