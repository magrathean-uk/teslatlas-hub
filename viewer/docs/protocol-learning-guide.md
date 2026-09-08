# Protocol learning guide

This guide explains what the viewer demonstrates through the current public
Hub profile and what remains richer fixture-only presentation.

## Contract status

The live adapter consumes only public exports from the packaged TypeScript SDK
for `hub-http-v1@1.0.0`: discovery, pairing, health, readiness, vehicles,
current state, and bounded drive pages. The richer JSON-like fixture values are
viewer-owned display examples. Do not copy their property names into a Hub or
SDK implementation.

## What each view teaches

| Viewer surface | Public concept exercised | Important client behaviour |
| --- | --- | --- |
| Pairing | Discovery identity and scoped paired devices | Confirm identity, keep fixture credentials in memory, show failures clearly |
| Hub health | Discovery versions and capabilities | Separate reachable, degraded, stale, and offline |
| Vehicles | Bounded vehicle collection | Treat an empty collection as a valid result |
| Current state | Latest vehicle projection | Keep absent and inferred fields explicit |
| Recent sessions | Bounded drive pages; richer fixture charge summaries | Preserve null distance/duration and label unavailable charge data |
| Data quality | Richer fixture-only projection evidence | Show unsupported for the current live profile |
| Collector freshness | Richer fixture-only collection evidence | Show unsupported for the current live profile |
| Paired devices | Local viewer lifecycle | Clear local credentials without claiming remote revocation |

## The SDK adapter

The live adapter:

1. imports types and client functions from the released package;
2. maps released response types into the viewer-owned display model;
3. leaves cursors, conditional requests, event replay, typed errors, and
   credential storage with the SDK;
4. has mapper/lifecycle tests for current-Hub null and failure semantics;
5. has a managed and installed-Hub browser acceptance path; those runs are
   recorded separately from the local fixture and unit evidence.

Do not retrofit fixture property names into that adapter. The released SDK is
authoritative even when its names differ.

See the [public SDK integration roadmap](public-sdk-integration-roadmap.md) for
the artifact and evidence gates.

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
