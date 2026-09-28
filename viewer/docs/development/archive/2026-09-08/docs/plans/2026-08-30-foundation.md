# Viewer foundation plan

## Goal

Ship one small open client that proves the protocol works for a client other than Teslatlas.

## Dependencies

- Released TypeScript SDK and protocol fixtures.
- Test Hub or fixture-backed protocol harness.
- Public pairing and device-management contract.

## Delivery sequence

1. Define privacy, accessibility, browser-support, and deployment policy.
2. Implement public pairing and pinned-Hub discovery through the TypeScript SDK.
3. Implement the seven reference views with bounded loading, error, empty, and stale-data states.
4. Add deterministic UI and integration tests against the public conformance harness.
5. Demonstrate a clean checkout build with no proprietary app, Hub source, or Tesla account.
6. Publish a short protocol-learning guide linked to the exercised public resources.

## Acceptance

- A new contributor can use the viewer to pair and inspect fixture-backed Hub data.
- Every network call is represented in the released protocol contract.
- The viewer visibly distinguishes missing data from inferred or stale data.

## Out of scope

Feature parity with Teslatlas, vehicle command control, embedded Grafana, or a required cloud deployment.
