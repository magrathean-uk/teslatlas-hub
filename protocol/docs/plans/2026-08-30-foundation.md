# Protocol foundation plan

## Status

The v1 contract foundation is implemented locally. The current profile is
`1.2.0`; the compatibility window is `1.0.0`, `1.1.0`, and `1.2.0`.

Local deliverables are:

- `schemas/*.schema.json`: JSON Schema 2020-12 domain and conformance contracts;
- `openapi/teslatlas-v1.openapi.json`: generated, self-contained OpenAPI 3.1.1;
- `events/teslatlas-v1.sse.json`: SSE framing, replay, reset, and event catalogue;
- `examples/`, `fixtures/v1/`, and `fixtures/manifest.json`: deterministic,
  redacted examples and observation scenarios;
- `compatibility/`: current and previous two minor profiles;
- `conformance/`: JSONL adapter protocol, executable cases, and runner;
- `tools/build_openapi.py`, `tools/build_fixtures.py`, and
  `tools/build_conformance.py`: deterministic generators and drift checks.

The local evidence is `./tools/check`: 54 unit tests pass and all 31 selected
profile/case runs pass. This proves artifact consistency and runner/reference
adapter behavior only. It does not prove a Hub, SDK, deployment, credential
flow, production data path, or external implementation conforms.

## Delivered contract

The implemented v1 surface covers discovery and version negotiation, bounded
HTTP query resources, opaque cursors, conditional requests and ETags, problem
details, immutable observations and projections, data quality, asynchronous
idempotent command jobs, mutable metadata and tombstones, and SSE replay/reset.

The JSONL adapter is language-neutral. The runner validates each response
envelope before expectations, rejects malformed or non-finite JSON, applies
semantic checks that JSON Schema cannot express, isolates state per case, and
rejects future event types in older profiles. The v1 SSE wire remains the
legacy event envelope; identity and revision equality are documented semantic
rules, not a wire redesign.

## Local acceptance

- Every shipped schema, example, fixture, conformance artifact, and generated
  OpenAPI document passes the local validation gate.
- Fixture generation, OpenAPI generation, and conformance generation are byte
  deterministic and checked for drift.
- Profiles cover the current minor and exactly its two predecessors.
- Fixtures contain synthetic/redacted data and no required Tesla account,
  Hub database, secret, VIN, or proprietary source.

These checks are release-authoring evidence, not deployment or interoperability
proof. Deployment proof requires a separately authorized external adapter or
implementation run and must record its own redacted evidence.

## Future versioned work

These decisions came from the earlier authoring plan. They are not silently
added to the current wire contract.

1. Add release governance: an append-only `docs/changes.md`, release evidence
   index, artifact digests, and an explicit immutable-baseline process.
2. Decide projection provenance requirements. If reproducibility needs a
   required projection revision, public algorithm identity/documentation, or
   RFC 8785 input digest, specify and version that addition together across
   schemas, OpenAPI, examples, fixtures, and cases.
3. Decide metadata value privacy constraints. If values must have constrained
   display names, tags, annotations, and explicit bans on credentials, secrets,
   executable content, or location history, add them as a versioned schema and
   semantic contract with negative cases.
4. Add a separate client-conformance profile and result schema for discovery
   first-use, retry, deduplication, conditional writes, replay, and reset.
   Run it against independently implemented clients; do not call the current
   server-style runner evidence client proof.
5. Decide whether raw observation, vehicle-singleton, and state-history REST
   endpoints belong in the public surface. Any addition must be capability
   declared, documented, schema-backed, and introduced in a future profile.

## Superseded assumptions

The current contract intentionally does not adopt the earlier draft's
OpenAPI 3.2/AsyncAPI 3.1 artifact layout, normalized event envelope, SSE query
fallback, `406` version error, zero-to-six timestamp fractions, or generic
16–128-character idempotency keys. Current artifacts and prose are authoritative;
those changes require explicit versioned contract work.

## Out of scope

Hub implementation, generated SDK source, product code, deployment automation,
production credentials or data, hosted CI, dashboard, and command UI.
