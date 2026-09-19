# Teslatlas Home Assistant integration

Home Assistant custom integration for the public Teslatlas Hub protocol.

The current release-cohort product version and its compatibility status are
described in [product versioning](docs/product-versioning.md).

## Current status

The integration implements the checked `hub-http-v1@1.0.0` current-Hub profile
with an asynchronous, bounded local polling client. It is a `2026.36.2`
release candidate. Its compatibility entry is accepted for the exact
source-built synthetic Debian 13 ARM64 Container lane recorded in the current
development receipt; it has not been published through HACS.

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

- uses only the public current-Hub HTTP API described by the embedded profile;
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
uv run --locked pytest --cov=custom_components/teslatlas_hub --cov-report=term-missing
uv run --locked ruff check .
uv run --locked ruff format --check custom_components tests/test_client.py tests/test_config_flow.py tests/test_coordinator.py tests/test_diagnostics.py tests/test_documentation.py tests/test_init.py tests/test_migration.py tests/test_models.py tests/test_package.py tests/test_sensor.py tests/helpers.py tests/conftest.py tests/integration/test_current_hub_client.py
```

The product-owned format check covers the component and its product tests.
The retained correction-3 matrix harness has a separate source-review format
exception; the full-tree format check remains pending until that inherited
review scope changes.

The approved profile resources are embedded under
`custom_components/teslatlas_hub/profile`. The opt-in `tools/test-live-hub`
entrypoint fails closed unless it receives private fixture, CA, synchronization,
and receipt paths.

## Container candidate

The repository includes a minimal official Home Assistant Container example in
[`compose.yaml`](compose.yaml) and [the Docker guide](docs/docker.md). It pins
the documented 2026.8.3 baseline, persists `ha-config`, and mounts only the
custom component read-only. A selected Debian 13 ARM64 lane has now passed
Container startup, the authenticated user config-flow API, scheduled
current-state refresh and outage recovery,
reauthentication after fixture revocation, reload/unload, and stop/start
persistence. The redacted receipt is recorded in
[`docs/development/STATUS.json`](docs/development/STATUS.json). This does not
claim HA OS, macOS, x86_64, installer, replacement-upgrade, or full matrix
acceptance.

## Manual installation and capabilities

For an existing Home Assistant installation, copy only
`custom_components/teslatlas_hub` into `/config/custom_components/teslatlas_hub`,
including its profile and translations, then restart Home Assistant. Add the
integration through the UI and enter the Hub hostname or IP, port, TLS choice,
optional certificate pin, and a short-lived Hub invitation. Do not copy
`tools/`, `.venv/`, tests, or fixture data into a production Home Assistant
configuration. This is a custom integration, not a Supervisor add-on or a HACS
publication.

### macOS selection boundary

On Apple-silicon macOS, a Hub package selector may offer this integration only
when an existing HA configuration or an explicitly selected HA Container/VM
target is available. The active working-product path may consume the accepted
Debian ARM64 HA Container through an operator-managed reachability path, such
as an existing authenticated tunnel; the Mac host does not become the HA
runtime. The option stages the same `custom_components/teslatlas_hub` payload
into the selected HA configuration; it never creates a native macOS Home
Assistant service, owns HA's database, or silently starts a hidden runtime.
If no supported HA OS/Container/VM target is available, the selector must show
the option as blocked with that reason. Mac reachability alone is not a claim
of authenticated pairing or native macOS HA acceptance.

### Apple-silicon Mac consumer access

When the accepted Debian ARM64 HA runtime is the selected target, the Mac host
can reach its UI through an operator-managed SSH forward. From this checkout,
run `tools/mac-consumer-forward` and open `http://127.0.0.1:18123/` in a
browser. The helper forwards only the existing HA UI port; it does not start a
guest, Docker/Colima, Home Assistant, or a native macOS service. Press
`Ctrl-C` to close the forward. The selected HA runtime still owns pairing,
credentials, configuration, database, and the Teslatlas Hub connection.

The current profile exposes public discovery, invitation claim, credential
rotation, vehicle listing, and current-state reads. The integration polls one
entry every 30 seconds, never overlaps refreshes, limits current reads to four
at a time, and bounds transient backoff at 300 seconds. It has no Zeroconf,
SSE, commands, or private collector calls. Numeric zero is retained; missing
fields remain unknown, and removed vehicles stay registered but unavailable.

Common setup failures are actionable: a wrong Hub identity or certificate/pin
mismatch means stop before pairing; an invalid or replayed invitation requires
a fresh Hub invitation; an unavailable endpoint needs network/TLS correction;
an expired or revoked device credential starts reauthentication; and an
unsupported profile must be upgraded or rejected before a bearer is sent.
Diagnostics omit credentials, pairing material, endpoints, identities, and
locations. The selected Debian ARM64 receipt also records live reconfigure,
replay rejection, clean stop/start, and diagnostics redaction. The local tests
and selected runtime receipt cover only the current source/runtime boundary;
replacement-upgrade, full parity/matrix, and three-target platform evidence
remain pending. Treat `core.config_entries` as credential-bearing state and
keep its host permissions owner-only.

For an existing entry, reload the integration from Home Assistant's Settings →
Devices & services page after changing the mounted component. If the device
credential expires or is revoked, follow the entry's repair or reauthentication
prompt and provide a fresh invitation; the old bearer is replaced only after
the Hub identity and an authenticated read pass. Endpoint or TLS changes use
the entry's reconfigure flow and must resolve to the same Hub identity.

See [the architecture notes](docs/architecture.md) for request and lifecycle
boundaries, [the public protocol binding](docs/protocol-readiness.md) for the
profile status, [product versioning](docs/product-versioning.md) for the
version distinction, and [the Container guide](docs/docker.md) for backup,
update, and TLS handling.

## Configuration backup and rollback

Before replacing this custom component in an existing Home Assistant instance,
stop Home Assistant and make a private backup of its `/config` directory and
the exact previous component bytes. To roll back, restore those matching
component bytes and the configuration backup while Home Assistant is stopped,
then start it again. An older component cannot be expected to read an unknown
newer major config-entry schema; keep the backup and component version paired.

## HACS status

`hacs.json`, the custom-component layout, manifest version, documentation, issue tracker, and code owner are present. Publication is intentionally blocked until the public protocol is released, runtime conformance passes, a licensed brand asset is approved, required repository topics are set, and a real release exists. No repository publication or HACS submission has occurred.

## Licence

Apache-2.0.
