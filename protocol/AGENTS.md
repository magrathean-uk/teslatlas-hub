# Teslatlas protocol

This repository owns public contracts, not Hub implementation.

- Use lowercase, hyphenated documentation names and lowercase `snake_case` schema fields.
- Keep schemas source-neutral and independently implementable.
- Preserve semantic versioning, capability negotiation, opaque cursors, UTC timestamps, ETags, and stable errors.
- Fixtures must be deterministic and redacted.
- Do not copy AGPL Hub implementation or proprietary Teslatlas source here.

## Contract discipline

- This repository is source-neutral: names, fields, algorithms, limits, and
  semantics must be implementable by an unaffiliated client from public files.
- RFC 2119 and RFC 8174 define the meaning of `MUST`, `MUST NOT`, `REQUIRED`,
  `SHOULD`, `SHOULD NOT`, and `MAY` when written in uppercase.
- Every contract change MUST update affected schemas, OpenAPI, SSE contract,
  examples, fixtures, profiles, conformance cases, generated artifacts, and
  tests. Do not silently change wire behavior.
- Machine-readable artifacts are the wire authority. Any artifact/prose
  conflict blocks release until resolved and verified by the local gate.
- Keep current behavior and the current profile plus previous two minor
  profiles unless a documented compatibility change is intentionally approved.

## Goals and non-goals

- Goal: publish stable, versioned, implementation-neutral contracts and
  executable evidence for independent clients.
- Non-goal: define Hub internals, proprietary App behavior, deployment
  automation, or generated SDK implementations.
