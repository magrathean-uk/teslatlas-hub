# TypeScript SDK post-adoption plan — 2026-09-19

Objective: Preserve the accepted Node/browser binding and prepare a clean,
unpublished npm distribution candidate.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[coordination](../../../docs/development/COORDINATION.md),
[App v7 handoff](../../../docs/development/APP_V7_READINESS.md), and
[STATUS.json](STATUS.json).

## Current position

G3 and G4 are accepted for the exact 81-member
`@teslatlas/sdk@2026.36.2` archive, SHA-256
`03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c`,
bound to `hub-http-v1@1.0.0`. Fresh Node and Chrome consumers proved TLS,
CORS, one-use claim, current/history, opaque pagination, `304`, rotation and
cleanup. This is runtime acceptance of the private candidate archive, not npm
publication or a broad Node/browser support-floor claim.

Do not rerun G3/G4 unless the source, archive, profile, Hub cohort or supported
target changes.

## Next goal draft — not started

L3: prepare a reproducible, publish-ready but unpublished npm candidate from the
accepted source. State a truthful Node/npm and browser support policy from
existing evidence; do not infer an untested minimum. Produce a deterministic
archive and provenance record, install it into fresh caller-owned Node and
browser-example roots without a Hub runtime, verify public imports and package
contents, then remove only those consumer roots.

Acceptance requires:

- exact source, lockfile, generated output, archive SHA-256 and ordered member
  manifest;
- package name/version, exports, licence, documentation and protocol lock bound
  to the accepted G3 profile;
- clean offline install/import of root, Node and browser entry points with no
  service, listener, credential or Hub state;
- uninstall/cleanup limited to caller-owned roots;
- explicit `not published` status and no support claim beyond evidence.

This draft does not authorize source changes, package generation, tests,
publication, or runtime work. The coordinator must create and start a new goal.

## Later work

Revalidate consumer setup/recovery only for a package, profile, source or target
delta. Named-source and real-data semantics are Hub/Protocol evidence consumed
by the SDK, not an independent collection lane. npm publication needs separate
owner authorization.

## Boundaries

The SDK is a caller-owned developer resource, never a daemon or Hub component.
Preserve the dirty `main` checkout. No App or Viewer work, x86/Intel/Azure,
production or vehicle action, commit, push, CI, release, publication, or reuse
of historical private inputs.
