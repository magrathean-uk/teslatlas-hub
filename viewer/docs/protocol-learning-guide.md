# Protocol learning guide

This guide explains what the viewer demonstrates without pretending that the
foundation protocol is already frozen.

## Contract status

`teslatlas-protocol` currently documents contract layers and candidate
resources. It contains no released OpenAPI, JSON Schema, event schema,
compatibility fixture, or conformance runner. `teslatlas-sdk-typescript`
contains no package or runtime client.

The JSON-like TypeScript values in this viewer are therefore viewer-owned
display fixtures. They are deterministic and source-neutral, but they are not
released protocol examples. Do not copy their property names into a Hub or SDK
implementation.

## What each view teaches

| Viewer surface | Public concept exercised | Important client behaviour |
| --- | --- | --- |
| Pairing | Discovery identity and scoped paired devices | Confirm identity, keep fixture credentials in memory, show failures clearly |
| Hub health | Discovery versions and capabilities | Separate reachable, degraded, stale, and offline |
| Vehicles | Bounded vehicle collection | Treat an empty collection as a valid result |
| Current state | Latest vehicle projection | Keep absent and inferred fields explicit |
| Recent sessions | Drive and charge summaries | Keep sources, gaps, and derived fields beside session values |
| Data quality | Projection coverage and telemetry gaps | Never hide unresolved loss or silently interpolate |
| Collector freshness | Independent collection paths | One path can degrade or fail without rewriting another path's status |
| Paired devices | Device lifecycle and scopes | Show least-privilege scope and announce removal |

## The future SDK adapter

When released artifacts exist, add one adapter that:

1. imports types and client functions from the released package;
2. maps released response types into the viewer-owned display model;
3. leaves cursors, conditional requests, event replay, typed errors, and
   credential storage with the SDK;
4. adds conformance-backed adapter tests from released fixtures;
5. removes the unavailable-live source only after clean-checkout browser proof.

Do not retrofit fixture property names into that adapter. The released SDK is
authoritative even when its names differ.

## Fixture scenarios

The state selector and query input expose:

```text
complete
empty
stale
inferred
degraded
offline
error
loading
```

Each fixture uses a fixed clock and redacted identifiers. Browser tests also
assert that fixture mode sends no public API request.
