# Teslatlas Home Assistant integration

Home Assistant custom integration for the public Teslatlas Hub protocol.

## Current status

The Home Assistant foundation is implemented and locally testable. Runtime network transport is deliberately disabled because `teslatlas-protocol` still contains foundation documents only: it has no frozen discovery, pairing, query, or event schemas.

The integration fails closed instead of inventing Hub routes. See [protocol readiness](docs/protocol-readiness.md).

Do not install this on a production Home Assistant instance yet. It has not been published through HACS.

## Implemented Home Assistant behaviour

- HACS-shaped `custom_components/teslatlas_hub` package and metadata.
- `local_push` hub manifest and `_teslatlas-hub._tcp.local.` discovery.
- Manual and Zeroconf config flows.
- Transient pairing-secret claim boundary; pairing secrets are never stored.
- Stable Hub identity, duplicate prevention, reauthentication, and reconfiguration.
- One initial current-state snapshot followed by push-only event updates.
- `Last-Event-ID` continuity, bounded reconnect, authentication-loss repair, and clean unload.
- One Hub device plus isolated vehicle devices.
- Read-only sensors, unavailable/unknown semantics, dynamic vehicle addition, and translated names.
- Diagnostics that redact endpoint, bearer, Hub identity, pairing material, coordinates, vehicle identity, and replay identity.
- Config-entry migration guard.
- Deterministic redacted fixtures and Home Assistant tests.

Vehicle sensors currently cover state of charge, charging state/power/limit, estimated range, odometer, activity, temperatures, access state, software state, telemetry age, and data quality. Hub sensors cover collector health, estimated Fleet API cost, and backup age.

These are integration-side fixture fields, not claims about the eventual public wire schema.

## Hard boundaries

The integration:

- uses only the future released public Hub API and event stream;
- never reads Hub SQLite or collector internals;
- never asks for Tesla credentials;
- never polls Tesla or calls private APIs;
- exposes no commands, buttons, switches, or command services;
- contains no GitHub Actions, hosted CI, Dependabot, or release automation.

## Local contributor setup

Requires Python 3.14.2 or newer and `uv`.

```bash
uv sync --group dev
uv run pytest
uv run pytest --cov=custom_components/teslatlas_hub --cov-report=term-missing
uv run ruff check .
uv run ruff format --check .
```

Fixtures live in `tests/fixtures`. They are deterministic, contain no VIN, coordinates, credentials, or provider payloads, and require no Tesla account or live Hub. Parsing stays in `tests/helpers.py`; production code does not treat the temporary fixture shape as a released protocol.

## HACS status

`hacs.json`, the custom-component layout, manifest version, documentation, issue tracker, and code owner are present. Publication is intentionally blocked until the public protocol is released, runtime conformance passes, a licensed brand asset is approved, required repository topics are set, and a real release exists. No repository publication or HACS submission has occurred.

## Licence

Apache-2.0.
