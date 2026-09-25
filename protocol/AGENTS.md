# Teslatlas protocol

Follow `../AGENTS.md` and `../docs/development/COORDINATION.md`, then this
product's `docs/development/PLAN.md` and `STATUS.json`. Model and effort defaults
are in `../AGENTS.md`. Work only on the assigned scope. App v7 (`../app`) consumes
this product; change the App only as App work. Viewer is excluded.

Run commands through `../scripts/dev/run.sh teslatlas-protocol COMMAND...` so build output and
caches stay out of this tree (clean-development routes the `uv` cache; see `.clean-development.json`).

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

## Current-Hub checker

- Preserve exact wire spellings where the current-Hub profile defines them,
  including `sourceUrl`, `pairingId`, `expiresAtMs`, and `tlsPin`; the general
  naming convention does not rename published fields.
- `tools/check-current-hub` is a bounded, read-only remote-wire smoke. It
  cannot prove pairing, lifecycle, native process provenance, installed-host
  acceptance, or complete drive history.
- Its private configuration directory and bearer header must remain owner-only
  and must never be added to examples, test fixtures, image layers, output, or
  diagnostics. The image gate is offline after its locked dependency install;
  an unrun Docker definition is static evidence only.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.
