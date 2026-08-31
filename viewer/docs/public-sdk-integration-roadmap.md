# Public SDK integration roadmap

This roadmap preserves the strongest protocol-first requirements from the
parallel reference-client design without presenting unreleased APIs as real.

## Current gate

`teslatlas-protocol` and `teslatlas-sdk-typescript` are foundation-only. The
viewer therefore uses its own deterministic display fixtures and keeps live
mode unavailable. Fixture names, fields, timestamps, and operations are not a
draft public contract.

Do not enable live mode until released protocol, SDK, and conformance artifacts
exist.

## Contract catalogue

Before adding a live adapter, check in a catalogue derived from the selected
releases. For each screen action it must record:

- package versions and fixture-manifest digest;
- SDK export and released request/response types;
- required advertised capability and visibility scope;
- conformance fixture case and expected presentation state.

The catalogue must not contain guessed route strings. The released artifacts,
not this viewer's fixture model, remain authoritative.

## One network boundary

The future SDK adapter is the only permitted network boundary. Views must not
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

## Release-backed fixture expansion

Add cases only after their released contracts exist:

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

Live mode needs all of the following from a clean checkout:

1. conformance-backed adapter and state-mapper tests;
2. a static boundary check rejecting direct transport imports and route literals
   outside the SDK adapter;
3. visible journey proof for health, pairing, vehicles, state, sessions,
   quality, freshness, device lifecycle, and clearing local session data;
4. keyboard, screen-reader smoke, reflow, contrast, and reduced-motion checks;
5. proof that no Teslatlas/Hub source checkout, Tesla account, proprietary
   asset, analytics service, remote logging, or private route is required.
