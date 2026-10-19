# Protocol architecture

## Responsibility

Own versioned, implementation-neutral public contracts. Hub is the authoritative server implementation; this repository is the public compatibility authority. The current-Hub HTTP profile, the Hub sync profiles, and Edge delivery are separate contracts; select one before applying a document's rules.

## Goals and non-goals

The goal is a stable, versioned, source-neutral boundary that an unaffiliated
client can implement from public files alone, with executable evidence for
compatibility.

This repository does not define Hub internals, proprietary Teslatlas App
behavior, generated SDK implementations, hosted automation, or deployment
instructions.

## Normative language and change discipline

Uppercase `MUST`, `MUST NOT`, `REQUIRED`, `SHOULD`, `SHOULD NOT`, and `MAY` have
the meanings assigned by RFC 2119 and RFC 8174. Lowercase uses are descriptive.

Changes MUST remain implementation-neutral and MUST update every affected
schema, OpenAPI document, example, fixture, profile, conformance case,
generated artifact, and test. A published profile stays byte-identical; a change
is a new profile (see [`versioning.md`](../reference/versioning.md)).

Machine-readable artifacts are the wire authority. Prose explains intent and
cannot override them. Any artifact/prose conflict blocks release until the
conflict is resolved and the local validation gate passes.

## Contract layers

| Layer | Artifact | Purpose |
| --- | --- | --- |
| Current-Hub HTTP | `profiles/hub-http-v1/1.2.0/` | Discovery, pairing, authenticated reads and pages, rotation, revocation, and the bounded conformance contract |
| Hub sync | `profiles/hub-sync-v1/{1.3.0,1.4.0,1.5.0}/` | Changes-since, no-op, key discovery, and signed packs |
| Edge delivery | `profiles/edge-delivery-v2/2.0.0/` | Edge pull, stable identity, acknowledgement, and durable-consumer disposition |
| Conformance | `conformance/` | The `hub-sync-v1@1.3.0` fixture cases and the `hub-http-v1@1.2.0` offline and native checks |

## Public rules

- Semantic protocol versions and capability negotiation.
- Opaque cursors only; no offset pagination.
- UTC time bounds, stable machine-readable errors, ETags, and conditional requests.
- No secret, VIN, raw provider token, or precise-location requirement belongs in a public fixture.

## Boundaries

The protocol is permissively licensed. It contains schemas, fixtures, specifications, and independently authored examples only. It does not contain AGPL Hub implementation code or proprietary Teslatlas product code.

## Current-Hub surface

The `hub-http-v1@1.2.0` profile specifies `/.well-known/teslatlas-hub`, health
and readiness, pairing claim, the vehicle list and current state, drive, charge,
position and charge-sample pages, and credential rotation and revocation, by
OpenAPI and the profile's schemas. This prose explains boundaries;
machine-readable artifacts are the wire authority.

## Source-of-truth order

1. JSON Schemas define payload validity.
2. OpenAPI defines HTTP operations, parameters, status codes, and media types.
3. Profiles and conformance cases define executable behaviour.
4. Prose documents explain intent without overriding machine-readable rules.

The current-Hub OpenAPI 3.1.0 document references adjacent files; distribute and
bind the complete profile bundle as described in
[`current-hub.md`](../reference/current-hub.md).
