# Teslatlas protocol

Public, source-neutral protocol contracts for Teslatlas Hub clients and integrations.

## Status

This repository carries three related contracts with different identities:

- Candidate current-Hub HTTP: `hub-http-v1@1.0.0`, documented in
  [`docs/current-hub.md`](docs/current-hub.md). It describes discovery,
  pairing, authenticated vehicle/current/drives reads, and credential
  rotation.
- Rich semantic HTTP and event profiles: `1.0.0`, `1.1.0`, and `1.2.0`.
  The local conformance gate covers the current minor and its two predecessors.
  Their walkthrough is [`docs/client-quickstart.md`](docs/client-quickstart.md)
  and includes version negotiation, SSE, commands, and metadata.
- Edge delivery: `profiles/edge-delivery-v2/2.0.0/`, with its own pull and
  acknowledgement contract.

These are protocol profiles, not claims about a deployed Hub or SDK release.

This repository contains no Hub implementation, generated SDK, or proprietary
Teslatlas source.

## Purpose

This repository owns the stable public boundary between Teslatlas Hub and Teslatlas, public SDKs, open reference clients, Home Assistant, user-operated edge receivers, and future third-party integrations.

The protocol is implementable from this repository alone. A client does not
need Hub Rust or proprietary Teslatlas source.

## Contract discipline

This repository is source-neutral. Public files must be sufficient for an
unaffiliated client to implement the contract without Hub Rust, proprietary App
source, or private deployment knowledge. `MUST`, `MUST NOT`, `REQUIRED`,
`SHOULD`, `SHOULD NOT`, and `MAY` are normative terms as defined by RFC 2119
and RFC 8174 when written in uppercase.

Machine-readable artifacts are the wire authority. Prose explains intent but
must not contradict schemas, OpenAPI, the SSE JSON contract, profiles, or
conformance cases; an unresolved conflict blocks release. A contract change
updates every affected artifact, example, fixture, profile, generated output,
and test, with the local gate as the release check.

## Goals and non-goals

Goal: publish stable, versioned, implementation-neutral contracts and
executable evidence for independent clients.

Non-goals: Hub internals, proprietary App behavior, generated SDK
implementations, hosted automation, or deployment instructions.

## Validate locally

Install [uv](https://docs.astral.sh/uv/), then run:

```sh
uv sync --locked
./tools/check
```

The gate validates every JSON Schema, OpenAPI, SSE example, valid and invalid
example, deterministic fixture, conformance vector, and all three compatibility
profiles. It uses no hosted CI.

## Artifacts

- `openapi/teslatlas-v1.openapi.json` — self-contained OpenAPI 3.1.1 rich query API.
- `schemas/` — canonical JSON Schema 2020-12 contracts.
- `events/teslatlas-v1.sse.json` — SSE framing, replay, and event catalogue.
- `examples/` — redacted valid and deliberately invalid wire examples.
- `fixtures/v1/` — deterministic normal, duplicate, delayed, missing, and
  reordered observation scenarios.
- `compatibility/` — the current and previous two minor profiles.
- `profiles/hub-http-v1/1.0.0/` — current-Hub HTTP schemas, OpenAPI 3.1.0,
  examples, cases, and content hash. Distribute the complete directory because
  its OpenAPI references adjacent schema files.
- `profiles/edge-delivery-v2/2.0.0/` — Edge pull, stable identity,
  sequence/gap, acknowledgement, and durable-consumer disposition contract.
- `conformance/` — JSONL adapter protocol, executable cases, and runner.

## Read next

- [Product versioning](docs/product-versioning.md)
- [Architecture](docs/architecture.md)
- [Current-Hub walkthrough](docs/current-hub.md)
- [Docker checker and read-only smoke](docs/docker.md)
- [Compatibility record](docs/compatibility.md)
- [Verification status](docs/verification.md)
- [Rich-profile client walkthrough](docs/client-quickstart.md)
- [Rich-profile HTTP contract](docs/http.md)
- [Canonical data model](docs/data-model.md)
- [Event stream](docs/events.md)
- [Commands and metadata](docs/commands-and-metadata.md)
- [Conformance](docs/conformance.md)
- [Normative references](docs/standards.md)
- [Foundation plan](docs/plans/2026-08-30-foundation.md)

## Licence

Apache-2.0.
