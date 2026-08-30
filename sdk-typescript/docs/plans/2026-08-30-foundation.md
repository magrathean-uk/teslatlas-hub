# TypeScript SDK foundation plan

## Goal

Provide a semantically identical public client path for browser and Node.js users.

## Dependencies

- Released protocol artifacts and conformance fixtures.
- Browser and Node.js support policy.
- Viewer proves the browser path; integration authors prove the Node.js path.

## Delivery sequence

1. Define package names, exports, runtime support matrix, semantic-version policy, and generated-artifact review rules.
2. Derive public types and error codes from protocol artifacts.
3. Add HTTP cursor/ETag support and SSE replay/reconnect support.
4. Add Node and browser transport adapters with caller-owned credential storage.
5. Run fixture suites for discovery, errors, pagination, resume after event loss, and non-idempotent command handling.
6. Use the SDK in the reference viewer and publish compatibility evidence.

## Acceptance

- Browser and Node.js samples use only published artifacts.
- The same fixtures pass through both transport adapters.
- Viewer needs no private route, model, or auth exception.

## Out of scope

A general-purpose web framework, a dashboard, server-side credentials, or a command console.
