# Teslatlas Home Assistant integration

Home Assistant custom integration for the public Teslatlas Hub protocol.

The current release-cohort product version and its compatibility status are
described in [product versioning](docs/product-versioning.md).

## Current status

The integration implements the frozen `hub-http-v1@1.0.0` current-Hub profile
with an asynchronous, bounded local polling client. It is a `2026.36.2`
release candidate and has not been published through HACS.

Do not install this on a production Home Assistant instance yet. It has not been published through HACS.

## Implemented Home Assistant behaviour

- HACS-shaped `custom_components/teslatlas_hub` package and metadata.
- `local_poll` hub manifest and one non-overlapping 30-second refresh authority.
- Reliable manual endpoint configuration; Zeroconf remains disabled because the
  current Hub does not publish the required identity and TLS discovery fields.
- Private invitation claim using endpoint, TLS pin, pairing UUID, secret, and
  device name; invitation material is never stored.
- Stable Hub identity, duplicate prevention, reauthentication, and reconfiguration.
- Unauthenticated identity checks before a saved bearer is sent, device-bearer
  rotation, expiry/401 reauthentication, bounded retry, and clean unload.
- Vehicle and current-state polling with at most four current reads in flight.
- Isolated vehicle devices with stable matching entity identities.
- Read-only sensors, unavailable/unknown semantics, dynamic vehicle addition, and translated names.
- Diagnostics that redact endpoint, bearer, Hub identity, pairing material, coordinates, vehicle identity, and replay identity.
- Config-entry migration guard.
- Deterministic redacted fixtures and Home Assistant tests.

Vehicle sensors cover the current profile's bound state of charge, charging
state/power/limit, estimated range, odometer, activity, temperatures, lock
state, software state, and observation-derived telemetry age. Collector health,
Fleet cost, backup age, and data quality are not public current-Hub fields and
are not exposed. The profile has no SSE event route.

## Hard boundaries

The integration:

- uses only the released public current-Hub HTTP API;
- never reads Hub SQLite or collector internals;
- never asks for Tesla credentials;
- never polls Tesla or calls private APIs;
- exposes no commands, buttons, switches, or command services;
- contains no GitHub Actions, hosted CI, Dependabot, or release automation.

## Local contributor setup

Requires Python 3.14.2 or newer and `uv`.

```bash
uv sync --locked --group dev
uv run --locked pytest
uv run pytest --cov=custom_components/teslatlas_hub --cov-report=term-missing
uv run ruff check .
uv run ruff format --check .
```

The approved profile resources are embedded under
`custom_components/teslatlas_hub/profile`. The opt-in `tools/test-live-hub`
entrypoint fails closed unless it receives private fixture, CA, synchronization,
and receipt paths.

## HACS status

`hacs.json`, the custom-component layout, manifest version, documentation, issue tracker, and code owner are present. Publication is intentionally blocked until the public protocol is released, runtime conformance passes, a licensed brand asset is approved, required repository topics are set, and a real release exists. No repository publication or HACS submission has occurred.

## Licence

Apache-2.0.
