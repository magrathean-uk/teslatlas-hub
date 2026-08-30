# Protocol dependency gate

## Current result

Resource-level SDK development is blocked because no reproducible protocol revision
contains the required public authority artifacts.

Authority inspected:

- repository: `/Users/bolyki/dev/source/teslatlas-protocol`;
- commit: `b7b48a86a7705e8ab016f1debd25cecd20ebbb89`;
- tracked status: foundation only, with candidate resource names but no frozen schemas.

During SDK implementation, the protocol worktree gained untracked OpenAPI, schema, event,
fixture, and conformance work in progress. Those files are not part of the commit above,
cannot be pinned or reproduced by SDK consumers, and were not used as authority here.

The SDK implements only contract-neutral mechanics. Candidate routes and payload examples
in the product brief are not treated as released protocol.

## Required tracked upstream artifacts

| Missing authority artifact | SDK work blocked by it |
| --- | --- |
| Versioned discovery schema | Discovery client, Hub identity model, endpoint selection |
| Approved OpenAPI document | Generated paths, methods, query names, request and response types |
| JSON Schemas | Runtime payload validation and protocol-derived model exports |
| Event-stream contract | Event names, payload decoders, replay window, terminal reconnect rules |
| Authentication and pairing contract | Credential type, authorization scheme, scopes, rotation behavior |
| Stable error catalogue and envelope | Protocol error-code union, decoder, safe request-ID extraction |
| Pagination envelope | Cursor parameter and next-page extraction |
| Per-resource ETag rules | Conditional read/write wrappers and mutation conflict behavior |
| Command-job contract | Submission header, payload, states, polling, cancellation, expiry, verification |
| Redacted deterministic fixtures | Protocol-level SDK conformance tests |
| Language-neutral conformance runner | Cross-SDK compatibility evidence |
| Browser and Node.js support policy | Public runtime support matrix and package `engines` range |
| Two-minor-version policy | Frozen compatibility window and deprecation behavior |

## Gate-opening acceptance

Before generated code enters this repository:

1. Each artifact is committed in a reviewed protocol revision with a released version and
   stable repository path.
2. The protocol repository validates its schemas and fixtures independently.
3. Generation is deterministic from a clean checkout.
4. Generated diffs are reviewable and contain no Hub or proprietary source.
5. The same protocol fixtures pass through browser and Node.js adapters.
6. Command tests prove no blind retry and state-verification requirements.
7. The package support matrix names exact tested runtimes.

`openapi-typescript` plus `openapi-fetch` is the current free, maintained generation
candidate. It is not added until a real OpenAPI document exists and its generated surface
can be reviewed against protocol fixtures.

## Claims that remain unavailable

- functional public Hub resource client;
- protocol-derived vehicle, drive, charge, state, update, event, data-quality, or command
  payload types;
- authenticated pairing or credential-rotation flow;
- typed protocol error catalogue;
- command submission or job polling;
- Teslatlas protocol conformance;
- viewer integration proof;
- npm package readiness or publication readiness.
