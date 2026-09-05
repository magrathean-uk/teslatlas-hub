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

## GPT-6 Astra execution

Reference: [OpenAI GPT-6 Astra guide](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6-astra), reviewed 2026-09-05.
These execution conventions preserve the repository-specific rules above and do
not change the host's model defaults or API configuration.

- Infer the intended objective and scope, then carry authorized work through to
  completion. Resolve routine choices with judgment; ask focused questions only
  when material uncertainty affects the result, scope, or required authority.
- Retain the user's session permissions and preferences. Complete authorized work
  that makes the result concrete and reviewable before a necessary final
  approval; explain the specific remaining approval requirement.
- Follow current user directions over skill guidelines within higher-priority
  instructions and tool constraints. If a skill blocks progress, link the exact
  `SKILL.md`, quote its relevant rule, and explain how it applies.
- Treat later messages as steering the current objective unless the user
  explicitly cancels it or replaces it with an incompatible objective.
- Delegate bounded independent work to available agents when useful work can
  continue in parallel. Assign file ownership to avoid shared edit races. Batch
  independent reads; keep dependent operations and conflicting edits sequential.
- Scale meaningful verification to the change. Once relevant checks pass, stop
  repeating or broadening them unless new changes, failures, or unresolved
  concerns justify it. Preserve required repository gates.
- Report failed, blocked, and untested paths accurately. Separate inspection,
  local tests, simulator evidence, and real-device or live-service acceptance
  where applicable; compilation alone does not prove acceptance.
- Use concise, plain, outcome-first prose and meaningful progress updates.
  Explain what changed, why, the supporting evidence, and material limits.
  Keep messages between agents legible, with normal spacing.
