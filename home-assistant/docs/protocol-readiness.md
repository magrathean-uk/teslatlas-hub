# Public protocol readiness gate

## Current evidence

`teslatlas-protocol` at foundation commit `b7b48a8` explicitly states that it contains no deployed protocol implementation, generated SDK, compatibility promise, frozen resource schemas, fixtures, or conformance runner.

Only architectural intentions exist today: semantic versions, capability negotiation, opaque cursors, UTC bounds, ETags, stable errors, SSE `Last-Event-ID`, and candidate resources. Those rules are insufficient to implement a real client without inventing details.

## Required before enabling runtime transport

The protocol repository must release and version all of:

1. well-known discovery response and stable Hub identity rules;
2. `_teslatlas-hub._tcp.local.` TXT fields, endpoint priority, and TLS/pinning rules;
3. pairing invitation, claim, status, cancellation, bearer rotation, and error contracts;
4. current Hub/vehicle query routes, request limits, response schemas, and cache semantics;
5. SSE URL, event names, payload schemas, heartbeat/end rules, replay bounds, and `Last-Event-ID` errors;
6. protocol/capability negotiation and two-minor-version compatibility rules;
7. stable authentication, identity-change, unsupported-version, and rate-limit error codes;
8. deterministic redacted fixtures plus a conformance runner.

## Behaviour until the gate closes

`client.create_client()` returns a fail-closed adapter. Probe, pairing, snapshot, and event operations raise `ProtocolContractUnavailable`. Home Assistant config flow shows `protocol_not_ready`; config-entry setup remains retryable.

Test clients implement the typed boundary entirely in memory. Their JSON inputs exist only to verify Home Assistant behaviour and must not be copied into a production parser.

## Commands

Commands are not part of this integration. Adding a command client, service, entity, scope, or confirmation flow requires separate written scope and authorization after the read-only protocol is released.

## HACS publication gate

Do not publish until runtime conformance is demonstrated against a fresh Hub using only released public artifacts, the repository has an approved/licensed brand asset, required repository topics, and a public release, and the then-current HACS validation requirements are met without adding unapproved GitHub automation.
