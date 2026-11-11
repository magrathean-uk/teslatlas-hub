# Home Assistant integration foundation plan

Status: Superseded

This historical plan was replaced by the [approved concurrent foundation merge
design](../superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md).
It predates the checked `hub-http-v1@1.0.0` read-only polling profile; its
local-push, Zeroconf, and event-stream proposals are retained as history and
are not current support claims. Use the [current README](../../README.md) and
[working-product plan](../superpowers/plans/2026-09-08-working-product-plan.md)
for the active implementation and evidence boundary. Do not execute this
historical plan.

## Goal

Build a high-quality HACS custom integration that proves local push through the public Hub protocol.

## Dependencies

- Stable protocol discovery, pairing, queries, events, and data-quality fields.
- TypeScript or Python-free conformance fixtures usable by Home Assistant tests.
- Hub mDNS and device-scope behaviour.

## Delivery sequence

1. Freeze Home Assistant support, quality-level, entity, command, diagnostic, and translation policies.
2. Implement config flow, Zeroconf discovery, pairing/reauthentication, and reconfiguration via public protocol calls.
3. Map public query and event data into device classes and availability semantics.
4. Add offline/reconnect, authentication-loss, migration, diagnostics-redaction, and configuration-flow tests.
5. Package HACS metadata, documentation, and fixture-based developer setup.
6. Validate local-push operation against a fresh Hub installation without Tesla credentials in Home Assistant.

## Acceptance

- Updates arrive through the Hub public event interface.
- Lost connection, expired device bearer, and Hub endpoint changes recover predictably.
- Diagnostics redact secrets and precise locations by default.
- No integration code depends on Hub storage or collector internals.

## Out of scope

Hub-embedded Home Assistant code, raw MQTT replacement, and unrestricted vehicle commands.
