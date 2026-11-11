# HACS Local-Push Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a locally validated, HACS-shaped Teslatlas Hub integration foundation without inventing the unfinished public transport contract.

**Architecture:** Home Assistant code depends on a typed public-client boundary. Tests inject deterministic fixture clients; the production boundary fails closed until released protocol artifacts define routes and schemas. A push-only coordinator performs one setup snapshot, consumes replayable events, and owns availability/reconnect state.

**Tech Stack:** Python 3.14.2+, Home Assistant 2026.8.3, pytest-homeassistant-custom-component 0.13.357, pytest, Ruff, aiohttp supplied by Home Assistant.

**Spec:** `docs/superpowers/specs/2026-08-30-hacs-local-push-foundation-design.md`

## Global Constraints

- Work directly on `main`; preserve unrelated changes.
- Use only public Hub APIs and events; encode no unfrozen route or schema.
- Never read Hub SQLite, request Tesla credentials, poll Tesla, call private APIs, or touch `teslatlas-service/app`.
- Add no commands, services, GitHub Actions, hosted CI, Dependabot, release automation, or HACS publication.
- Pairing secrets are transient; device bearers are redacted everywhere outside config-entry storage.
- Use `_teslatlas-hub._tcp.local.` without assuming Zeroconf TXT fields.
- Keep one writer and perform one verified bulk commit/push.

---

### Task 1: Local package and validation foundation

**Files:**
- Create: `pyproject.toml`
- Create: `.gitignore`
- Create: `hacs.json`
- Create: `custom_components/teslatlas_hub/manifest.json`
- Create: `custom_components/teslatlas_hub/const.py`
- Create: `tests/test_package.py`

**Interfaces:**
- Produces: integration domain `teslatlas_hub`, platforms `sensor`, manifest version `0.1.0`, minimum Home Assistant `2026.8.0` in HACS metadata.

- [ ] Write `tests/test_package.py` to load JSON files and assert the domain, local-push class, config flow, hub type, Zeroconf service, command absence, and HACS domain.
- [ ] Run `uv run pytest tests/test_package.py -q`; verify collection/import failure because files do not exist.
- [ ] Add pinned development dependencies and the minimal metadata files. Do not add workflow files.
- [ ] Run `uv lock`, then `uv run pytest tests/test_package.py -q`; expect pass.
- [ ] Run `uv run ruff check .` and `uv run ruff format --check .`.

### Task 2: Typed model and fixture-client boundary

**Files:**
- Create: `custom_components/teslatlas_hub/models.py`
- Create: `custom_components/teslatlas_hub/client.py`
- Create: `tests/fixtures/initial-snapshot.json`
- Create: `tests/fixtures/vehicle-update.json`
- Create: `tests/helpers.py`
- Create: `tests/test_models.py`
- Create: `tests/test_client.py`

**Interfaces:**
- Produces: immutable `HubEndpoint`, `HubInfo`, `PairingResult`, `HubStatus`, `VehicleState`, `HubSnapshot`, and `HubEvent` dataclasses.
- Produces: `TeslatlasHubClient` protocol with `async_probe()`, `async_pair(secret)`, `async_snapshot()`, `async_events(last_event_id)`, and `async_close()`.
- Produces: `create_client(endpoint, bearer_token)` that returns a fail-closed pending-contract client.

- [ ] Write literal fixture parsing tests covering one Hub, two vehicles, every initial entity field, and an event replacement.
- [ ] Run focused tests; verify missing model/client imports fail.
- [ ] Implement immutable models and fixture-only parsing helpers under `tests/`; production code must not parse the temporary fixture shape.
- [ ] Write a test proving every production-client operation raises `ProtocolContractUnavailable` and includes no HTTP route.
- [ ] Implement the protocol and pending-contract client; run focused tests to green.

### Task 3: Config flow, pairing, discovery, reauthentication, and reconfiguration

**Files:**
- Create: `custom_components/teslatlas_hub/config_flow.py`
- Create: `custom_components/teslatlas_hub/strings.json`
- Create: `custom_components/teslatlas_hub/translations/en.json`
- Create: `tests/conftest.py`
- Create: `tests/test_config_flow.py`

**Interfaces:**
- Consumes: `create_client`, `HubEndpoint`, client errors, `HubInfo`, `PairingResult`.
- Produces: config-entry data keys `host`, `port`, `use_tls`, `hub_id`, `access_token`; entry version `1`, minor version `1`.

- [ ] Write manual-flow tests: form, successful transient secret claim, connection error, invalid secret, pending contract, duplicate Hub, and no persisted pairing secret.
- [ ] Run the manual tests; verify missing flow fails.
- [ ] Implement manual endpoint and pairing steps with password selectors and client validation.
- [ ] Write Zeroconf tests: discovered host/port, client-probed stable ID, duplicate update, and no TXT dependency.
- [ ] Run to red, implement `async_step_zeroconf`, then run to green.
- [ ] Write reauth tests proving token replacement on the same Hub, rejection of another Hub, and update/abort rather than a new entry.
- [ ] Run to red, implement reauth, then run to green.
- [ ] Write reconfigure tests proving endpoint replacement only after probing the same Hub and no token change.
- [ ] Run to red, implement reconfigure, then run all config-flow tests and require full module coverage.

### Task 4: Runtime setup, migration, push reconnect, and unload

**Files:**
- Create: `custom_components/teslatlas_hub/__init__.py`
- Create: `custom_components/teslatlas_hub/coordinator.py`
- Create: `tests/test_init.py`
- Create: `tests/test_migration.py`
- Create: `tests/test_coordinator.py`

**Interfaces:**
- Produces: typed `TeslatlasConfigEntry` runtime data containing client and `TeslatlasDataCoordinator`.
- Produces: one initial `async_snapshot()` call, push task using `async_events(last_event_id)`, capped delays `1, 2, 4, 8, 16, 30` seconds, and clean cancellation/close.

- [ ] Write setup tests for successful first snapshot, connection-not-ready, authentication-triggered reauth, platform forwarding, and unload cleanup.
- [ ] Run to red, implement setup/unload and error translation, then run to green.
- [ ] Write migration tests proving version 1/minor 0 advances to minor 1 without changing data and future major versions fail closed.
- [ ] Run to red, implement `async_migrate_entry`, then run to green.
- [ ] Write coordinator tests for event delivery, replay cursor reuse, unavailable state on disconnect, exact capped reconnect sequence, recovery on the first replayed event, auth-loss reauth, and task cancellation.
- [ ] Run to red, implement push coordinator with an injected sleeper for deterministic tests, then run to green.
- [ ] Assert no periodic update interval exists and no Tesla/provider client dependency is imported.

### Task 5: Devices, sensors, dynamic additions, and availability

**Files:**
- Create: `custom_components/teslatlas_hub/entity.py`
- Create: `custom_components/teslatlas_hub/sensor.py`
- Create: `tests/test_sensor.py`

**Interfaces:**
- Consumes: immutable coordinator snapshots.
- Produces: stable Hub and vehicle `DeviceInfo`; translated sensor descriptions; unique IDs `{hub_id}_{scope_id}_{key}`.

- [ ] Write entity tests for Hub and vehicle device creation, exact unique IDs, translated entity names, units/device classes/state classes, unknown missing fields, unavailable disconnect, and no command/service entities.
- [ ] Run to red, implement base and sensor entities, then run to green.
- [ ] Write a test where a new vehicle arrives by push and gets one entity set without duplicates.
- [ ] Run to red, add listener-driven dynamic entity creation, then run all sensor tests.

### Task 6: Diagnostics, redaction, fixtures, and contributor documentation

**Files:**
- Create: `custom_components/teslatlas_hub/diagnostics.py`
- Create: `tests/test_diagnostics.py`
- Create: `docs/protocol-readiness.md`
- Modify: `README.md`
- Modify: `docs/architecture.md`

**Interfaces:**
- Produces: config-entry diagnostics with redacted endpoint/token and aggregate runtime state only.
- Produces: local commands `uv sync --group dev`, `uv run pytest`, `uv run ruff check .`, and `uv run ruff format --check .`.

- [ ] Write diagnostics tests that serialize output and prove fixture token, host, vehicle IDs/names, coordinates, and event ID are absent.
- [ ] Run to red, implement minimal safe diagnostics, then run to green.
- [ ] Document fixture provenance as integration-internal, deterministic, redacted, and not a protocol promise.
- [ ] Document every missing upstream artifact and the fail-closed production behaviour.
- [ ] Update README with local setup, architecture, entity list, privacy boundary, commands disabled, and no-live-Tesla requirement.

### Task 7: Full verification, read-only review, and bulk publication

**Files:**
- Review all changed files.

**Interfaces:**
- Produces: one coherent verified commit on `main`; one push to `origin/main`.

- [ ] Run `uv run pytest --cov=custom_components/teslatlas_hub --cov-report=term-missing`; require all tests pass, config flow 100%, and package coverage at least 95%.
- [ ] Run `uv run ruff format --check .` and `uv run ruff check .`.
- [ ] Parse every JSON fixture/metadata/translation file with Python.
- [ ] Run a source-boundary scan for SQLite, Tesla credentials, polling intervals, private API paths, command services, workflow files, and references to `teslatlas-service/app`.
- [ ] Inspect `git diff --check`, `git status --short`, and the full diff.
- [ ] Dispatch a read-only independent review for P1/P2 correctness, privacy, HA conventions, and invented-contract violations; fix only validated findings through new failing tests.
- [ ] Repeat the full verification after all fixes.
- [ ] Commit once with `feat: add Home Assistant local-push foundation` and push `main` once.
