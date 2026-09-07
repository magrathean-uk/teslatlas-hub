# Public SDK integration roadmap

This roadmap records the packaged SDK boundary now used by the reference
client and the richer functions that remain outside the current Hub profile.

## Current gate

Live mode is bound to the verified `@teslatlas/sdk` `2026.36.2` package and
`hub-http-v1@1.0.0` profile. The repository vendors the exact tarball because
registry publication is not part of this candidate. Install, typecheck, test,
and build admission recompute its SHA-256 and the byte-for-byte installed
78-member manifest. Fixture names and richer display fields remain
viewer-owned examples rather than additions to the current profile.

## Contract catalogue

For each live screen action, keep the selected package boundary explicit:

- package versions and fixture-manifest digest;
- SDK export and released request/response types;
- required advertised capability and visibility scope;
- conformance fixture case and expected presentation state.

The adapter contains no route strings or direct `fetch`; the packaged SDK and
its public types remain authoritative.

## One network boundary

The SDK adapter is the only permitted network boundary. Views must not
construct URLs, call `fetch`, retain raw pairing material, interpret private
fields, merge records across sources, or calculate proprietary analytics.

Opaque cursors, conditional requests, event replay, typed errors, identity
validation, and caller-owned credential storage remain SDK responsibilities.
The viewer maps released responses into its small display model.

## State provenance

- Absence is not zero.
- Stale and inferred labels come only from released metadata or an explicitly
  released SDK policy applied to a public timestamp.
- Missing time means age unknown, not stale.
- Capability or scope denial becomes unavailable, without exposing hidden data.
- A resource failure preserves already-labelled prior data and offers a retry
  local to that resource.
- One collector path failing must not rewrite another path's evidence.

Fixture mode demonstrates these rules with viewer-owned markers. It does not
claim they are the eventual protocol fields.

## Remaining profile expansion

Add cases only after their public contracts exist:

- incompatible protocol and missing capability;
- denied pairing, denied revoke, and revoked-device refresh;
- insufficient vehicle scope and a removed selected vehicle;
- unknown observation age;
- drive and charge cursor, end-cursor, detail, and denied-detail behaviour;
- per-resource failure, retained evidence, and retry;
- event/query inconsistency.

Every public fixture should include synthetic identifiers, the released
protocol version and capability set, expected state, and a manifest checksum.
A real Hub may supplement this suite but cannot replace it as the test oracle.

## Evidence gate

Live acceptance needs all of the following from a clean checkout:

1. conformance-backed adapter and state-mapper tests;
2. a static boundary check rejecting direct transport imports and route literals
   outside the SDK adapter;
3. visible journey proof for health, pairing, vehicles, state, sessions,
   quality, freshness, device lifecycle, and clearing local session data;
4. keyboard, screen-reader smoke, reflow, contrast, and reduced-motion checks;
5. proof that no Teslatlas/Hub source checkout, Tesla account, proprietary
   asset, analytics service, remote logging, or private route is required.
