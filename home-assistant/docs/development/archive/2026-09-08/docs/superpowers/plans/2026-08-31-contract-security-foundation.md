# Contract and Security Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace temporary fixture semantics with bounded, protocol `1.2.0` parsing and close the config-flow and diagnostics security gaps without enabling production pairing or transport.

**Architecture:** Vendored Apache-2.0 protocol artifacts at commit `79ced4c` are validated through one schema/semantic boundary. Pure bounded readers, discovery negotiation, and query traversal feed immutable integration models; config flow and diagnostics consume only safe classified interfaces. `create_client()` remains a fail-closed pending adapter.

**Tech Stack:** Python 3.14.2+, Home Assistant 2026.8.3, JSON Schema draft 2020-12 through project range `jsonschema[format-nongpl]>=4.26,<5` and Home Assistant manifest/lock pin `4.26.0`, pytest-homeassistant-custom-component, pytest-cov, Ruff.

**Spec:** `docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md`

## Global Constraints

- Begin on clean local `main` after the approved design/plan commits. The production-code baseline is `1a61d78`; before Task 1, `git diff --exit-code 1a61d78 -- custom_components tests pyproject.toml uv.lock hacs.json README.md docs/architecture.md docs/protocol-readiness.md` must show no implementation drift. Use protocol commit `79ced4c7fdc79520ad31d72a0280bf5f3f19f407`.
- Keep domain `teslatlas_hub`; do not create a second integration.
- Keep production pairing, synchronized transport, Zeroconf, HACS publication, commands, services, and hosted automation disabled.
- Never import Hub source, read Hub storage, request Tesla credentials, poll Tesla, or touch the proprietary app.
- Never expose endpoints, ports, Hub/vehicle IDs or names, locations, precise vehicle timestamps, replay IDs, request IDs, headers, response bodies, or raw exception text through diagnostics/errors/logs.
- Preserve `/Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant` unchanged, including its untracked plan.
- Execute Tasks 1-6 in order. Each behavior change starts with the listed failing test and ends with its focused gate and commit.

---

### Suite preflight: Capture the separate checkout before any implementation

**Files:** None. Read-only evidence only.

**Interfaces:** Produces the immutable execution-transcript baseline used by the readiness plan: separate repository HEAD, full porcelain status, and sorted SHA-256 for every untracked path.

- [ ] **Step 1: Record the original separate-repository baseline**

Run before Task 1 or any implementation edit:

```bash
git -C /Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant rev-parse HEAD
git -C /Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant status --porcelain=v1 -uall
python3 - <<'PY'
import hashlib
import subprocess
from pathlib import Path

root = Path("/Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant")
raw_paths = subprocess.check_output(
    ["git", "-C", str(root), "ls-files", "--others", "--exclude-standard", "-z"]
)
for raw_path in sorted(path for path in raw_paths.split(b"\0") if path):
    relative = raw_path.decode("utf-8")
    with (root / relative).open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    print(f"{digest}  {relative}")
PY
```

Copy all three outputs verbatim into the execution transcript under `SEPARATE_REPOSITORY_ORIGINAL_BASELINE`. Do not continue unless the status is understood and every untracked path, including `docs/plans/2026-08-30-local-push-hacs.md`, has a digest. Every later plan treats these original bytes as immutable.

### Task 1: Vendor and validate protocol `1.2.0` artifacts

**Files:**
- Create: `custom_components/teslatlas_hub/protocol/__init__.py`
- Create: `custom_components/teslatlas_hub/errors.py`
- Create: `custom_components/teslatlas_hub/protocol/schema.py`
- Create: `custom_components/teslatlas_hub/protocol/semantics.py`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/PROVENANCE.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/LICENSE`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/common.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/discovery.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/resources.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/data-quality.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/error.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/event.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/observation.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/command.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/metadata.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/sse-contract.schema.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/teslatlas-v1.sse.json`
- Create: `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/profile.json`
- Create: `tests/fixtures/protocol-1.2.0/discovery.json`
- Create: `tests/fixtures/protocol-1.2.0/vehicles-page.json`
- Create: `tests/fixtures/protocol-1.2.0/current-state.json`
- Create: `tests/fixtures/protocol-1.2.0/event-envelope.json`
- Create: `tests/fixtures/protocol-1.2.0/data-quality.json`
- Create: `tests/fixtures/protocol-1.2.0/error.json`
- Create: `tests/fixtures/protocol-1.2.0/observation.json`
- Create: `tests/fixtures/protocol-1.2.0/drives-page.json`
- Create: `tests/fixtures/protocol-1.2.0/charges-page.json`
- Create: `tests/fixtures/protocol-1.2.0/states-page.json`
- Create: `tests/fixtures/protocol-1.2.0/updates-page.json`
- Create: `tests/fixtures/protocol-1.2.0/command-job.json`
- Create: `tests/fixtures/protocol-1.2.0/metadata-record.json`
- Create: `tests/fixtures/protocol-1.2.0/metadata-tombstone.json`
- Create: `tests/fixtures/protocol-1.2.0/invalid/data-quality-complete-with-gap.json`
- Create: `tests/fixtures/protocol-1.2.0/invalid/error-success-status.json`
- Create: `tests/fixtures/protocol-1.2.0/invalid/event-unknown-type.json`
- Create: `tests/fixtures/protocol-1.2.0/invalid/metadata-zero-revision.json`
- Create: `tests/fixtures/protocol-1.2.0/invalid/observation-missing-hash.json`
- Create: `tests/fixtures/protocol-1.2.0/invalid/observation-non-utc.json`
- Create: `tests/fixtures/protocol-1.2.0/invalid/resource-short-cursor.json`
- Create: `tests/fixtures/protocol-1.2.0/invalid/command-unknown-state.json`
- Create: `tests/test_protocol_schema.py`
- Modify: `pyproject.toml`
- Modify: `uv.lock`
- Modify: `custom_components/teslatlas_hub/manifest.json`

**Interfaces:**
- Produces: `PROTOCOL_VERSION = "1.2.0"` and immutable artifact provenance for commit `79ced4c7fdc79520ad31d72a0280bf5f3f19f407`.
- Produces: initial safe `ProtocolConformanceError(code="protocol_conformance")`; Task 2 expands the same module into the complete client taxonomy.
- Produces: `validate_document(schema_id: str, value: object) -> dict[str, object]`.
- Produces: `validate_event_semantics(event: Mapping[str, object]) -> None`, `validate_metadata_semantics(value: Mapping[str, object]) -> None`, and `validate_discovery_semantics(value: Mapping[str, object]) -> None`.

- [ ] **Step 1: Write the complete failing provenance/schema test module**

Create `tests/test_protocol_schema.py` with this complete body (the existing
`protocol_fixture` fixture returns a fresh `dict` loaded from
`tests/fixtures/protocol-1.2.0`):

```python
from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path

import pytest

from custom_components.teslatlas_hub.errors import ProtocolConformanceError
from custom_components.teslatlas_hub.protocol import PROTOCOL_VERSION
from custom_components.teslatlas_hub.protocol.schema import validate_document
from custom_components.teslatlas_hub.protocol.semantics import (
    validate_discovery_semantics,
    validate_event_semantics,
    validate_metadata_semantics,
)

ROOT = Path(__file__).parents[1]
ARTIFACT_ROOT = (
    ROOT / "custom_components/teslatlas_hub/protocol/artifacts"
)
FIXTURE_ROOT = ROOT / "tests/fixtures/protocol-1.2.0"
COMMIT = "79ced4c7fdc79520ad31d72a0280bf5f3f19f407"

SCHEMAS = {
    "discovery.json": "urn:teslatlas:protocol:schema:discovery:1.2.0",
    "vehicles-page.json": "urn:teslatlas:protocol:schema:resources:1.2.0",
    "current-state.json": "urn:teslatlas:protocol:schema:resources:1.2.0",
    "event-envelope.json": "urn:teslatlas:protocol:schema:event:1.2.0",
    "data-quality.json": "urn:teslatlas:protocol:schema:data-quality:1.2.0",
    "error.json": "urn:teslatlas:protocol:schema:error:1.2.0",
    "observation.json": "urn:teslatlas:protocol:schema:observation:1.2.0",
    "drives-page.json": "urn:teslatlas:protocol:schema:resources:1.2.0",
    "charges-page.json": "urn:teslatlas:protocol:schema:resources:1.2.0",
    "states-page.json": "urn:teslatlas:protocol:schema:resources:1.2.0",
    "updates-page.json": "urn:teslatlas:protocol:schema:resources:1.2.0",
    "command-job.json": "urn:teslatlas:protocol:schema:command:1.2.0",
    "metadata-record.json": "urn:teslatlas:protocol:schema:metadata:1.2.0",
    "metadata-tombstone.json": "urn:teslatlas:protocol:schema:metadata:1.2.0",
}
INVALID_SCHEMAS = {
    "data-quality-complete-with-gap.json": "urn:teslatlas:protocol:schema:data-quality:1.2.0",
    "error-success-status.json": "urn:teslatlas:protocol:schema:error:1.2.0",
    "event-unknown-type.json": "urn:teslatlas:protocol:schema:event:1.2.0",
    "metadata-zero-revision.json": "urn:teslatlas:protocol:schema:metadata:1.2.0",
    "observation-missing-hash.json": "urn:teslatlas:protocol:schema:observation:1.2.0",
    "observation-non-utc.json": "urn:teslatlas:protocol:schema:observation:1.2.0",
    "resource-short-cursor.json": "urn:teslatlas:protocol:schema:resources:1.2.0",
    "command-unknown-state.json": "urn:teslatlas:protocol:schema:command:1.2.0",
}
ARTIFACT_NAMES = {
    "LICENSE",
    "1.2.0/common.schema.json",
    "1.2.0/discovery.schema.json",
    "1.2.0/resources.schema.json",
    "1.2.0/data-quality.schema.json",
    "1.2.0/error.schema.json",
    "1.2.0/event.schema.json",
    "1.2.0/observation.schema.json",
    "1.2.0/command.schema.json",
    "1.2.0/metadata.schema.json",
    "1.2.0/sse-contract.schema.json",
    "1.2.0/teslatlas-v1.sse.json",
    "1.2.0/profile.json",
}


def _semantic_validate(name: str, value: dict[str, object]) -> None:
    if name == "discovery.json":
        validate_discovery_semantics(value)
    elif name == "event-envelope.json":
        validate_event_semantics(value)
    elif name.startswith("metadata-"):
        validate_metadata_semantics(value)


def test_copied_file_digest_matches_provenance() -> None:
    provenance = json.loads(
        (ARTIFACT_ROOT / "PROVENANCE.json").read_text(encoding="utf-8")
    )
    expected = {
        *(
            f"custom_components/teslatlas_hub/protocol/artifacts/{name}"
            for name in ARTIFACT_NAMES
        ),
        *(f"tests/fixtures/protocol-1.2.0/{name}" for name in SCHEMAS),
        *(
            f"tests/fixtures/protocol-1.2.0/invalid/{name}"
            for name in INVALID_SCHEMAS
        ),
    }
    assert len(expected) == 35
    assert set(provenance["files"]) == expected
    assert provenance["source_commit"] == COMMIT
    assert provenance["license"] == "Apache-2.0"
    for relative, record in provenance["files"].items():
        destination = ROOT / relative
        assert set(record) == {"source_path", "sha256"}
        assert hashlib.sha256(destination.read_bytes()).hexdigest() == record["sha256"]


@pytest.mark.parametrize(("name", "schema_id"), SCHEMAS.items())
def test_frozen_positive_fixture_validates(
    protocol_fixture, name: str, schema_id: str
) -> None:
    value = protocol_fixture(name)
    assert validate_document(schema_id, value) is value
    _semantic_validate(name, value)


@pytest.mark.parametrize(("name", "schema_id"), INVALID_SCHEMAS.items())
def test_frozen_negative_fixture_is_rejected(
    protocol_fixture, name: str, schema_id: str
) -> None:
    with pytest.raises(ProtocolConformanceError) as caught:
        validate_document(schema_id, protocol_fixture(f"invalid/{name}"))
    assert caught.value.code == "protocol_conformance"


def test_frozen_current_state_validates(protocol_fixture) -> None:
    assert PROTOCOL_VERSION == "1.2.0"
    value = protocol_fixture("current-state.json")
    assert validate_document(SCHEMAS["current-state.json"], value)["resource_type"] == (
        "current_state"
    )


def test_data_quality_event_wrapper_is_deterministic(protocol_fixture) -> None:
    data = protocol_fixture("data-quality.json")
    base = protocol_fixture("event-envelope.json")
    event = {
        **base,
        "event_id": "event_demo_data_quality_0001",
        "event_type": "data_quality.changed",
        "occurred_at": "2026-08-30T12:00:00.000Z",
        "vehicle_id": None,
        "resource_id": data["subject_id"],
        "revision": 1,
        "data": data,
    }
    first = validate_document(SCHEMAS["event-envelope.json"], copy.deepcopy(event))
    second = validate_document(SCHEMAS["event-envelope.json"], copy.deepcopy(event))
    validate_event_semantics(first)
    validate_event_semantics(second)
    assert first == second


def test_optional_current_temperature_may_be_omitted(protocol_fixture) -> None:
    value = protocol_fixture("current-state.json")
    del value["inside_temperature_c"]
    del value["outside_temperature_c"]
    assert validate_document(SCHEMAS["current-state.json"], value) is value


@pytest.mark.parametrize(
    "field",
    [
        "battery_level_percent",
        "range_km",
        "odometer_km",
        "locked",
        "climate_on",
        "charging_state",
    ],
)
def test_every_other_current_metric_is_required(protocol_fixture, field: str) -> None:
    value = protocol_fixture("current-state.json")
    del value[field]
    with pytest.raises(ProtocolConformanceError):
        validate_document(SCHEMAS["current-state.json"], value)
```

- [ ] **Step 2: Run the schema module and verify the import failure**

Run: `uv run pytest tests/test_protocol_schema.py -q`

Expected: FAIL during collection with `ModuleNotFoundError` for
`custom_components.teslatlas_hub.protocol`.

- [ ] **Step 3: Add and lock the validator dependency**

Run: `uv add 'jsonschema[format-nongpl]>=4.26,<5'`

Then replace the generated Home Assistant requirement with the exact runtime
pin in `custom_components/teslatlas_hub/manifest.json`:

```json
"requirements": ["jsonschema[format-nongpl]==4.26.0"]
```

Run: `uv lock --check && rg -n 'jsonschema.*4\.26\.0' uv.lock custom_components/teslatlas_hub/manifest.json`

Expected: lock check passes; the lock and manifest both contain `4.26.0`.

- [ ] **Step 4: Copy the 13 immutable protocol artifacts**

First require the exact source revision:

```bash
test "$(git -C /Users/bolyki/dev/source/teslatlas-protocol rev-parse HEAD)" = "79ced4c7fdc79520ad31d72a0280bf5f3f19f407"
mkdir -p custom_components/teslatlas_hub/protocol/artifacts/1.2.0
```

Copy these exact protocol paths at commit `79ced4c` to the listed integration paths, preserving bytes:

| Protocol source | Integration destination |
|---|---|
| `LICENSE` | `custom_components/teslatlas_hub/protocol/artifacts/LICENSE` |
| `schemas/common.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/common.schema.json` |
| `schemas/discovery.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/discovery.schema.json` |
| `schemas/resources.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/resources.schema.json` |
| `schemas/data-quality.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/data-quality.schema.json` |
| `schemas/error.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/error.schema.json` |
| `schemas/event.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/event.schema.json` |
| `schemas/observation.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/observation.schema.json` |
| `schemas/command.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/command.schema.json` |
| `schemas/metadata.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/metadata.schema.json` |
| `schemas/sse-contract.schema.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/sse-contract.schema.json` |
| `events/teslatlas-v1.sse.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/teslatlas-v1.sse.json` |
| `compatibility/1.2.0/profile.json` | `custom_components/teslatlas_hub/protocol/artifacts/1.2.0/profile.json` |

Copy these exact positive fixtures without editing:

`examples/discovery.json`, `examples/vehicles-page.json`, `examples/current-state.json`, `examples/event-envelope.json`, `examples/data-quality.json`, `examples/error.json`, `examples/observation.json`, `examples/drives-page.json`, `examples/charges-page.json`, `examples/states-page.json`, `examples/updates-page.json`, `examples/command-job.json`, `examples/metadata-record.json`, and `examples/metadata-tombstone.json` go to the same basename under `tests/fixtures/protocol-1.2.0/`.

Copy these exact negative fixtures without editing:

`examples/invalid/data-quality-complete-with-gap.json`, `examples/invalid/error-success-status.json`, `examples/invalid/event-unknown-type.json`, `examples/invalid/metadata-zero-revision.json`, `examples/invalid/observation-missing-hash.json`, `examples/invalid/observation-non-utc.json`, `examples/invalid/resource-short-cursor.json`, and `examples/invalid/command-unknown-state.json` go to the same basename under `tests/fixtures/protocol-1.2.0/invalid/`.

Execute the copy with these exact source/destination pairs; do not modify the bytes:

```bash
cp /Users/bolyki/dev/source/teslatlas-protocol/LICENSE custom_components/teslatlas_hub/protocol/artifacts/LICENSE
for name in common discovery resources data-quality error event observation command metadata sse-contract; do
  cp "/Users/bolyki/dev/source/teslatlas-protocol/schemas/$name.schema.json" "custom_components/teslatlas_hub/protocol/artifacts/1.2.0/$name.schema.json"
done
cp /Users/bolyki/dev/source/teslatlas-protocol/events/teslatlas-v1.sse.json custom_components/teslatlas_hub/protocol/artifacts/1.2.0/teslatlas-v1.sse.json
cp /Users/bolyki/dev/source/teslatlas-protocol/compatibility/1.2.0/profile.json custom_components/teslatlas_hub/protocol/artifacts/1.2.0/profile.json
```

- [ ] **Step 5: Copy the 14 positive protocol fixtures**

```bash
mkdir -p tests/fixtures/protocol-1.2.0
for name in discovery vehicles-page current-state event-envelope data-quality error observation drives-page charges-page states-page updates-page command-job metadata-record metadata-tombstone; do
  cp "/Users/bolyki/dev/source/teslatlas-protocol/examples/$name.json" "tests/fixtures/protocol-1.2.0/$name.json"
done
```

- [ ] **Step 6: Copy the eight negative protocol fixtures**

```bash
mkdir -p tests/fixtures/protocol-1.2.0/invalid
for name in data-quality-complete-with-gap error-success-status event-unknown-type metadata-zero-revision observation-missing-hash observation-non-utc resource-short-cursor command-unknown-state; do
  cp "/Users/bolyki/dev/source/teslatlas-protocol/examples/invalid/$name.json" "tests/fixtures/protocol-1.2.0/invalid/$name.json"
done
```

- [ ] **Step 7: Generate the exact bidirectional provenance manifest**

Run this mechanical generator once; it records all 35 copied files and rejects
any source/destination byte difference:

```bash
python3 - <<'PY'
import hashlib
import json
from pathlib import Path

source_root = Path("/Users/bolyki/dev/source/teslatlas-protocol")
destination_root = Path.cwd()
commit = "79ced4c7fdc79520ad31d72a0280bf5f3f19f407"
pairs = [("LICENSE", "custom_components/teslatlas_hub/protocol/artifacts/LICENSE")]
pairs += [
    (
        f"schemas/{name}.schema.json",
        f"custom_components/teslatlas_hub/protocol/artifacts/1.2.0/{name}.schema.json",
    )
    for name in (
        "common", "discovery", "resources", "data-quality", "error", "event",
        "observation", "command", "metadata", "sse-contract",
    )
]
pairs += [
    (
        "events/teslatlas-v1.sse.json",
        "custom_components/teslatlas_hub/protocol/artifacts/1.2.0/teslatlas-v1.sse.json",
    ),
    (
        "compatibility/1.2.0/profile.json",
        "custom_components/teslatlas_hub/protocol/artifacts/1.2.0/profile.json",
    ),
]
positive = (
    "discovery", "vehicles-page", "current-state", "event-envelope",
    "data-quality", "error", "observation", "drives-page", "charges-page",
    "states-page", "updates-page", "command-job", "metadata-record",
    "metadata-tombstone",
)
negative = (
    "data-quality-complete-with-gap", "error-success-status", "event-unknown-type",
    "metadata-zero-revision", "observation-missing-hash", "observation-non-utc",
    "resource-short-cursor", "command-unknown-state",
)
pairs += [
    (f"examples/{name}.json", f"tests/fixtures/protocol-1.2.0/{name}.json")
    for name in positive
]
pairs += [
    (
        f"examples/invalid/{name}.json",
        f"tests/fixtures/protocol-1.2.0/invalid/{name}.json",
    )
    for name in negative
]
records = {}
for source_name, logical_destination in pairs:
    source = source_root / source_name
    destination = destination_root / logical_destination
    assert source.read_bytes() == destination.read_bytes()
    records[logical_destination] = {
        "source_path": source_name,
        "sha256": hashlib.sha256(destination.read_bytes()).hexdigest(),
    }
assert len(records) == 35
output = {
    "license": "Apache-2.0",
    "source_commit": commit,
    "files": dict(sorted(records.items())),
}
path = destination_root / "custom_components/teslatlas_hub/protocol/artifacts/PROVENANCE.json"
path.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
```

- [ ] **Step 8: Implement the schema registry and safe base exception**

```python
# custom_components/teslatlas_hub/protocol/__init__.py
PROTOCOL_VERSION = "1.2.0"


# custom_components/teslatlas_hub/errors.py
class ProtocolConformanceError(Exception):
    """A protocol document failed an allow-listed contract check."""

    code = "protocol_conformance"

    def __str__(self) -> str:
        return self.code

    def __repr__(self) -> str:
        return f"{type(self).__name__}(code={self.code!r})"


# custom_components/teslatlas_hub/protocol/schema.py
ARTIFACT_ROOT = Path(__file__).parent / "artifacts" / PROTOCOL_VERSION


@cache
def _registry() -> Registry:
    registry = Registry()
    for path in sorted(ARTIFACT_ROOT.glob("*.schema.json")):
        document = json.loads(path.read_text(encoding="utf-8"))
        registry = registry.with_resource(
            document["$id"], Resource.from_contents(document)
        )
    return registry


def validate_document(schema_id: str, value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ProtocolConformanceError()
    validator = Draft202012Validator(
        _registry().get_or_retrieve(schema_id).value.contents,
        registry=_registry(),
        format_checker=FormatChecker(),
    )
    if next(validator.iter_errors(value), None) is not None:
        raise ProtocolConformanceError()
    return value
```

- [ ] **Step 9: Run the structural tests and verify the remaining semantic failures**

Run: `uv run pytest tests/test_protocol_schema.py -q`

Expected: FAIL importing `protocol.semantics`; the artifact, dependency, and
schema-registry portions collected before that failure are now present.

- [ ] **Step 10: Port the exact language-neutral semantic checks**

Source is `/Users/bolyki/dev/source/teslatlas-protocol/conformance/runner.py`
at commit `79ced4c7fdc79520ad31d72a0280bf5f3f19f407`: discovery lines 76-140,
metadata lines 143-224, and event lines 226-264. Implement the same decisions in
`protocol/semantics.py`; only error reporting changes from a list of unsafe
strings to the fixed exception:

```python
from __future__ import annotations

from collections.abc import Mapping, Sequence
from jsonschema import Draft202012Validator
from jsonschema.exceptions import SchemaError

from ..errors import ProtocolConformanceError

EVENT_RESOURCE_FIELDS = {
    "observation.admitted": "observation_id",
    "vehicle.current.changed": "vehicle_id",
    "drive.started": "drive_id",
    "drive.updated": "drive_id",
    "drive.ended": "drive_id",
    "charge.started": "charge_id",
    "charge.updated": "charge_id",
    "charge.ended": "charge_id",
    "state.changed": "state_id",
    "software_update.changed": "update_id",
    "data_quality.changed": "subject_id",
    "command.changed": "command_id",
    "metadata.changed": "metadata_id",
}


def _fail() -> None:
    raise ProtocolConformanceError() from None


def _semver(value: object) -> tuple[int, int, int]:
    if not isinstance(value, str):
        _fail()
    parts = value.split(".")
    if len(parts) != 3 or any(not part.isdecimal() for part in parts):
        _fail()
    return (int(parts[0]), int(parts[1]), int(parts[2]))


def validate_discovery_semantics(value: Mapping[str, object]) -> None:
    try:
        protocol = value["protocol"]
        capabilities = value["capabilities"]
        if not isinstance(protocol, Mapping) or not isinstance(capabilities, Sequence):
            _fail()
        supported = protocol["supported_versions"]
        if not isinstance(supported, list) or not supported:
            _fail()
        parsed = [_semver(version) for version in supported]
        current = _semver(protocol["current_version"])
        minimum = _semver(protocol["minimum_client_version"])
        if parsed != sorted(parsed) or current != parsed[-1] or minimum != parsed[0]:
            _fail()
        if any(version[0] != current[0] for version in parsed):
            _fail()
        identifiers: list[str] = []
        for capability in capabilities:
            if not isinstance(capability, Mapping):
                _fail()
            identifier = capability.get("id")
            commands = capability.get("commands", [])
            if not isinstance(identifier, str) or not isinstance(commands, list):
                _fail()
            identifiers.append(identifier)
            names: list[str] = []
            for command in commands:
                if not isinstance(command, Mapping) or not isinstance(command.get("name"), str):
                    _fail()
                names.append(command["name"])
                for field in ("parameters_schema", "expected_state_schema"):
                    schema = command.get(field)
                    if isinstance(schema, Mapping):
                        try:
                            Draft202012Validator.check_schema(schema)
                        except SchemaError:
                            _fail()
            if len(names) != len(set(names)):
                _fail()
            deprecation = capability.get("deprecation")
            if isinstance(deprecation, Mapping):
                deprecated_at = deprecation.get("deprecated_at")
                sunset_at = deprecation.get("sunset_at")
                if isinstance(deprecated_at, str) and isinstance(sunset_at, str):
                    if sunset_at < deprecated_at:
                        _fail()
        if len(identifiers) != len(set(identifiers)):
            _fail()
    except (KeyError, TypeError, ValueError):
        _fail()


def _validate_history(value: object) -> list[Mapping[str, object]]:
    if not isinstance(value, list) or not value:
        _fail()
    history: list[Mapping[str, object]] = []
    previous_hash: object = None
    for index, entry in enumerate(value):
        if not isinstance(entry, Mapping):
            _fail()
        action = "created" if index == 0 else "updated"
        if (
            entry.get("revision") != index + 1
            or entry.get("action") != action
            or entry.get("previous_hash") != previous_hash
        ):
            _fail()
        previous_hash = entry.get("new_hash")
        history.append(entry)
    return history


def validate_metadata_semantics(value: Mapping[str, object]) -> None:
    audit = value.get("audit")
    if isinstance(audit, list):
        history = _validate_history(audit)
        if (
            value.get("revision") != history[-1].get("revision")
            or value.get("updated_at") != history[-1].get("at")
            or value.get("updated_by") != history[-1].get("actor_id")
            or value.get("created_at") != history[0].get("at")
            or value.get("created_by") != history[0].get("actor_id")
        ):
            _fail()
        return
    if isinstance(audit, Mapping):
        history = _validate_history(audit.get("history"))
        deletion = audit.get("deletion")
        if not isinstance(deletion, Mapping):
            _fail()
        terminal = history[-1].get("revision")
        if not isinstance(terminal, int) or deletion.get("revision") != terminal + 1:
            _fail()


def validate_event_semantics(event: Mapping[str, object]) -> None:
    data = event.get("data")
    if not isinstance(data, Mapping):
        _fail()
    data_revision = data.get("revision")
    audit = data.get("audit")
    if data_revision is None and isinstance(audit, Mapping):
        deletion = audit.get("deletion")
        if isinstance(deletion, Mapping):
            data_revision = deletion.get("revision")
    if data_revision is not None and event.get("revision") != data_revision:
        _fail()
    if "vehicle_id" in data and event.get("vehicle_id") != data["vehicle_id"]:
        _fail()
    identifier_field = EVENT_RESOURCE_FIELDS.get(event.get("event_type"))
    if (
        identifier_field is not None
        and identifier_field in data
        and event.get("resource_id") != data[identifier_field]
    ):
        _fail()
```

- [ ] **Step 11: Run focused and dependency gates**

Run: `uv lock --check`

Run: `uv run pytest tests/test_protocol_schema.py -q`

Expected: PASS with every positive/negative/provenance case green.

- [ ] **Step 12: Commit Task 1**

```bash
git add pyproject.toml uv.lock custom_components/teslatlas_hub/manifest.json custom_components/teslatlas_hub/errors.py custom_components/teslatlas_hub/protocol tests/fixtures/protocol-1.2.0 tests/test_protocol_schema.py
git commit -m "feat: add frozen protocol contract validation"
```

### Task 2: Safe error taxonomy and bounded JSON bodies

**Files:**
- Modify: `custom_components/teslatlas_hub/errors.py`
- Create: `custom_components/teslatlas_hub/protocol/body.py`
- Create: `tests/test_protocol_body.py`
- Modify: `custom_components/teslatlas_hub/client.py`
- Modify: `tests/test_client.py`

**Interfaces:**
- Produces: `ClientErrorCode(StrEnum)` and safe `HubClientError(code, retryable=False)` subclasses.
- Produces: allow-listed `ProblemCode(StrEnum)`, `ProblemOperation(StrEnum)`, and immutable `SafeProblem(status, code, retryable, retry_after_seconds)`.
- Produces: `BodyKind(StrEnum)` with byte caps `DISCOVERY_OR_PAGE=2 MiB`, `CURRENT=256 KiB`, and `PROBLEM=64 KiB`.
- Produces: `HeaderFields = tuple[tuple[str, str], ...]`, preserving duplicate wire headers, and immutable `JsonResponse(status: int, headers: HeaderFields, chunks: AsyncIterable[bytes])`.
- Produces: `single_header(headers: HeaderFields, name: str, *, required: bool) -> str | None`, case-insensitive and rejecting duplicate values before body parsing.
- Produces: `async_read_json(chunks: AsyncIterable[bytes], *, kind: BodyKind, content_type: str, content_length: str | None) -> dict[str, object]`.
- Produces: `strict_load_json_object(text: str) -> dict[str, object]` for already UTF-8-decoded SSE data.
- Produces: `async_parse_problem_response(response: JsonResponse, *, operation: ProblemOperation) -> HubClientError`.

- [ ] **Step 1: Write complete bounded-body tests**

Create `tests/test_protocol_body.py` with the following imports, helpers, and
body tests. These helpers are the only constructors used by the problem tests
in Step 3.

```python
from __future__ import annotations

import json
from collections.abc import AsyncIterator

import pytest

from custom_components.teslatlas_hub.errors import (
    ClientErrorCode,
    ConcurrencyLimitError,
    CursorExpiredError,
    CursorInvalidError,
    CursorQueryMismatchError,
    CursorScopeChangedError,
    EventIdInvalidError,
    ProtocolBodyError,
    ProtocolConformanceError,
    RateLimitedError,
    ReplayExpiredError,
)
from custom_components.teslatlas_hub.protocol.body import (
    BODY_LIMITS,
    BodyKind,
    JsonResponse,
    ProblemOperation,
    async_parse_problem_response,
    async_read_json,
    single_header,
    strict_load_json_object,
)

MIB = 1024 * 1024


async def chunks_of(value: bytes, size: int) -> AsyncIterator[bytes]:
    for offset in range(0, len(value), size):
        yield value[offset : offset + size]


def sized_object(size: int) -> bytes:
    prefix = b'{"pad":"'
    suffix = b'"}'
    assert size >= len(prefix) + len(suffix)
    return prefix + b"x" * (size - len(prefix) - len(suffix)) + suffix


def problem_response(
    *,
    status: int,
    code: str,
    retryable: bool,
    retry_header: tuple[str, ...] = (),
    body_delay: int | None = None,
    body_status: int | None = None,
    extensions: dict[str, object] | None = None,
) -> JsonResponse:
    body: dict[str, object] = {
        "type": f"urn:teslatlas:problem:{code.replace('_', '-')}",
        "title": "Safe title",
        "status": status if body_status is None else body_status,
        "detail": "RAW_DETAIL_SENTINEL",
        "instance": "/requests/request_raw_sentinel",
        "code": code,
        "request_id": "request_raw_sentinel",
        "retryable": retryable,
    }
    if body_delay is not None:
        body["retry_after_seconds"] = body_delay
    if extensions:
        body.update(extensions)
    payload = json.dumps(body, separators=(",", ":")).encode()
    headers = (
        ("Content-Type", "application/problem+json"),
        ("Content-Length", str(len(payload))),
        *(("Retry-After", item) for item in retry_header),
        ("X-Raw-Sentinel", "RAW_HEADER_SENTINEL"),
    )
    return JsonResponse(status=status, headers=headers, chunks=chunks_of(payload, 11))


@pytest.mark.parametrize("kind", list(BodyKind))
async def test_body_accepts_exact_limit_and_rejects_one_byte_over(kind: BodyKind) -> None:
    media_type = (
        "application/problem+json" if kind is BodyKind.PROBLEM else "application/json"
    )
    limit = BODY_LIMITS[kind]
    exact = await async_read_json(
        chunks_of(sized_object(limit), 8191),
        kind=kind,
        content_type=media_type,
        content_length=str(limit),
    )
    assert len(exact["pad"]) > 0
    with pytest.raises(ProtocolBodyError):
        await async_read_json(
            chunks_of(sized_object(limit + 1), 8191),
            kind=kind,
            content_type=media_type,
            content_length=None,
        )


@pytest.mark.parametrize(
    ("declared", "accepted"),
    [
        ("10", True),
        (None, True),
        (str(BODY_LIMITS[BodyKind.CURRENT] + 1), False),
        ("-1", False),
        ("+10", False),
        ("10.0", False),
        ("ten", False),
    ],
)
async def test_declared_content_length_is_strict(
    declared: str | None, accepted: bool
) -> None:
    operation = async_read_json(
        chunks_of(b'{"pad":""}', 2),
        kind=BodyKind.CURRENT,
        content_type="application/json",
        content_length=declared,
    )
    if accepted:
        assert await operation == {"pad": ""}
    else:
        with pytest.raises(ProtocolBodyError):
            await operation


@pytest.mark.parametrize(
    "name",
    ["Content-Type", "Content-Length", "Retry-After", "Teslatlas-Protocol-Version"],
)
def test_single_header_is_case_insensitive_and_rejects_duplicates(name: str) -> None:
    assert single_header(((name.swapcase(), "one"),), name, required=True) == "one"
    assert single_header((), name, required=False) is None
    with pytest.raises(ProtocolConformanceError):
        single_header(((name, "one"), (name.lower(), "two")), name, required=False)
    with pytest.raises(ProtocolConformanceError):
        single_header((), name, required=True)


async def test_actual_chunks_override_false_small_declared_length() -> None:
    with pytest.raises(ProtocolBodyError):
        await async_read_json(
            chunks_of(sized_object(BODY_LIMITS[BodyKind.CURRENT] + 1), 23),
            kind=BodyKind.CURRENT,
            content_type="application/json",
            content_length="1",
        )


@pytest.mark.parametrize(
    ("kind", "media_type", "accepted"),
    [
        (BodyKind.DISCOVERY_OR_PAGE, "application/json", True),
        (BodyKind.CURRENT, "application/json", True),
        (BodyKind.PROBLEM, "application/problem+json", True),
        (BodyKind.DISCOVERY_OR_PAGE, "application/problem+json", False),
        (BodyKind.CURRENT, "application/problem+json", False),
        (BodyKind.PROBLEM, "application/json", False),
        (BodyKind.CURRENT, "text/json", False),
    ],
)
async def test_body_media_type_is_operation_specific(
    kind: BodyKind, media_type: str, accepted: bool
) -> None:
    operation = async_read_json(
        chunks_of(b"{}", 1),
        kind=kind,
        content_type=media_type,
        content_length="2",
    )
    if accepted:
        assert await operation == {}
    else:
        with pytest.raises(ProtocolBodyError):
            await operation


async def test_split_utf8_decodes_after_complete_bounded_body() -> None:
    payload = '{"value":"€"}'.encode()
    for split in range(1, len(payload)):
        async def split_chunks() -> AsyncIterator[bytes]:
            yield payload[:split]
            yield payload[split:]

        assert await async_read_json(
            split_chunks(),
            kind=BodyKind.CURRENT,
            content_type="application/json",
            content_length=str(len(payload)),
        ) == {"value": "€"}


async def test_malformed_utf8_is_safe_body_error() -> None:
    with pytest.raises(ProtocolBodyError):
        await async_read_json(
            chunks_of(b'{"value":"\xff"}', 2),
            kind=BodyKind.CURRENT,
            content_type="application/json",
            content_length=None,
        )


@pytest.mark.parametrize("text", ["[]", "1", '"x"', "null", "{", '{"n":NaN}',
                                             '{"n":Infinity}', '{"n":-Infinity}'])
def test_json_root_and_numbers_are_strict(text: str) -> None:
    with pytest.raises(ProtocolConformanceError):
        strict_load_json_object(text)


async def test_body_failures_log_only_safe_classification(caplog) -> None:
    sentinel = "RAW_BODY_SENTINEL"
    with pytest.raises(ProtocolBodyError) as caught:
        await async_read_json(
            chunks_of(sized_object(2 * MIB + 1) + sentinel.encode(), 17),
            kind=BodyKind.DISCOVERY_OR_PAGE,
            content_type="application/json",
            content_length=None,
        )
    assert caught.value.code is ClientErrorCode.PROTOCOL_BODY
    assert sentinel not in str(caught.value)
    assert sentinel not in repr(caught.value)
    assert sentinel not in caplog.text
```

- [ ] **Step 2: Run only the body matrix and verify red**

Run: `uv run pytest tests/test_protocol_body.py -q`

Expected: FAIL during collection because `protocol/body.py` and the expanded
error taxonomy do not exist.

- [ ] **Step 3: Append complete problem-response tests**

Append these tests to `tests/test_protocol_body.py`:

```python
@pytest.mark.parametrize(
    ("status", "code", "error_type"),
    [
        (400, "invalid_cursor", CursorInvalidError),
        (409, "cursor_query_mismatch", CursorQueryMismatchError),
        (403, "cursor_scope_changed", CursorScopeChangedError),
        (410, "cursor_expired", CursorExpiredError),
    ],
)
async def test_query_problem_pairs_are_exact(status, code, error_type) -> None:
    error = await async_parse_problem_response(
        problem_response(status=status, code=code, retryable=False),
        operation=ProblemOperation.QUERY,
    )
    assert type(error) is error_type


@pytest.mark.parametrize(
    ("status", "code", "error_type"),
    [(400, "event_id_invalid", EventIdInvalidError),
     (410, "event_replay_expired", ReplayExpiredError)],
)
async def test_event_problem_pairs_are_exact(status, code, error_type) -> None:
    error = await async_parse_problem_response(
        problem_response(status=status, code=code, retryable=False),
        operation=ProblemOperation.EVENT_STREAM,
    )
    assert type(error) is error_type


@pytest.mark.parametrize(
    ("code", "delay", "error_type"),
    [("rate_limited", 0, RateLimitedError),
     ("rate_limited", 86400, RateLimitedError),
     ("concurrency_limit", 30, ConcurrencyLimitError)],
)
async def test_rate_limit_problem_is_exact(code, delay, error_type) -> None:
    error = await async_parse_problem_response(
        problem_response(
            status=429,
            code=code,
            retryable=True,
            retry_header=(str(delay),),
            body_delay=delay,
        ),
        operation=ProblemOperation.QUERY,
    )
    assert type(error) is error_type
    assert error.retryable is True
    assert error.retry_after_seconds == delay


@pytest.mark.parametrize(
    ("body_delay", "accepted"), [(None, True), (7, True), (8, False)]
)
async def test_optional_body_retry_delay_must_equal_header(
    body_delay: int | None, accepted: bool
) -> None:
    response = problem_response(
        status=429,
        code="rate_limited",
        retryable=True,
        retry_header=("7",),
        body_delay=body_delay,
    )
    if accepted:
        assert (await async_parse_problem_response(
            response, operation=ProblemOperation.QUERY
        )).retry_after_seconds == 7
    else:
        with pytest.raises(ProtocolConformanceError):
            await async_parse_problem_response(response, operation=ProblemOperation.QUERY)


@pytest.mark.parametrize(
    ("response", "operation"),
    [
        (problem_response(status=400, body_status=409, code="invalid_cursor", retryable=False),
         ProblemOperation.QUERY),
        (problem_response(status=400, code="unauthorized", retryable=False),
         ProblemOperation.QUERY),
        (problem_response(status=400, code="event_id_invalid", retryable=False),
         ProblemOperation.QUERY),
    ],
)
async def test_problem_status_code_operation_mismatch_is_conformance_error(
    response: JsonResponse, operation: ProblemOperation
) -> None:
    with pytest.raises(ProtocolConformanceError):
        await async_parse_problem_response(response, operation=operation)


@pytest.mark.parametrize(
    "response",
    [
        problem_response(status=429, code="rate_limited", retryable=False,
                         retry_header=("1",)),
        problem_response(status=429, code="rate_limited", retryable=True),
        problem_response(status=429, code="rate_limited", retryable=True,
                         retry_header=("x",)),
        problem_response(status=429, code="rate_limited", retryable=True,
                         retry_header=("1", "2")),
        problem_response(status=429, code="rate_limited", retryable=True,
                         retry_header=("-1",)),
        problem_response(status=429, code="rate_limited", retryable=True,
                         retry_header=("86401",)),
        problem_response(status=400, code="rate_limited", retryable=True,
                         retry_header=("1",)),
    ],
)
async def test_rate_limit_rejects_invalid_retry_contract(response: JsonResponse) -> None:
    with pytest.raises(ProtocolConformanceError):
        await async_parse_problem_response(response, operation=ProblemOperation.QUERY)


async def test_problem_unknown_extensions_are_ignored_after_schema_validation() -> None:
    error = await async_parse_problem_response(
        problem_response(
            status=400,
            code="invalid_cursor",
            retryable=False,
            extensions={"future_extension": {"private": "RAW_EXTENSION_SENTINEL"}},
        ),
        operation=ProblemOperation.QUERY,
    )
    assert type(error) is CursorInvalidError
    assert not hasattr(error, "future_extension")


async def test_problem_errors_drop_all_untrusted_fields(caplog) -> None:
    error = await async_parse_problem_response(
        problem_response(status=400, code="invalid_cursor", retryable=False),
        operation=ProblemOperation.QUERY,
    )
    rendered = f"{error!s} {error!r} {vars(error)} {caplog.text}"
    for sentinel in ("RAW_DETAIL_SENTINEL", "request_raw_sentinel", "RAW_HEADER_SENTINEL"):
        assert sentinel not in rendered
```

- [ ] **Step 4: Run exact problem tests and verify red**

Run: `uv run pytest tests/test_protocol_body.py::test_query_problem_pairs_are_exact tests/test_protocol_body.py::test_rate_limit_rejects_invalid_retry_contract -q`

Expected: FAIL during collection because the error types and parser are absent.

- [ ] **Step 5: Expand `errors.py` into the complete safe taxonomy**

```python
from __future__ import annotations

from enum import StrEnum


class ClientErrorCode(StrEnum):
    CONNECTION = "connection"
    AUTHENTICATION = "authentication"
    PAIRING = "pairing"
    DEPLOYMENT_NOT_READY = "deployment_not_ready"
    ENDPOINT_SECURITY = "endpoint_security"
    PROTOCOL_BODY = "protocol_body"
    PROTOCOL_CONFORMANCE = "protocol_conformance"
    CURSOR_EXPIRED = "cursor_expired"
    CURSOR_SCOPE_CHANGED = "cursor_scope_changed"
    CURSOR_INVALID = "cursor_invalid"
    CURSOR_QUERY_MISMATCH = "cursor_query_mismatch"
    QUERY_TRAVERSAL = "query_traversal"
    EVENT_ID_INVALID = "event_id_invalid"
    REPLAY_EXPIRED = "event_replay_expired"
    RATE_LIMITED = "rate_limited"
    CONCURRENCY_LIMIT = "concurrency_limit"
    CONTINUITY = "continuity"
    STORAGE = "storage"
    STREAM_CAPACITY = "stream_capacity"
    TERMINAL_STREAM = "terminal_stream"


class HubClientError(Exception):
    def __init__(self, code: ClientErrorCode, *, retryable: bool = False) -> None:
        super().__init__()
        self.code = code
        self.retryable = retryable

    def __str__(self) -> str:
        return self.code.value

    def __repr__(self) -> str:
        return f"{type(self).__name__}(code={self.code.value!r}, retryable={self.retryable!r})"


class _FixedError(HubClientError):
    error_code: ClientErrorCode

    def __init__(self) -> None:
        super().__init__(self.error_code)


class HubConnectionError(_FixedError): error_code = ClientErrorCode.CONNECTION
class HubAuthenticationError(_FixedError): error_code = ClientErrorCode.AUTHENTICATION
class HubPairingError(_FixedError): error_code = ClientErrorCode.PAIRING
class DeploymentNotReadyError(_FixedError): error_code = ClientErrorCode.DEPLOYMENT_NOT_READY
class EndpointSecurityError(_FixedError): error_code = ClientErrorCode.ENDPOINT_SECURITY
class ProtocolBodyError(_FixedError): error_code = ClientErrorCode.PROTOCOL_BODY
class ProtocolConformanceError(_FixedError): error_code = ClientErrorCode.PROTOCOL_CONFORMANCE
class CursorExpiredError(_FixedError): error_code = ClientErrorCode.CURSOR_EXPIRED
class CursorScopeChangedError(_FixedError): error_code = ClientErrorCode.CURSOR_SCOPE_CHANGED
class CursorInvalidError(_FixedError): error_code = ClientErrorCode.CURSOR_INVALID
class CursorQueryMismatchError(_FixedError): error_code = ClientErrorCode.CURSOR_QUERY_MISMATCH
class QueryTraversalError(_FixedError): error_code = ClientErrorCode.QUERY_TRAVERSAL
class EventIdInvalidError(_FixedError): error_code = ClientErrorCode.EVENT_ID_INVALID
class ReplayExpiredError(_FixedError): error_code = ClientErrorCode.REPLAY_EXPIRED
class StreamCapacityError(_FixedError): error_code = ClientErrorCode.STREAM_CAPACITY
class StorageError(_FixedError): error_code = ClientErrorCode.STORAGE
class TerminalStreamError(_FixedError): error_code = ClientErrorCode.TERMINAL_STREAM


class RateLimitedError(HubClientError):
    def __init__(self, retry_after_seconds: int) -> None:
        super().__init__(ClientErrorCode.RATE_LIMITED, retryable=True)
        self.retry_after_seconds = retry_after_seconds


class ConcurrencyLimitError(HubClientError):
    def __init__(self, retry_after_seconds: int) -> None:
        super().__init__(ClientErrorCode.CONCURRENCY_LIMIT, retryable=True)
        self.retry_after_seconds = retry_after_seconds
```

Keep `ProtocolContractUnavailable = DeploymentNotReadyError` as a temporary
import alias for existing tests. In `client.py`, import the taxonomy from
`errors.py`, set `PROTOCOL_UNAVAILABLE = "synchronized transport is not ready"`,
and render the pending client as
`_PendingProtocolClient(use_tls=True|False)` with no host or port.

- [ ] **Step 6: Run the safe-rendering client tests**

Run: `uv run pytest tests/test_client.py -q`

Expected: PASS after updating constructors in existing tests to take no raw
message; no exception or pending-client representation contains endpoint data.

- [ ] **Step 7: Implement headers, bounded decoding, and strict SSE JSON**

Create `protocol/body.py` with these complete primitives:

```python
from __future__ import annotations

import json
from collections.abc import AsyncIterable
from dataclasses import dataclass
from enum import StrEnum

from ..errors import ProtocolBodyError, ProtocolConformanceError

HeaderFields = tuple[tuple[str, str], ...]


class BodyKind(StrEnum):
    DISCOVERY_OR_PAGE = "discovery_or_page"
    CURRENT = "current"
    PROBLEM = "problem"


BODY_LIMITS = {
    BodyKind.DISCOVERY_OR_PAGE: 2 * 1024 * 1024,
    BodyKind.CURRENT: 256 * 1024,
    BodyKind.PROBLEM: 64 * 1024,
}
MEDIA_TYPES = {
    BodyKind.DISCOVERY_OR_PAGE: "application/json",
    BodyKind.CURRENT: "application/json",
    BodyKind.PROBLEM: "application/problem+json",
}


@dataclass(frozen=True, slots=True)
class JsonResponse:
    status: int
    headers: HeaderFields
    chunks: AsyncIterable[bytes]


def single_header(
    headers: HeaderFields, name: str, *, required: bool
) -> str | None:
    values = [value for field, value in headers if field.casefold() == name.casefold()]
    if len(values) > 1 or (required and len(values) != 1):
        raise ProtocolConformanceError() from None
    return values[0] if values else None


def _validate_declared_length(value: str | None, limit: int) -> None:
    if value is None:
        return
    if not value.isdecimal() or int(value) > limit:
        raise ProtocolBodyError() from None


def _reject_non_finite(_value: str) -> None:
    raise ValueError


def _load_object(text: str, error_type: type[Exception]) -> dict[str, object]:
    try:
        value = json.loads(text, parse_constant=_reject_non_finite)
    except (json.JSONDecodeError, ValueError):
        raise error_type() from None
    if not isinstance(value, dict):
        raise error_type() from None
    return value


def strict_load_json_object(text: str) -> dict[str, object]:
    return _load_object(text, ProtocolConformanceError)


async def async_read_json(
    chunks: AsyncIterable[bytes],
    *,
    kind: BodyKind,
    content_type: str,
    content_length: str | None,
) -> dict[str, object]:
    limit = BODY_LIMITS[kind]
    if content_type.casefold() != MEDIA_TYPES[kind]:
        raise ProtocolBodyError() from None
    _validate_declared_length(content_length, limit)
    body = bytearray()
    async for chunk in chunks:
        if len(body) + len(chunk) > limit:
            raise ProtocolBodyError() from None
        body.extend(chunk)
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError:
        raise ProtocolBodyError() from None
    return _load_object(text, ProtocolBodyError)
```

- [ ] **Step 8: Run bounded-body tests before problem parsing**

Run: `uv run pytest tests/test_protocol_body.py -q -k 'not problem and not rate_limit'`

Expected: PASS.

- [ ] **Step 9: Implement the exact problem allow list**

Append to `protocol/body.py`:

```python
from ..errors import (
    ConcurrencyLimitError,
    CursorExpiredError,
    CursorInvalidError,
    CursorQueryMismatchError,
    CursorScopeChangedError,
    EventIdInvalidError,
    HubClientError,
    RateLimitedError,
    ReplayExpiredError,
)
from .schema import validate_document


class ProblemCode(StrEnum):
    INVALID_REQUEST = "invalid_request"
    INVALID_TIME_RANGE = "invalid_time_range"
    UNSUPPORTED_PROTOCOL_VERSION = "unsupported_protocol_version"
    INVALID_CURSOR = "invalid_cursor"
    CURSOR_EXPIRED = "cursor_expired"
    CURSOR_QUERY_MISMATCH = "cursor_query_mismatch"
    CURSOR_SCOPE_CHANGED = "cursor_scope_changed"
    UNAUTHORIZED = "unauthorized"
    FORBIDDEN = "forbidden"
    NOT_FOUND = "not_found"
    PRECONDITION_REQUIRED = "precondition_required"
    METADATA_REVISION_CONFLICT = "metadata_revision_conflict"
    IDEMPOTENCY_CONFLICT = "idempotency_conflict"
    COMMAND_NOT_SUPPORTED = "command_not_supported"
    COMMAND_NOT_PERMITTED = "command_not_permitted"
    COMMAND_EXPIRED = "command_expired"
    EVENT_REPLAY_EXPIRED = "event_replay_expired"
    EVENT_ID_INVALID = "event_id_invalid"
    RATE_LIMITED = "rate_limited"
    REQUEST_TOO_LARGE = "request_too_large"
    RANGE_TOO_LARGE = "range_too_large"
    CONCURRENCY_LIMIT = "concurrency_limit"
    INTERNAL_ERROR = "internal_error"
    UNAVAILABLE = "unavailable"


class ProblemOperation(StrEnum):
    QUERY = "query"
    EVENT_STREAM = "event_stream"


QUERY_ERRORS: dict[tuple[int, ProblemCode], type[HubClientError]] = {
    (400, ProblemCode.INVALID_CURSOR): CursorInvalidError,
    (409, ProblemCode.CURSOR_QUERY_MISMATCH): CursorQueryMismatchError,
    (403, ProblemCode.CURSOR_SCOPE_CHANGED): CursorScopeChangedError,
    (410, ProblemCode.CURSOR_EXPIRED): CursorExpiredError,
}
EVENT_ERRORS: dict[tuple[int, ProblemCode], type[HubClientError]] = {
    (400, ProblemCode.EVENT_ID_INVALID): EventIdInvalidError,
    (410, ProblemCode.EVENT_REPLAY_EXPIRED): ReplayExpiredError,
}


async def async_parse_problem_response(
    response: JsonResponse, *, operation: ProblemOperation
) -> HubClientError:
    content_type = single_header(response.headers, "Content-Type", required=True)
    content_length = single_header(response.headers, "Content-Length", required=False)
    retry_header = single_header(response.headers, "Retry-After", required=False)
    assert content_type is not None
    document = await async_read_json(
        response.chunks,
        kind=BodyKind.PROBLEM,
        content_type=content_type,
        content_length=content_length,
    )
    validate_document("urn:teslatlas:protocol:schema:error:1.2.0", document)
    try:
        status = document["status"]
        code = ProblemCode(document["code"])
        retryable = document["retryable"]
    except (KeyError, TypeError, ValueError):
        raise ProtocolConformanceError() from None
    if status != response.status or not isinstance(retryable, bool):
        raise ProtocolConformanceError() from None
    if code in (ProblemCode.RATE_LIMITED, ProblemCode.CONCURRENCY_LIMIT):
        if status != 429 or retryable is not True or retry_header is None:
            raise ProtocolConformanceError() from None
        if not retry_header.isdecimal():
            raise ProtocolConformanceError() from None
        delay = int(retry_header)
        if delay > 86400:
            raise ProtocolConformanceError() from None
        body_delay = document.get("retry_after_seconds")
        if body_delay is not None and body_delay != delay:
            raise ProtocolConformanceError() from None
        error_type = (
            RateLimitedError
            if code is ProblemCode.RATE_LIMITED
            else ConcurrencyLimitError
        )
        return error_type(delay)
    if retry_header is not None or retryable is not False:
        raise ProtocolConformanceError() from None
    errors = QUERY_ERRORS if operation is ProblemOperation.QUERY else EVENT_ERRORS
    error_type = errors.get((status, code))
    if error_type is None:
        raise ProtocolConformanceError() from None
    return error_type()
```

The returned exception retains no source document, headers, response, request
ID, title, detail, instance, field errors, or extension values.

- [ ] **Step 10: Run focused tests and Ruff**

Run: `uv run pytest tests/test_protocol_body.py tests/test_client.py -q`

Run: `uv run ruff check custom_components/teslatlas_hub/errors.py custom_components/teslatlas_hub/protocol/body.py custom_components/teslatlas_hub/client.py tests/test_protocol_body.py tests/test_client.py`

Expected: PASS.

- [ ] **Step 11: Commit Task 2**

```bash
git add custom_components/teslatlas_hub/errors.py custom_components/teslatlas_hub/protocol/body.py custom_components/teslatlas_hub/client.py tests/test_protocol_body.py tests/test_client.py
git commit -m "feat: bound protocol bodies and errors"
```

### Task 3: Protocol-aligned immutable models and discovery negotiation

**Files:**
- Modify: `custom_components/teslatlas_hub/models.py`
- Create: `custom_components/teslatlas_hub/protocol/discovery.py`
- Create: `tests/test_protocol_discovery.py`
- Modify: `tests/test_models.py`
- Modify: `tests/helpers.py`

**Interfaces:**
- Produces: immutable `DiscoveryCapability`, `CapabilityIdentity`, `ProtocolLimits`, `HubInfo`, `VehicleSummary`, `VehicleState`, `HubSnapshot`, and `SessionBinding`.
- Produces: `parse_discovery(document, endpoint) -> DiscoveryCandidate`, `confirm_initial_protocol_version(candidate, headers) -> HubInfo`, `confirm_protocol_version(headers, expected="1.2.0") -> None`, and `validate_endpoint(endpoint) -> HubEndpoint`.
- `VehicleState` uses exact protocol fields: `state`, `battery_level_percent`, `range_km`, `odometer_km`, optional temperatures, `locked`, `climate_on`, `charging_state`, `observed_at`, `revision`, and `quality`.

- [ ] **Step 1: Write failing model/discovery tests**

```python
def test_plain_http_accepts_only_literal_loopback():
    assert validate_endpoint(HubEndpoint("127.0.0.1", 7443, False)).host == "127.0.0.1"
    assert validate_endpoint(HubEndpoint("::1", 7443, False)).host == "::1"
    for host in ("localhost", "hub.local", "192.168.1.20"):
        with pytest.raises(EndpointSecurityError):
            validate_endpoint(HubEndpoint(host, 7443, False))


def test_discovery_requires_exact_used_capabilities(protocol_fixture):
    document = protocol_fixture("discovery.json")
    candidate = parse_discovery(document, HubEndpoint("hub.example.invalid", 443, True))
    assert {item.capability_id for item in candidate.capabilities} >= {
        "query.vehicles",
        "events.sse",
    }
```

Append this complete matrix to `tests/test_protocol_discovery.py` (imports are
`copy`, `datetime`, `timezone`, `pytest`, `EndpointSecurityError`,
`ProtocolConformanceError`, all Task 3 models, and all four discovery functions):

```python
ENDPOINT = HubEndpoint("hub.example.invalid", 443, True)


def _capability(document, capability_id: str):
    return next(item for item in document["capabilities"] if item["id"] == capability_id)


@pytest.mark.parametrize("capability_id", ["query.vehicles", "events.sse"])
@pytest.mark.parametrize("mutation", ["missing", "duplicate"])
def test_required_capability_cardinality_is_exact(
    protocol_fixture, capability_id: str, mutation: str
) -> None:
    document = protocol_fixture("discovery.json")
    item = _capability(document, capability_id)
    document["capabilities"].remove(item)
    if mutation == "duplicate":
        document["capabilities"].extend((copy.deepcopy(item), copy.deepcopy(item)))
    with pytest.raises(ProtocolConformanceError):
        parse_discovery(document, ENDPOINT)


@pytest.mark.parametrize("status", ["experimental", "stable"])
def test_required_capability_rejects_conflicting_or_experimental_descriptor(
    protocol_fixture, status: str
) -> None:
    document = protocol_fixture("discovery.json")
    item = _capability(document, "query.vehicles")
    if status == "experimental":
        item["status"] = status
    else:
        duplicate = copy.deepcopy(item)
        duplicate["version"] = "1.1.0"
        document["capabilities"].append(duplicate)
    with pytest.raises(ProtocolConformanceError):
        parse_discovery(document, ENDPOINT)


@pytest.mark.parametrize(
    ("now", "accepted"),
    [("2030-06-30T23:59:59+00:00", True),
     ("2030-07-01T00:00:00+00:00", False),
     ("2030-07-01T00:00:01+00:00", False)],
)
def test_supported_deprecated_descriptor_obeys_effective_sunset(
    protocol_fixture, now: str, accepted: bool
) -> None:
    document = protocol_fixture("discovery.json")
    item = _capability(document, "query.vehicles")
    item["status"] = "deprecated"
    item["deprecation"] = {
        "deprecated_at": "2030-01-01T00:00:00.000Z",
        "sunset_at": "2030-07-01T00:00:00.000Z",
        "successor": "query.vehicles",
        "documentation": "https://example.invalid/migration",
    }
    operation = lambda: parse_discovery(
        document, ENDPOINT, now=datetime.fromisoformat(now)
    )
    if accepted:
        assert operation().hub_id == document["hub_id"]
    else:
        with pytest.raises(ProtocolConformanceError):
            operation()


def test_data_quality_capability_is_optional_for_this_projection(protocol_fixture) -> None:
    document = protocol_fixture("discovery.json")
    document["capabilities"] = [
        item for item in document["capabilities"] if item["id"] != "data-quality"
    ]
    assert parse_discovery(document, ENDPOINT).hub_id == document["hub_id"]


@pytest.mark.parametrize("endpoint_name", ["well_known", "api", "events", "openapi"])
@pytest.mark.parametrize(
    "replacement",
    ["http://hub.example.invalid/v1", "https://other.example.invalid/v1",
     "https://hub.example.invalid:444/v1"],
)
def test_discovery_rejects_cross_origin_endpoint(
    protocol_fixture, endpoint_name: str, replacement: str
) -> None:
    document = protocol_fixture("discovery.json")
    document["endpoints"][endpoint_name] = replacement
    with pytest.raises(EndpointSecurityError):
        parse_discovery(document, ENDPOINT)


@pytest.mark.parametrize(
    ("capability_id", "href", "accepted"),
    [("query.vehicles", "/v1/vehicles", True),
     ("events.sse", "/v1/events", True),
     ("query.vehicles", "/outside/vehicles", False),
     ("events.sse", "/v1/other", False),
     ("query.vehicles", "https://other.invalid/v1/vehicles", False),
     ("events.sse", "/v1/events#fragment", False)],
)
def test_capability_hrefs_resolve_only_under_declared_api_or_event_origin(
    protocol_fixture, capability_id: str, href: str, accepted: bool
) -> None:
    document = protocol_fixture("discovery.json")
    _capability(document, capability_id)["href"] = href
    if accepted:
        assert parse_discovery(document, ENDPOINT).hub_id == document["hub_id"]
    else:
        with pytest.raises(ProtocolConformanceError):
            parse_discovery(document, ENDPOINT)


def test_discovery_rejects_unsupported_protocol_version(protocol_fixture) -> None:
    document = protocol_fixture("discovery.json")
    document["protocol"]["supported_versions"] = ["2.0.0", "2.1.0", "2.2.0"]
    document["protocol"]["minimum_client_version"] = "2.0.0"
    document["protocol"]["current_version"] = "2.2.0"
    with pytest.raises(ProtocolConformanceError):
        parse_discovery(document, ENDPOINT)


@pytest.mark.parametrize(
    "headers",
    [(), (("Teslatlas-Protocol-Version", "1.1.0"),),
     (("Teslatlas-Protocol-Version", "1.2.0"),
      ("teslatlas-protocol-version", "1.2.0"))],
)
def test_authenticated_response_version_must_match_once_and_exactly(headers) -> None:
    with pytest.raises(ProtocolConformanceError):
        confirm_protocol_version(headers)


def test_confirmed_hub_info_strips_discovery_hrefs(protocol_fixture) -> None:
    candidate = parse_discovery(protocol_fixture("discovery.json"), ENDPOINT)
    info = confirm_initial_protocol_version(
        candidate, (("Teslatlas-Protocol-Version", "1.2.0"),)
    )
    assert all(not hasattr(item, "href") for item in info.capabilities)
    assert "/v1/" not in repr(info.capabilities)
    assert candidate.proposed_protocol_version == "1.2.0"
```

- [ ] **Step 2: Run focused tests and verify red**

Run: `uv run pytest tests/test_models.py tests/test_protocol_discovery.py -q`

Expected: FAIL because old temporary fields and discovery parser remain.

- [ ] **Step 3: Replace temporary models and implement discovery checks**

```python
@dataclass(frozen=True, slots=True)
class DiscoveryCapability:
    capability_id: str
    version: str
    href: str


@dataclass(frozen=True, slots=True)
class CapabilityIdentity:
    capability_id: str
    version: str


@dataclass(frozen=True, slots=True)
class ProtocolLimits:
    default_page_size: int
    max_page_size: int
    max_concurrent_requests: int
    max_sse_connections: int
    event_replay_retention_seconds: int


@dataclass(frozen=True, slots=True)
class HubInfo:
    hub_id: str
    protocol_version: str
    capabilities: tuple[CapabilityIdentity, ...]
    api_origin: str
    event_origin: str
    limits: ProtocolLimits


@dataclass(frozen=True, slots=True)
class DiscoveryCandidate:
    hub_id: str
    proposed_protocol_version: str
    capabilities: tuple[DiscoveryCapability, ...]
    api_origin: str
    event_origin: str
    limits: ProtocolLimits


@dataclass(frozen=True, slots=True)
class VehicleSummary:
    vehicle_id: str
    name: str
    state: str
    last_observed_at: datetime
    revision: int


@dataclass(frozen=True, slots=True)
class VehicleState:
    vehicle_id: str
    name: str
    observed_at: datetime
    revision: int
    state: str
    battery_level_percent: float | None
    range_km: float | None
    odometer_km: float | None
    inside_temperature_c: float | None
    outside_temperature_c: float | None
    locked: bool | None
    climate_on: bool | None
    charging_state: str | None
    quality: str


@dataclass(frozen=True, slots=True)
class SessionBinding:
    hub_id: str
    api_origin: str
    event_origin: str
    protocol_version: str
    normalized_filters: tuple[tuple[str, str], ...]
    credential_generation: int
```

Implement duplicate rejection in `HubSnapshot.create` exactly:

```python
@classmethod
def create(cls, *, info: HubInfo, vehicles: Iterable[VehicleState]) -> HubSnapshot:
    owned = tuple(vehicles)
    vehicle_map = {vehicle.vehicle_id: vehicle for vehicle in owned}
    if len(vehicle_map) != len(owned):
        raise ProtocolConformanceError() from None
    return cls(info=info, vehicles=MappingProxyType(vehicle_map))
```

Create `protocol/discovery.py` with the complete parser below. It validates all
four public URLs against the configured origin before returning anything and
never accepts a stored version as negotiation evidence.

```python
from __future__ import annotations

from datetime import datetime, timezone
from ipaddress import ip_address
from urllib.parse import urljoin, urlsplit

from ..errors import EndpointSecurityError, ProtocolConformanceError
from ..models import (
    CapabilityIdentity, DiscoveryCandidate, DiscoveryCapability, HubEndpoint,
    HubInfo, ProtocolLimits,
)
from . import PROTOCOL_VERSION
from .body import HeaderFields, single_header
from .schema import validate_document
from .semantics import validate_discovery_semantics

VERSION_HEADER = "Teslatlas-Protocol-Version"
REQUIRED_CAPABILITIES = {"query.vehicles", "events.sse"}


def validate_endpoint(endpoint: HubEndpoint) -> HubEndpoint:
    if not endpoint.host or not 1 <= endpoint.port <= 65535:
        raise EndpointSecurityError() from None
    if not endpoint.use_tls:
        try:
            loopback = ip_address(endpoint.host).is_loopback
        except ValueError:
            loopback = False
        if not loopback:
            raise EndpointSecurityError() from None
    return endpoint


def _origin(parts) -> tuple[str, str, int]:
    if parts.username is not None or parts.password is not None:
        raise EndpointSecurityError() from None
    scheme = parts.scheme.casefold()
    host = parts.hostname.casefold() if parts.hostname else ""
    try:
        port = parts.port or (443 if scheme == "https" else 80)
    except ValueError:
        raise EndpointSecurityError() from None
    return scheme, host, port


def _configured_origin(endpoint: HubEndpoint) -> tuple[str, str, int]:
    checked = validate_endpoint(endpoint)
    return ("https" if checked.use_tls else "http", checked.host.casefold(), checked.port)


def _utc(value: str) -> datetime:
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ProtocolConformanceError() from None
    return parsed.astimezone(timezone.utc)


def parse_discovery(
    document: object,
    endpoint: HubEndpoint,
    *,
    now: datetime | None = None,
) -> DiscoveryCandidate:
    value = validate_document(
        "urn:teslatlas:protocol:schema:discovery:1.2.0", document
    )
    validate_discovery_semantics(value)
    expected_origin = _configured_origin(endpoint)
    endpoints = value["endpoints"]
    assert isinstance(endpoints, dict)
    parsed_endpoints = {name: urlsplit(endpoints[name]) for name in
                        ("well_known", "api", "events", "openapi")}
    if any(_origin(parts) != expected_origin for parts in parsed_endpoints.values()):
        raise EndpointSecurityError() from None
    protocol = value["protocol"]
    assert isinstance(protocol, dict)
    if PROTOCOL_VERSION not in protocol["supported_versions"]:
        raise ProtocolConformanceError() from None
    effective_now = now or datetime.now(timezone.utc)
    selected: list[DiscoveryCapability] = []
    for raw in value["capabilities"]:
        assert isinstance(raw, dict)
        if raw["id"] not in REQUIRED_CAPABILITIES and raw["id"] != "data-quality":
            continue
        if raw["status"] == "experimental":
            if raw["id"] in REQUIRED_CAPABILITIES:
                raise ProtocolConformanceError() from None
            continue
        if raw["status"] == "deprecated":
            sunset = raw["deprecation"]["sunset_at"]
            if sunset is not None and effective_now >= _utc(sunset):
                raise ProtocolConformanceError() from None
        href = raw["href"]
        if not isinstance(href, str) or not href.startswith("/") or urlsplit(href).fragment:
            raise ProtocolConformanceError() from None
        if raw["id"] == "events.sse":
            if urljoin(endpoints["events"], href) != endpoints["events"]:
                raise ProtocolConformanceError() from None
        elif not urlsplit(href).path.startswith(urlsplit(endpoints["api"]).path.rstrip("/") + "/"):
            raise ProtocolConformanceError() from None
        selected.append(DiscoveryCapability(raw["id"], raw["version"], href))
    counts = {identifier: sum(item.capability_id == identifier for item in selected)
              for identifier in REQUIRED_CAPABILITIES}
    if counts != {identifier: 1 for identifier in REQUIRED_CAPABILITIES}:
        raise ProtocolConformanceError() from None
    limits = value["limits"]
    assert isinstance(limits, dict)
    origin_text = f"{expected_origin[0]}://{expected_origin[1]}:{expected_origin[2]}"
    return DiscoveryCandidate(
        hub_id=value["hub_id"], proposed_protocol_version=PROTOCOL_VERSION,
        capabilities=tuple(selected), api_origin=origin_text,
        event_origin=origin_text,
        limits=ProtocolLimits(
            default_page_size=limits["default_page_size"],
            max_page_size=limits["max_page_size"],
            max_concurrent_requests=limits["max_concurrent_requests"],
            max_sse_connections=limits["max_sse_connections"],
            event_replay_retention_seconds=limits["event_replay_retention_seconds"],
        ),
    )


def confirm_protocol_version(
    headers: HeaderFields, expected: str = PROTOCOL_VERSION
) -> None:
    if single_header(headers, VERSION_HEADER, required=True) != expected:
        raise ProtocolConformanceError() from None


def confirm_initial_protocol_version(
    candidate: DiscoveryCandidate, headers: HeaderFields
) -> HubInfo:
    confirm_protocol_version(headers, candidate.proposed_protocol_version)
    return HubInfo(
        hub_id=candidate.hub_id,
        protocol_version=candidate.proposed_protocol_version,
        capabilities=tuple(
            CapabilityIdentity(item.capability_id, item.version)
            for item in candidate.capabilities
        ),
        api_origin=candidate.api_origin,
        event_origin=candidate.event_origin,
        limits=candidate.limits,
    )
```

- [ ] **Step 4: Replace temporary fixture parsing**

In `tests/helpers.py`, define `protocol_fixture(name)` as a fresh strict JSON
load from `tests/fixtures/protocol-1.2.0/name`. Build `VehicleSummary` from the
matching list item and `VehicleState` from `current-state.json`; explicitly set
the list item's `state` to `"divergent-list-sentinel"` in
`test_current_state_wins_over_list_summary`, then assert the built state equals
`current["state"]` and not the sentinel. Map every `VehicleState` constructor
argument one-for-one from the names in the Task 3 interface; use
`datetime.fromisoformat(value.replace("Z", "+00:00"))` for both timestamps.

- [ ] **Step 5: Run model/discovery tests green**

Run: `uv run pytest tests/test_models.py tests/test_protocol_discovery.py -q`

Expected: PASS.

- [ ] **Step 6: Commit Task 3**

```bash
git add custom_components/teslatlas_hub/models.py custom_components/teslatlas_hub/protocol/discovery.py tests/test_models.py tests/test_protocol_discovery.py tests/helpers.py
git commit -m "feat: align models with protocol discovery"
```

### Task 4: All-or-nothing vehicle/current traversal

**Files:**
- Create: `custom_components/teslatlas_hub/protocol/query.py`
- Create: `tests/test_protocol_query.py`
- Modify: `tests/helpers.py`

**Interfaces:**
- Consumes: validated discovery, bounded JSON reader, schema validators, and immutable models.
- Produces: `QuerySource` protocol with `async_vehicle_page(cursor)` and `async_current_state(vehicle_id)`.
- Produces: immutable `QueryCandidate(snapshot: HubSnapshot, binding: SessionBinding)`.
- Produces: `async_build_candidate(source, *, discovery, credential_generation, normalized_filters=(), sleep=asyncio.sleep, clock=time.monotonic, restart_limit=3, time_budget_seconds=30) -> QueryCandidate`.

- [ ] **Step 1: Write failing pagination/atomicity tests**

```python
async def test_changed_snapshot_revision_discards_everything(query_source):
    query_source.pages = [page("snap-a", "cursor-1"), page("snap-b", None)]
    with pytest.raises(ProtocolConformanceError):
        await async_build_candidate(
            query_source, discovery=DISCOVERY, credential_generation=1
        )
    assert query_source.published == []


async def test_scope_churn_exhausts_one_shared_budget(query_source):
    query_source.failures = [
        query_problem_response(
            status=403,
            code="cursor_scope_changed",
            retryable=False,
        )
        for _ in range(4)
    ]
    with pytest.raises(QueryTraversalError):
        await async_build_candidate(
            query_source,
            discovery=DISCOVERY,
            credential_generation=1,
            restart_limit=3,
        )
```

Define `FakeQuerySource` in `tests/helpers.py` with `pages: deque[JsonResponse]`,
`currents: dict[str, JsonResponse]`, `page_cursors: list[str | None]`,
`current_calls: list[str]`, `cancelled: set[str]`, and the two exact async
`QuerySource` methods. Each method records its argument and pops/returns only
the configured raw `JsonResponse`; it never raises a fixture-injected typed
error. Then append this complete matrix:

```python
async def test_cursor_is_passed_verbatim_and_null_terminates(query_source) -> None:
    query_source.pages.extend((page("snap-a", "Opaque_~-Z9"), page("snap-a", None)))
    await async_build_candidate(query_source, discovery=DISCOVERY, credential_generation=1)
    assert query_source.page_cursors == [None, "Opaque_~-Z9"]


@pytest.mark.parametrize("duplicate", ["cursor", "vehicle"])
async def test_duplicate_cursor_or_vehicle_rejects_whole_candidate(
    query_source, duplicate: str
) -> None:
    if duplicate == "cursor":
        query_source.pages.extend((page("snap-a", "same"), page("snap-a", "same")))
    else:
        query_source.pages.extend((page("snap-a", "next"), page("snap-a", None)))
        query_source.pages[1] = page("snap-a", None, vehicle_id="vehicle_demo_alpha")
    with pytest.raises(ProtocolConformanceError):
        await async_build_candidate(query_source, discovery=DISCOVERY, credential_generation=1)


@pytest.mark.parametrize(
    ("pages", "vehicles", "accepted"),
    [(100, 100, True), (101, 101, False), (1, 10_000, True), (1, 10_001, False)],
)
async def test_local_page_and_item_caps_are_exact(
    capped_query_source, pages: int, vehicles: int, accepted: bool
) -> None:
    capped_query_source.configure_counts(pages=pages, vehicles=vehicles)
    operation = async_build_candidate(
        capped_query_source, discovery=DISCOVERY, credential_generation=1
    )
    if accepted:
        assert len((await operation).snapshot.vehicles) == vehicles
    else:
        with pytest.raises(QueryTraversalError):
            await operation


async def test_cursor_expired_restarts_from_null_within_shared_budget(query_source) -> None:
    query_source.pages.extend((page("snap-a", "old"), raw_problem(410, "cursor_expired"),
                               page("snap-b", None)))
    await async_build_candidate(query_source, discovery=DISCOVERY, credential_generation=7)
    assert query_source.page_cursors == [None, "old", None]


@pytest.mark.parametrize("status,code", [(400, "invalid_cursor"),
                                           (409, "cursor_query_mismatch")])
async def test_invalid_or_query_mismatch_cursor_is_terminal(
    query_source, status: int, code: str
) -> None:
    query_source.pages.append(raw_problem(status, code))
    with pytest.raises((CursorInvalidError, CursorQueryMismatchError)):
        await async_build_candidate(query_source, discovery=DISCOVERY, credential_generation=4)
    assert query_source.page_cursors == [None]


async def test_scope_change_restarts_only_with_same_credential(query_source) -> None:
    query_source.pages.extend((raw_problem(403, "cursor_scope_changed"),
                               raw_problem(403, "cursor_scope_changed"),
                               page("snap-c", None)))
    result = await async_build_candidate(
        query_source, discovery=DISCOVERY, credential_generation=9, restart_limit=2
    )
    assert result.binding.credential_generation == 9
    assert query_source.page_cursors == [None, None, None]


@pytest.mark.parametrize("target", ["page", "current"])
async def test_every_page_and_current_response_confirms_protocol_version(
    query_source, target: str
) -> None:
    query_source.replace_version(target, "1.1.0")
    with pytest.raises(ProtocolConformanceError):
        await async_build_candidate(query_source, discovery=DISCOVERY, credential_generation=1)


async def test_missing_current_resource_cancels_siblings_and_discards_candidate(
    multi_vehicle_query_source,
) -> None:
    multi_vehicle_query_source.remove_current("vehicle-2")
    with pytest.raises(ProtocolConformanceError):
        await async_build_candidate(
            multi_vehicle_query_source, discovery=DISCOVERY, credential_generation=1
        )
    assert multi_vehicle_query_source.published == []
    assert multi_vehicle_query_source.cancelled


async def test_query_cancellation_propagates_and_closes_owned_work(
    blocking_query_source,
) -> None:
    task = asyncio.create_task(async_build_candidate(
        blocking_query_source, discovery=DISCOVERY, credential_generation=1
    ))
    await blocking_query_source.started.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert blocking_query_source.open_operations == 0


async def test_device_retirement_callback_runs_only_after_complete_candidate_commit(
    query_source,
) -> None:
    calls = []
    candidate = await async_build_candidate(
        query_source, discovery=DISCOVERY, credential_generation=1
    )
    await async_commit_candidate(candidate, publish=lambda value: calls.append(("publish", value)),
                                 retire=lambda ids: calls.append(("retire", ids)))
    assert [name for name, _ in calls] == ["publish", "retire"]
```

Every cursor fixture above is a `JsonResponse`; traversal must call `async_parse_problem_response(..., operation=ProblemOperation.QUERY)` and never trust a fixture-injected typed exception.

- [ ] **Step 2: Run focused tests and verify red**

Run: `uv run pytest tests/test_protocol_query.py -q`

Expected: FAIL because `protocol/query.py` is absent.

- [ ] **Step 3: Implement private candidate traversal**

```python
class QuerySource(Protocol):
    async def async_vehicle_page(self, cursor: str | None) -> JsonResponse:
        raise NotImplementedError

    async def async_current_state(self, vehicle_id: str) -> JsonResponse:
        raise NotImplementedError


@dataclass(frozen=True, slots=True)
class QueryCandidate:
    snapshot: HubSnapshot
    binding: SessionBinding


async def async_build_candidate(
    source,
    *,
    discovery,
    credential_generation,
    normalized_filters=(),
    sleep=asyncio.sleep,
    clock=time.monotonic,
    restart_limit=3,
    time_budget_seconds=30,
):
    deadline = clock() + time_budget_seconds
    attempts = 0
    while True:
        try:
            info, summaries = await _async_collect_all_pages(
                source, discovery=discovery, deadline=deadline, clock=clock
            )
            break
        except (CursorExpiredError, CursorScopeChangedError):
            attempts += 1
            if attempts > restart_limit or clock() >= deadline:
                raise QueryTraversalError() from None
            await sleep(0)
    binding = SessionBinding(
        hub_id=info.hub_id,
        api_origin=info.api_origin,
        event_origin=info.event_origin,
        protocol_version=info.protocol_version,
        normalized_filters=tuple(normalized_filters),
        credential_generation=credential_generation,
    )
    states = await _async_load_all_current(source, summaries, info)
    return QueryCandidate(
        snapshot=HubSnapshot.create(info=info, vehicles=states),
        binding=binding,
    )
```

Add these complete private helpers in the same module; `_vehicle_summary` and
`_vehicle_state` are literal constructors mapping the protocol keys shown here,
so no fixture-specific adapter remains:

```python
async def _success_document(response, *, kind, schema_id, initial=None, expected=None):
    if response.status >= 400:
        raise await async_parse_problem_response(response, operation=ProblemOperation.QUERY)
    if initial is not None:
        info = confirm_initial_protocol_version(initial, response.headers)
    else:
        confirm_protocol_version(response.headers, expected)
        info = None
    document = await async_read_json(
        response.chunks, kind=kind,
        content_type=single_header(response.headers, "Content-Type", required=True),
        content_length=single_header(response.headers, "Content-Length", required=False),
    )
    return info, validate_document(schema_id, document)


def _stamp(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def _vehicle_summary(item) -> VehicleSummary:
    return VehicleSummary(
        vehicle_id=item["vehicle_id"], name=item["display_name"], state=item["state"],
        last_observed_at=_stamp(item["last_observed_at"]), revision=item["revision"],
    )


def _vehicle_state(document, summary) -> VehicleState:
    if document["vehicle_id"] != summary.vehicle_id:
        raise ProtocolConformanceError() from None
    return VehicleState(
        vehicle_id=document["vehicle_id"], name=summary.name,
        observed_at=_stamp(document["observed_at"]), revision=document["revision"],
        state=document["state"], battery_level_percent=document["battery_level_percent"],
        range_km=document["range_km"], odometer_km=document["odometer_km"],
        inside_temperature_c=document.get("inside_temperature_c"),
        outside_temperature_c=document.get("outside_temperature_c"),
        locked=document["locked"], climate_on=document["climate_on"],
        charging_state=document["charging_state"], quality=document["quality"]["quality"],
    )


async def _async_collect_all_pages(source, *, discovery, deadline, clock):
    cursor = None
    seen_cursors = set()
    summaries = {}
    snapshot_revision = None
    info = None
    for page_number in range(1, 102):
        if page_number > 100 or clock() >= deadline:
            raise QueryTraversalError() from None
        response = await source.async_vehicle_page(cursor)
        confirmed, document = await _success_document(
            response, kind=BodyKind.DISCOVERY_OR_PAGE,
            schema_id="urn:teslatlas:protocol:schema:resources:1.2.0",
            initial=discovery if info is None else None,
            expected=None if info is None else info.protocol_version,
        )
        if confirmed is not None:
            info = confirmed
        revision = document["snapshot_revision"]
        if snapshot_revision is None:
            snapshot_revision = revision
        elif revision != snapshot_revision:
            raise ProtocolConformanceError() from None
        for item in document["items"]:
            summary = _vehicle_summary(item)
            if summary.vehicle_id in summaries or len(summaries) == 10_000:
                raise QueryTraversalError() from None
            summaries[summary.vehicle_id] = summary
        cursor = document["next_cursor"]
        if cursor is None:
            assert info is not None
            return info, tuple(summaries.values())
        if cursor in seen_cursors:
            raise ProtocolConformanceError() from None
        seen_cursors.add(cursor)
    raise QueryTraversalError() from None


async def _async_load_all_current(source, summaries, info):
    semaphore = asyncio.Semaphore(min(info.limits.max_concurrent_requests, 8))
    states = {}

    async def load(summary):
        async with semaphore:
            response = await source.async_current_state(summary.vehicle_id)
            _, document = await _success_document(
                response, kind=BodyKind.CURRENT,
                schema_id="urn:teslatlas:protocol:schema:resources:1.2.0",
                expected=info.protocol_version,
            )
            states[summary.vehicle_id] = _vehicle_state(document, summary)

    async with asyncio.TaskGroup() as group:
        for summary in summaries:
            group.create_task(load(summary))
    return tuple(states[summary.vehicle_id] for summary in summaries)


async def async_commit_candidate(candidate, *, publish, retire) -> None:
    result = publish(candidate)
    if inspect.isawaitable(result):
        await result
    retirement = retire(frozenset(candidate.snapshot.vehicles))
    if inspect.isawaitable(retirement):
        await retirement
```

Import every referenced name explicitly (`asyncio`, `inspect`, `datetime`,
the error/body/discovery/schema/model symbols). Keep the candidate local until
all tasks exit successfully; `TaskGroup` cancellation provides sibling cleanup.

- [ ] **Step 4: Run query and model gates**

Run: `uv run pytest tests/test_protocol_query.py tests/test_models.py -q`

Expected: PASS.

- [ ] **Step 5: Commit Task 4**

```bash
git add custom_components/teslatlas_hub/protocol/query.py tests/test_protocol_query.py tests/helpers.py
git commit -m "feat: build atomic protocol query candidates"
```

### Task 5: Fail-closed config security and authenticated-read gating

**Files:**
- Modify: `custom_components/teslatlas_hub/client.py`
- Modify: `custom_components/teslatlas_hub/config_flow.py`
- Modify: `custom_components/teslatlas_hub/const.py`
- Modify: `custom_components/teslatlas_hub/models.py`
- Modify: `custom_components/teslatlas_hub/manifest.json`
- Modify: `custom_components/teslatlas_hub/strings.json`
- Modify: `custom_components/teslatlas_hub/translations/en.json`
- Modify: `tests/helpers.py`
- Modify: `tests/test_client.py`
- Modify: `tests/test_config_flow.py`
- Modify: `tests/test_models.py`
- Modify: `tests/test_package.py`

**Interfaces:**
- Adds: `HUB_NAME = "Teslatlas Hub"`; config entries never use a discovery or pairing display name.
- Replaces temporary `PairingResult(info, access_token)` with `PairingClaim(access_token: str = field(repr=False))`; a pairing response cannot construct or carry `HubInfo`.
- Changes: `TeslatlasHubClient.async_probe() -> DiscoveryCandidate` and `TeslatlasHubClient.async_pair(pairing_secret) -> PairingClaim`.
- Adds: `TeslatlasHubClient.async_validate_secret_channel() -> None`, an internal proof boundary with no invented route or payload shape. The production pending client raises `DeploymentNotReadyError`; fixture clients may prove sequencing.
- Adds: `TeslatlasHubClient.async_validate_access() -> HubInfo`, defined as one authenticated, version-confirmed `query.vehicles` read.
- Adds config key: `credential_generation: int`, initialized to `1` and incremented only after successful reauthentication.
- Removes: Zeroconf manifest advertisement and every discovery auto-update path.

- [ ] **Step 1: Write failing endpoint/secret-flow tests**

```python
async def test_reconfigure_never_creates_a_secret_bearing_candidate_client(hass):
    entry = configured_entry(hass)
    with patch(CREATE_CLIENT) as create:
        result = await hass.config_entries.flow.async_init(
            DOMAIN,
            context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        )
        result = await hass.config_entries.flow.async_configure(
            result["flow_id"],
            {CONF_HOST: "new.example", CONF_PORT: 443, CONF_USE_TLS: True},
        )
    assert result["errors"] == {"base": "deployment_not_ready"}
    create.assert_not_called()
    assert entry.data[CONF_HOST] == "old.example"
```

- [ ] Add `test_plain_http_endpoint_accepts_only_literal_loopback`, parametrized with `127.0.0.1`, `::1`, `localhost`, `.local`, private IPv4, and public names.
- [ ] Add `test_insecure_endpoint_fails_before_client_creation`.
- [ ] Add `test_pairing_password_exists_only_in_transient_form_and_claim_call`.
- [ ] Add `test_redirects_are_terminal_before_secret_forwarding`, parametrized over same-origin and cross-origin `3xx`.
- [ ] Add `test_secret_channel_validation_precedes_pairing_claim` by asserting the fixture call order.
- [ ] Add `test_unauthenticated_probe_returns_discovery_candidate_not_hub_info` and `test_pairing_claim_contains_token_only`, asserting neither interface can construct authenticated `HubInfo`.
- [ ] Add `test_authenticated_access_is_only_config_flow_hub_info_source`, asserting it runs through the fresh bearer client after claim-client close and before entry mutation.
- [ ] Add `test_pair_and_reauth_authenticated_read_failure_is_atomic`, comparing entry data before/after.
- [ ] Add `test_entry_title_is_always_fixed_hub_name`.
- [ ] Add `test_wrong_or_duplicate_hub_aborts_without_mutation`, parametrized over both cases.
- [ ] Add `test_credential_generation_increments_only_with_successful_reauth_update`.
- [ ] Add `test_pairing_secret_is_never_persisted_or_logged`.
- [ ] Add `test_zeroconf_surface_is_completely_absent`, checking handlers, imports, translations, manifest keys, and package tests.

- [ ] **Step 2: Run config/package tests and verify red**

Run: `uv run pytest tests/test_client.py tests/test_config_flow.py tests/test_models.py tests/test_package.py -q`

Expected: FAIL on plaintext acceptance, candidate bearer forwarding, early entry creation, and Zeroconf advertisement.

- [ ] **Step 3: Harden flow sequencing**

For a new flow, local endpoint validation and `async_probe()` retain only `DiscoveryCandidate`; no unauthenticated path constructs `HubInfo`. Implement this order for pair and reauth fixtures: local endpoint validation -> `async_validate_secret_channel()` -> transient `PairingClaim` -> close claim client -> new bearer-scoped client -> `async_validate_access()` returning the first `HubInfo` -> compare its Hub ID to the discovery candidate or existing entry -> close bearer client -> atomic entry update. Remove `PairingResult` and every `.info` use from models, client, fixtures, and config flow. The pending production client fails before pre-secret proof. Initialize `credential_generation=1` on entry creation; increment it only in the same successful reauthentication update as the new bearer. Reconfigure performs only local endpoint validation and returns `deployment_not_ready`; it sends neither stored bearer nor pairing material. All created clients close on success, classified failure, abort, and cancellation.

Remove `async_step_zeroconf`, `ZeroconfServiceInfo`, `ZEROCONF_TYPE`, discovery forms/translations, and `manifest.json` Zeroconf. Disable automatic redirects in the future transport interface; tests assert both same-origin and cross-origin `3xx` are terminal.

- [ ] **Step 4: Run config flow at full branch coverage**

Run: `uv run pytest tests/test_client.py tests/test_config_flow.py tests/test_models.py tests/test_package.py --cov=custom_components.teslatlas_hub.config_flow --cov-branch --cov-fail-under=100 -q`

Expected: PASS and config flow 100% branch coverage.

- [ ] **Step 5: Commit Task 5**

```bash
git add custom_components/teslatlas_hub/client.py custom_components/teslatlas_hub/config_flow.py custom_components/teslatlas_hub/const.py custom_components/teslatlas_hub/models.py custom_components/teslatlas_hub/manifest.json custom_components/teslatlas_hub/strings.json custom_components/teslatlas_hub/translations/en.json tests/helpers.py tests/test_client.py tests/test_config_flow.py tests/test_models.py tests/test_package.py
git commit -m "fix: fail closed before secret transport"
```

### Task 6: Allow-listed diagnostics and classified logs

**Files:**
- Modify: `custom_components/teslatlas_hub/diagnostics.py`
- Modify: `custom_components/teslatlas_hub/coordinator.py`
- Modify: `tests/test_diagnostics.py`
- Modify: `tests/test_coordinator.py`

**Interfaces:**
- Produces diagnostics keys only: `entry_version`, `transport.tls`, and safe runtime version/capability/state/error/count/checkpoint-presence fields.
- Consumes: `coordinator.connection_state`, `coordinator.error_code`, `coordinator.last_event_id is not None`, and immutable snapshot aggregates.
- Adds explicit read-only coordinator properties with initial values `connection_state=ConnectionState.PENDING` and `error_code=None`; the runtime plan replaces the static values with state-machine transitions.

- [ ] **Step 1: Write failing recursive sentinel tests**

```python
async def test_diagnostics_are_allow_listed(hass, caplog):
    entry = entry_with_unknown_sentinels(hass, "PRIVATE_SENTINEL")
    result = await async_get_config_entry_diagnostics(hass, entry)
    encoded = json.dumps(result, sort_keys=True)
    assert set(result) == {"entry_version", "transport", "runtime"}
    assert "PRIVATE_SENTINEL" not in encoded
    assert "PRIVATE_SENTINEL" not in caplog.text
    assert "entry_data" not in result
```

- [ ] Parameterize `test_diagnostics_are_allow_listed` over host, port, bearer, unknown legacy key, Hub/vehicle ID and name, location, timestamp, replay ID, request ID, exception text, and raw event sentinels.
- [ ] Add `test_safe_logs_contain_only_client_error_code`, exercising every `ClientErrorCode` and rejecting exception interpolation.
- [ ] Add `test_safe_error_and_exception_rendering_has_no_runtime_values`, asserting config-flow error text and every exception `str`/`repr` use fixed allow-listed text only; runtime repair issues are added and tested in the runtime plan.

- [ ] **Step 2: Run focused tests and verify red**

Run: `uv run pytest tests/test_diagnostics.py tests/test_coordinator.py -q`

Expected: FAIL because diagnostics start from raw entry data and coordinator interpolates exceptions.

- [ ] **Step 3: Replace deny-list output with a literal safe schema**

```python
return {
    "entry_version": {"major": entry.version, "minor": entry.minor_version},
    "transport": {"tls": bool(entry.data[CONF_USE_TLS])},
    "runtime": {
        "available": coordinator.last_update_success,
        "protocol_version": snapshot.info.protocol_version,
        "capabilities": sorted(
            descriptor.capability_id for descriptor in snapshot.info.capabilities
        ),
        "connection_state": coordinator.connection_state.value,
        "error_code": (
            None if coordinator.error_code is None else coordinator.error_code.value
        ),
        "vehicle_count": len(snapshot.vehicles),
        "data_quality_counts": quality_counts(snapshot),
        "last_event_id_present": coordinator.last_event_id is not None,
    },
}
```

Coordinator logging uses only `ClientErrorCode.value`; safe translated config-flow error text never formats exception data. The later runtime plan owns translated repair issues.
Define `ConnectionState(StrEnum)` initially with `PENDING`; add explicit `_connection_state: ConnectionState` and `_error_code: ClientErrorCode | None` fields and properties to the coordinator so diagnostics never use `getattr` or inspect exception objects. Preserve current lifecycle behavior until the runtime plan replaces it.

- [ ] **Step 4: Run privacy and contract-plan gates**

Run: `uv run pytest tests/test_diagnostics.py tests/test_client.py tests/test_protocol_body.py tests/test_protocol_schema.py tests/test_protocol_discovery.py tests/test_protocol_query.py tests/test_config_flow.py -q`

Run: `uv run ruff format --check . && uv run ruff check .`

Expected: PASS.

- [ ] **Step 5: Commit Task 6**

```bash
git add custom_components/teslatlas_hub/diagnostics.py custom_components/teslatlas_hub/coordinator.py tests/test_diagnostics.py tests/test_coordinator.py
git commit -m "fix: allow list support diagnostics"
```
