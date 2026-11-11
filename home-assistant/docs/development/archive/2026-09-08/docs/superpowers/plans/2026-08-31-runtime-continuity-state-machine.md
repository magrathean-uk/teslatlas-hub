# Runtime Continuity State Machine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the racing snapshot-then-stream fixture lifecycle with bounded event parsing, validated atomic projection/checkpoint commits, and an explicit synchronized-session coordinator while keeping the production adapter disabled.

**Architecture:** A byte-bounded SSE parser produces typed stream items. An event validator transforms those items against an immutable committed projection. A private atomic Home Assistant `Store` persists projection, binding, generation, and opaque replay identity in one document before publication. The coordinator owns exactly one synchronized or resumed fixture session; the production client refuses both operations until the Hub supplies the missing synchronization/deployment contract.

**Tech Stack:** Python 3.14.2+, asyncio, Home Assistant 2026.8.3 `DataUpdateCoordinator` and `Store`, SHA-256 content identity, pytest-homeassistant-custom-component, Ruff.

**Spec:** `docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md`

## Global Constraints

- Start after the contract/security and entity/registry plans pass.
- Reuse `SessionBinding` from `models.py`; do not create a second binding type or store bearer/claim material.
- Event IDs are opaque. Never decode, compare, sort, synthesize, or expose them.
- Persist a projected event before Home Assistant publication; never restore an orphan checkpoint or a projection with a mismatched binding/generation.
- Unknown event names are ignored before JSON decoding; known events require exact profile schema and semantic validation.
- Never drop/coalesce stream items, infer ordering from revisions/timestamps, or recover malformed known events through query refresh.
- Keep `create_client()` fail-closed. Only test fixtures may claim an atomic synchronized boundary.
- Execute Tasks 1-5 in order. Each task begins with failing tests and ends with a focused gate and commit.

---

### Task 1: Parse and buffer SSE within fixed limits

**Files:**
- Create: `custom_components/teslatlas_hub/sse.py`
- Create: `tests/test_sse.py`

**Interfaces:**

```python
@dataclass(frozen=True, slots=True)
class SseDispatch:
    event_name: str
    event_id: str
    data: str
    content_sha256: str


@dataclass(frozen=True, slots=True)
class SseCheckpointReset: ...


@dataclass(frozen=True, slots=True)
class SseRetry:
    milliseconds: int


@dataclass(frozen=True, slots=True)
class SseHeartbeat: ...


type StreamItem = SseDispatch | SseCheckpointReset | SseRetry | SseHeartbeat


def validate_sse_handshake(
    *, status: int, headers: HeaderFields, expected_version: str
) -> None: ...


class SseParser:
    def feed_data(self, chunk: bytes) -> tuple[StreamItem, ...]: ...
    def finish(self) -> tuple[StreamItem, ...]: ...


class BoundedEventBuffer:
    async def async_put(self, item: StreamItem) -> None: ...
    async def async_get(self) -> StreamItem: ...
```

Limits are exactly 64 KiB per encoded line, 1 MiB per accumulated event frame, 64 queued items, and five seconds to acquire a queue slot through injected `wait_for`.

- [ ] **Step 1: Write failing chunk/framing tests**

```python
import asyncio
from collections.abc import Awaitable

import pytest

from custom_components.teslatlas_hub.errors import (
    ProtocolConformanceError,
    StreamCapacityError,
    TerminalStreamError,
)
from custom_components.teslatlas_hub.protocol.body import HeaderFields
from custom_components.teslatlas_hub.sse import (
    LINE_LIMIT,
    FRAME_LIMIT,
    BoundedEventBuffer,
    SseCheckpointReset,
    SseDispatch,
    SseHeartbeat,
    SseParser,
    SseRetry,
    validate_sse_handshake,
)

EXAMPLE_WIRE = (
    b"id: opaque-42\nevent: vehicle.current.changed\n"
    b"data: {\"event_id\":\"opaque-42\"}\n\n"
)
EXPECTED_DISPATCH = SseDispatch(
    event_name="vehicle.current.changed",
    event_id="opaque-42",
    data='{\"event_id\":\"opaque-42\"}',
    content_sha256=(
        "c619c5bb95ba1ab90bd593699283746b3d28cdfba9b67ad81214a2e94b30ca0d"
    ),
)


def parse_all(wire: bytes) -> tuple[object, ...]:
    parser = SseParser()
    return parser.feed_data(wire) + parser.finish()


@pytest.mark.parametrize("split", range(len(EXAMPLE_WIRE) + 1))
def test_every_two_chunk_split_matches_upstream_example(split):
    parser = SseParser()
    items = parser.feed_data(EXAMPLE_WIRE[:split])
    items += parser.feed_data(EXAMPLE_WIRE[split:])
    assert items == (EXPECTED_DISPATCH,)


@pytest.mark.parametrize("ending", [b"\n", b"\r\n", b"\r"])
def test_all_whatwg_line_endings_dispatch(ending):
    wire = ending.join((b"id: opaque", b"event: unknown.future", b"data: {}", b"", b""))
    assert SseParser().feed_data(wire)[0].event_id == "opaque"


@pytest.mark.parametrize(
    ("headers", "accepted"),
    [
        ((("Content-Type", "text/event-stream"), ("Teslatlas-Protocol-Version", "1.2.0")), True),
        ((("content-type", "TEXT/EVENT-STREAM; charset=UTF-8"), ("teslatlas-protocol-version", "1.2.0")), True),
        ((("Content-Type", "application/json"), ("Teslatlas-Protocol-Version", "1.2.0")), False),
        ((("Content-Type", "text/event-stream; charset=latin-1"), ("Teslatlas-Protocol-Version", "1.2.0")), False),
        ((("Content-Type", "text/event-stream"),), False),
        ((("Content-Type", "text/event-stream"), ("Teslatlas-Protocol-Version", "1.1.0")), False),
        ((("Content-Type", "text/event-stream"), ("Content-Type", "text/event-stream"), ("Teslatlas-Protocol-Version", "1.2.0")), False),
    ],
)
def test_sse_handshake_accepts_only_exact_success_contract(
    headers: HeaderFields, accepted: bool
):
    if accepted:
        validate_sse_handshake(status=200, headers=headers, expected_version="1.2.0")
    else:
        with pytest.raises(ProtocolConformanceError):
            validate_sse_handshake(
                status=200, headers=headers, expected_version="1.2.0"
            )


@pytest.mark.parametrize("status", [204, *range(300, 400)])
def test_sse_handshake_terminal_statuses_precede_parser_creation(status):
    called = False

    def parser_factory():
        nonlocal called
        called = True
        return SseParser()

    with pytest.raises(TerminalStreamError):
        validate_sse_handshake(
            status=status,
            headers=(),
            expected_version="1.2.0",
        )
    assert called is False
    assert parser_factory is not None


@pytest.mark.parametrize("split", range(len("£".encode()) + 1))
def test_split_utf8_and_repeated_data_lines_dispatch_once_on_blank_line(split):
    encoded = "£".encode()
    parser = SseParser()
    first = b"id: opaque\nevent: unknown.future\ndata: " + encoded[:split]
    second = encoded[split:] + b"\ndata: two\n\n"
    items = parser.feed_data(first) + parser.feed_data(second)
    assert items[0].data == "£\ntwo"


def test_comments_fields_spaces_reset_and_retry_are_explicit():
    items = parse_all(
        b":live\nignored: field\nid:\nretry: 30001\nretry: +1\n"
        b"id: opaque\nevent: unknown.future\ndata:  one\n\n"
    )
    assert items == (
        SseHeartbeat(),
        SseCheckpointReset(),
        SseRetry(30000),
        SseDispatch(
            "unknown.future",
            "opaque",
            " one",
            "a548421cac5893012490265090ea95f79e837d4178e791eed7e93bf101399db4",
        ),
    )


@pytest.mark.parametrize(
    "wire",
    [
        b"event: unknown.future\ndata: {}\n\n",
        b"id: opaque\ndata: {}\n\n",
        b"id: opaque\nevent: \ndata: {}\n\n",
    ],
)
def test_dispatch_requires_nonempty_id_event_and_present_data(wire):
    with pytest.raises(ProtocolConformanceError):
        parse_all(wire)
    assert parse_all(b"id: opaque\nevent: unknown.future\n\n") == ()


@pytest.mark.parametrize(
    ("wire", "accepted"),
    [
        (b"x" * LINE_LIMIT + b"\n", True),
        (b"x" * (LINE_LIMIT + 1) + b"\n", False),
        (b"data:" + b"x" * (FRAME_LIMIT - 5) + b"\n", True),
        (b"data:" + b"x" * (FRAME_LIMIT - 4) + b"\n", False),
    ],
)
def test_line_and_frame_byte_caps_are_exact(wire, accepted):
    if accepted:
        parse_all(wire)
    else:
        with pytest.raises(ProtocolConformanceError):
            parse_all(wire)


def test_malformed_utf8_is_terminal_and_partial_eof_is_discarded():
    with pytest.raises(ProtocolConformanceError):
        parse_all(b"id: \xff\n")
    assert parse_all(b"id: opaque\nevent: unknown.future\ndata: partial") == ()


def test_sse_errors_recursively_exclude_raw_sentinels():
    sentinel = "RAW_STREAM_SENTINEL"
    with pytest.raises(ProtocolConformanceError) as caught:
        parse_all(b"id: " + sentinel.encode() + b"\ndata: {}\n\n")
    assert sentinel not in str(caught.value)
    assert sentinel not in repr(caught.value)
```

- [ ] **Step 2: Write failing backpressure tests**

```python
class ControlledWaitFor:
    def __init__(self) -> None:
        self.timeouts: list[float] = []

    async def __call__(self, awaitable: Awaitable[None], timeout: float) -> None:
        self.timeouts.append(timeout)
        awaitable.close()
        raise TimeoutError


async def test_full_buffer_waits_then_fails_without_dropping():
    clock = ControlledWaitFor()
    buffer = BoundedEventBuffer(wait_for=clock, capacity=64)
    for item in ITEMS[:64]:
        await buffer.async_put(item)
    with pytest.raises(StreamCapacityError):
        await buffer.async_put(ITEMS[64])
    assert clock.timeouts == [5]
    assert [await buffer.async_get() for _ in range(64)] == ITEMS[:64]


async def test_freed_buffer_slot_admits_exact_waiting_item():
    first, second = SseHeartbeat(), SseRetry(10)
    buffer = BoundedEventBuffer(capacity=1)
    await buffer.async_put(first)
    producer = asyncio.create_task(buffer.async_put(second))
    await asyncio.sleep(0)
    assert await buffer.async_get() is first
    await producer
    assert await buffer.async_get() is second


async def test_buffer_put_cancellation_preserves_all_items():
    first, second = SseHeartbeat(), SseRetry(10)
    buffer = BoundedEventBuffer(capacity=1)
    await buffer.async_put(first)
    producer = asyncio.create_task(buffer.async_put(second))
    await asyncio.sleep(0)
    producer.cancel()
    with pytest.raises(asyncio.CancelledError):
        await producer
    assert await buffer.async_get() is first
    assert buffer.empty()
```

- [ ] **Step 3: Run the focused tests and verify red**

Run: `uv run pytest tests/test_sse.py -q`

Expected: FAIL because `sse.py` is absent.

- [ ] **Step 4: Implement incremental framing and content identity**

`validate_sse_handshake` runs before constructing `SseParser`: status must be 200, exactly one media type must be `text/event-stream`, any declared charset must case-insensitively equal UTF-8, and exactly one `Teslatlas-Protocol-Version` must equal the expected profile. It raises `TerminalStreamError` for 204 and redirects, and `ProtocolConformanceError` for missing/duplicate/malformed success headers. Expected 400/410/429 responses are routed through the bounded problem decoder instead of this success validator.

Maintain one byte buffer and explicit CR/LF state so chunk boundaries cannot change parsing. Decode each complete line strictly as UTF-8 only after the byte cap. Count all accumulated wire-field bytes against the frame cap. A comment yields `SseHeartbeat` immediately and never contributes data. A complete empty `id:` line yields `SseCheckpointReset` immediately, before any blank line, so a following disconnect cannot retain the old cursor. A valid retry line updates retry state without waiting for event dispatch. Blank-line dispatch emits an event only when `id` and `event` were present and non-empty and at least one `data` field was present; otherwise a data-bearing incomplete dispatch raises a safe terminal framing error. A fieldless/comment-only frame produces no dispatch.

Write this complete `sse.py`; it defines every helper used by the public interfaces:

```python
from __future__ import annotations

import asyncio
import hashlib
from collections.abc import Awaitable, Callable
from dataclasses import dataclass

from .errors import ProtocolConformanceError, StreamCapacityError, TerminalStreamError
from .protocol.body import HeaderFields, single_header

LINE_LIMIT = 64 * 1024
FRAME_LIMIT = 1024 * 1024
QUEUE_CAPACITY = 64
QUEUE_DEADLINE_SECONDS = 5.0


@dataclass(frozen=True, slots=True)
class SseDispatch:
    event_name: str
    event_id: str
    data: str
    content_sha256: str


@dataclass(frozen=True, slots=True)
class SseCheckpointReset: ...


@dataclass(frozen=True, slots=True)
class SseRetry:
    milliseconds: int


@dataclass(frozen=True, slots=True)
class SseHeartbeat: ...


type StreamItem = SseDispatch | SseCheckpointReset | SseRetry | SseHeartbeat
type WaitFor = Callable[[Awaitable[None], float], Awaitable[None]]


def _parse_event_stream_content_type(value: str) -> None:
    parts = [part.strip() for part in value.split(";")]
    if not parts or parts[0].lower() != "text/event-stream":
        raise ProtocolConformanceError()
    parameters: dict[str, str] = {}
    for part in parts[1:]:
        name, separator, parameter = part.partition("=")
        key = name.strip().lower()
        if separator != "=" or key != "charset" or key in parameters:
            raise ProtocolConformanceError()
        parameters[key] = parameter.strip().strip('"').lower()
    if "charset" in parameters and parameters["charset"] not in {"utf-8", "utf8"}:
        raise ProtocolConformanceError()


def validate_sse_handshake(
    *, status: int, headers: HeaderFields, expected_version: str
) -> None:
    if status == 204 or 300 <= status <= 399:
        raise TerminalStreamError()
    if status != 200:
        raise ProtocolConformanceError()
    media_type = single_header(headers, "Content-Type", required=True)
    version = single_header(
        headers, "Teslatlas-Protocol-Version", required=True
    )
    if media_type is None or version != expected_version:
        raise ProtocolConformanceError()
    _parse_event_stream_content_type(media_type)


def _content_sha256(event_name: str, event_id: str, data: str) -> str:
    digest = hashlib.sha256()
    for value in (event_name, event_id, data):
        encoded = value.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
    return digest.hexdigest()


class SseParser:
    def __init__(self) -> None:
        self._line = bytearray()
        self._after_cr = False
        self._event_name: str | None = None
        self._event_id: str | None = None
        self._data: list[str] = []
        self._frame_bytes = 0

    def _reset_frame(self) -> None:
        self._event_name = None
        self._event_id = None
        self._data.clear()
        self._frame_bytes = 0

    def _decode_line(self) -> str:
        try:
            return self._line.decode("utf-8")
        except UnicodeDecodeError:
            raise ProtocolConformanceError() from None

    def _finish_line(self) -> tuple[StreamItem, ...]:
        line = self._decode_line()
        encoded_length = len(self._line)
        self._line.clear()
        if line == "":
            if not self._data:
                self._reset_frame()
                return ()
            if not self._event_id or not self._event_name:
                self._reset_frame()
                raise ProtocolConformanceError()
            data = "\n".join(self._data)
            dispatch = SseDispatch(
                self._event_name,
                self._event_id,
                data,
                _content_sha256(self._event_name, self._event_id, data),
            )
            self._reset_frame()
            return (dispatch,)
        if line.startswith(":"):
            return (SseHeartbeat(),)
        name, separator, value = line.partition(":")
        if separator and value.startswith(" "):
            value = value[1:]
        if name not in {"id", "event", "data", "retry"}:
            return ()
        self._frame_bytes += encoded_length
        if self._frame_bytes > FRAME_LIMIT:
            raise ProtocolConformanceError()
        if name == "id":
            if "\x00" in value:
                raise ProtocolConformanceError()
            if value == "":
                self._event_id = None
                return (SseCheckpointReset(),)
            self._event_id = value
        elif name == "event":
            self._event_name = value
        elif name == "data":
            self._data.append(value)
        elif value.isascii() and value.isdecimal():
            return (SseRetry(min(int(value), 30_000)),)
        return ()

    def feed_data(self, chunk: bytes) -> tuple[StreamItem, ...]:
        items: list[StreamItem] = []
        for octet in chunk:
            if self._after_cr:
                self._after_cr = False
                if octet == 0x0A:
                    continue
            if octet == 0x0D:
                items.extend(self._finish_line())
                self._after_cr = True
            elif octet == 0x0A:
                items.extend(self._finish_line())
            else:
                self._line.append(octet)
                if len(self._line) > LINE_LIMIT:
                    raise ProtocolConformanceError()
        return tuple(items)

    def finish(self) -> tuple[StreamItem, ...]:
        self._line.clear()
        self._after_cr = False
        self._reset_frame()
        return ()


class BoundedEventBuffer:
    def __init__(
        self,
        *,
        wait_for: WaitFor = asyncio.wait_for,
        capacity: int = QUEUE_CAPACITY,
    ) -> None:
        self._queue: asyncio.Queue[StreamItem] = asyncio.Queue(maxsize=capacity)
        self._wait_for = wait_for

    async def async_put(self, item: StreamItem) -> None:
        try:
            await self._wait_for(
                self._queue.put(item), QUEUE_DEADLINE_SECONDS
            )
        except TimeoutError:
            raise StreamCapacityError() from None

    async def async_get(self) -> StreamItem:
        return await self._queue.get()

    def empty(self) -> bool:
        return self._queue.empty()
```

- [ ] **Step 5: Run the focused test and verify green**

Run: `uv run pytest tests/test_sse.py -q`

Expected: PASS.

- [ ] **Step 6: Run SSE branch and style gates**

Run: `uv run pytest tests/test_sse.py --cov=custom_components.teslatlas_hub.sse --cov-branch --cov-fail-under=100 -q`

Run: `uv run ruff check custom_components/teslatlas_hub/sse.py tests/test_sse.py`

Expected: PASS at 100% branch coverage.

- [ ] **Step 7: Commit Task 1**

```bash
git add custom_components/teslatlas_hub/sse.py tests/test_sse.py
git commit -m "feat: add bounded SSE framing"
```

### Task 2: Validate event identity, visibility, and projection semantics

**Files:**
- Create: `custom_components/teslatlas_hub/runtime.py`
- Create: `custom_components/teslatlas_hub/protocol/events.py`
- Create: `tests/test_event_validation.py`
- Modify: `custom_components/teslatlas_hub/models.py`

**Interfaces:**

```python
@dataclass(frozen=True, slots=True)
class AcceptedEventIdentity:
    event_id: str
    content_sha256: str


@dataclass(frozen=True, slots=True)
class ReplayCheckpoint:
    binding: SessionBinding
    projection_generation: str
    last_event_id: str | None
    recent: tuple[AcceptedEventIdentity, ...]


@dataclass(frozen=True, slots=True)
class ProjectionCommit:
    binding: SessionBinding
    projection_generation: str
    snapshot: HubSnapshot
    checkpoint: ReplayCheckpoint


@dataclass(frozen=True, slots=True)
class AcceptedEvent:
    identity: AcceptedEventIdentity
    next_snapshot: HubSnapshot
    publish: bool


@dataclass(frozen=True, slots=True)
class DuplicateEvent: ...


@dataclass(frozen=True, slots=True)
class ReconciliationRequired:
    identity: AcceptedEventIdentity
    pending_vehicle_id: str = field(repr=False)


type EventDecision = AcceptedEvent | DuplicateEvent | ReconciliationRequired


def validate_and_apply_event(
    dispatch: SseDispatch, current: ProjectionCommit
) -> EventDecision: ...
```

`ProjectionCommit.__post_init__` rejects any checkpoint binding/generation mismatch and requires `snapshot.info.hub_id`, `api_origin`, `event_origin`, and `protocol_version` to equal its `SessionBinding`. The accepted-identity ring is capped at 256 exact `(event_id, content_sha256)` pairs.

- [ ] **Step 1: Write failing event-decision tests**

```python
import json
from dataclasses import replace

import pytest

from custom_components.teslatlas_hub.errors import ProtocolConformanceError
from custom_components.teslatlas_hub.models import HubSnapshot
from custom_components.teslatlas_hub.runtime import (
    AcceptedEvent,
    AcceptedEventIdentity,
    DuplicateEvent,
    ReconciliationRequired,
    accept_event,
    validate_and_apply_event,
)
from custom_components.teslatlas_hub.sse import SseDispatch
from tests.helpers import initial_protocol_commit, protocol_event


def dispatch(event_name: str, event_id: str, data: str) -> SseDispatch:
    return SseDispatch.from_text(event_name, event_id, data)


@pytest.fixture
def commit():
    return initial_protocol_commit()


def test_unknown_event_is_checkpoint_only_without_json_decode(commit):
    decision = validate_and_apply_event(
        dispatch("future.event", "opaque-z", "NOT JSON"), commit
    )
    assert decision == AcceptedEvent(
        AcceptedEventIdentity("opaque-z", decision.identity.content_sha256),
        commit.snapshot,
        False,
    )
    assert accept_event(commit, decision).checkpoint.last_event_id == "opaque-z"


def test_exact_duplicate_is_noop_and_changed_digest_is_terminal(commit):
    original = dispatch("future.event", "opaque-z", "first")
    accepted = validate_and_apply_event(original, commit)
    advanced = accept_event(commit, accepted)
    assert isinstance(validate_and_apply_event(original, advanced), DuplicateEvent)
    with pytest.raises(ProtocolConformanceError):
        validate_and_apply_event(
            dispatch("future.event", "opaque-z", "different"), advanced
        )


@pytest.mark.parametrize("event_id", ["0000", "-1", "A", "opaque previous"])
def test_opaque_event_ids_are_not_ordered(commit, event_id):
    decision = validate_and_apply_event(
        dispatch("future.event", event_id, "not-json"), commit
    )
    assert accept_event(commit, decision).checkpoint.last_event_id == event_id


@pytest.mark.parametrize(
    ("path", "value"),
    [
        (("event_id",), "other"),
        (("event_type",), "state.changed"),
        (("resource_id",), "other"),
        (("vehicle_id",), "other"),
        (("revision",), 99),
    ],
)
def test_sse_and_envelope_identity_fields_must_match(commit, path, value):
    document = protocol_event("vehicle.current.changed")
    document[path[0]] = value
    raw = json.dumps(document, separators=(",", ":"))
    with pytest.raises(ProtocolConformanceError):
        validate_and_apply_event(
            dispatch("vehicle.current.changed", "event_demo_0042", raw), commit
        )


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
def test_current_event_missing_required_field_is_terminal(commit, field):
    document = protocol_event("vehicle.current.changed")
    del document["data"][field]
    with pytest.raises(ProtocolConformanceError):
        validate_and_apply_event(
            dispatch(
                "vehicle.current.changed",
                document["event_id"],
                json.dumps(document),
            ),
            commit,
        )


@pytest.mark.parametrize("field", ["inside_temperature_c", "outside_temperature_c"])
def test_current_event_optional_temperature_may_be_omitted(commit, field):
    document = protocol_event("vehicle.current.changed")
    del document["data"][field]
    decision = validate_and_apply_event(
        dispatch(document["event_type"], document["event_id"], json.dumps(document)),
        commit,
    )
    assert getattr(decision.next_snapshot.vehicles[document["vehicle_id"]], field) is None


@pytest.mark.parametrize("constant", ["NaN", "Infinity", "-Infinity"])
def test_known_event_rejects_nonfinite_json_without_checkpoint(commit, constant):
    raw = json.dumps(protocol_event("vehicle.current.changed")).replace("78", constant, 1)
    with pytest.raises(ProtocolConformanceError):
        validate_and_apply_event(
            dispatch("vehicle.current.changed", "event_demo_0042", raw), commit
        )
    assert commit.checkpoint.last_event_id is None


def test_current_event_preserves_catalog_name_and_replaces_complete_state(commit):
    document = protocol_event("vehicle.current.changed")
    decision = validate_and_apply_event(
        dispatch(document["event_type"], document["event_id"], json.dumps(document)),
        commit,
    )
    vehicle = decision.next_snapshot.vehicles[document["vehicle_id"]]
    assert vehicle.name == commit.snapshot.vehicles[document["vehicle_id"]].name
    assert vehicle.battery_level_percent == document["data"]["battery_level_percent"]
    assert decision.publish is True


def test_known_vehicle_quality_with_null_vehicle_id_changes_only_quality(commit):
    document = protocol_event("data_quality.changed")
    vehicle_before = commit.snapshot.vehicles[document["data"]["subject_id"]]
    decision = validate_and_apply_event(
        dispatch(document["event_type"], document["event_id"], json.dumps(document)),
        commit,
    )
    vehicle_after = decision.next_snapshot.vehicles[document["data"]["subject_id"]]
    assert vehicle_after == replace(vehicle_before, quality=document["data"]["quality"])


def test_unknown_vehicle_quality_requires_private_reconciliation(commit):
    document = protocol_event("data_quality.changed")
    document["resource_id"] = document["data"]["subject_id"] = "vehicle_hidden"
    decision = validate_and_apply_event(
        dispatch(document["event_type"], document["event_id"], json.dumps(document)),
        commit,
    )
    assert isinstance(decision, ReconciliationRequired)
    assert decision.pending_vehicle_id == "vehicle_hidden"
    assert "vehicle_hidden" not in repr(decision)
    assert commit.checkpoint.last_event_id is None


def test_quality_resource_subject_mismatch_is_terminal(commit):
    document = protocol_event("data_quality.changed")
    document["resource_id"] = "different"
    with pytest.raises(ProtocolConformanceError):
        validate_and_apply_event(
            dispatch(document["event_type"], document["event_id"], json.dumps(document)),
            commit,
        )


@pytest.mark.parametrize("name", ["drive.updated", "command.changed", "metadata.changed"])
def test_known_unprojected_event_validates_then_checkpoints_only(commit, name):
    document = protocol_event(name)
    decision = validate_and_apply_event(
        dispatch(name, document["event_id"], json.dumps(document)), commit
    )
    assert decision.publish is False
    assert decision.next_snapshot is commit.snapshot


def test_unknown_vehicle_requires_reconciliation_without_checkpoint(commit):
    document = protocol_event("vehicle.current.changed")
    document["vehicle_id"] = document["resource_id"] = "vehicle_hidden"
    document["data"]["vehicle_id"] = "vehicle_hidden"
    document["data"]["quality"]["subject_id"] = "vehicle_hidden"
    decision = validate_and_apply_event(
        dispatch(document["event_type"], document["event_id"], json.dumps(document)),
        commit,
    )
    assert isinstance(decision, ReconciliationRequired)
    assert "vehicle_hidden" not in repr(decision)


def test_event_failures_exclude_hidden_sentinels(commit):
    sentinel = "HIDDEN_RESOURCE_SENTINEL"
    document = protocol_event("vehicle.current.changed")
    document["resource_id"] = sentinel
    raw = json.dumps(document)
    with pytest.raises(ProtocolConformanceError) as caught:
        validate_and_apply_event(
            dispatch(document["event_type"], document["event_id"], raw), commit
        )
    assert sentinel not in str(caught.value)
    assert sentinel not in repr(caught.value)
```

- [ ] **Step 2: Run the focused tests and verify red**

Run: `uv run pytest tests/test_event_validation.py -q`

Expected: FAIL because runtime/event-validation types are absent.

- [ ] **Step 3: Implement profile-catalogue selection before decoding**

```python
from __future__ import annotations

from dataclasses import dataclass, field, replace
from datetime import datetime

from .errors import ProtocolConformanceError
from .models import HubSnapshot, SessionBinding, VehicleState
from .protocol.body import strict_load_json_object
from .protocol.schema import validate_document, validate_event_semantics
from .sse import SseDispatch

PROFILE_EVENT_NAMES = frozenset(
    {
        "observation.admitted",
        "vehicle.current.changed",
        "drive.started",
        "drive.updated",
        "drive.ended",
        "charge.started",
        "charge.updated",
        "charge.ended",
        "state.changed",
        "software_update.changed",
        "data_quality.changed",
        "command.changed",
        "metadata.changed",
    }
)
EVENT_SCHEMA = "urn:teslatlas:protocol:schema:event:1.2.0"


@dataclass(frozen=True, slots=True)
class AcceptedEventIdentity:
    event_id: str
    content_sha256: str


@dataclass(frozen=True, slots=True)
class ReplayCheckpoint:
    binding: SessionBinding
    projection_generation: str
    last_event_id: str | None
    recent: tuple[AcceptedEventIdentity, ...]


@dataclass(frozen=True, slots=True)
class ProjectionCommit:
    binding: SessionBinding
    projection_generation: str
    snapshot: HubSnapshot
    checkpoint: ReplayCheckpoint

    def __post_init__(self) -> None:
        info = self.snapshot.info
        if (
            self.checkpoint.binding != self.binding
            or self.checkpoint.projection_generation != self.projection_generation
            or info.hub_id != self.binding.hub_id
            or info.api_origin != self.binding.api_origin
            or info.event_origin != self.binding.event_origin
            or info.protocol_version != self.binding.protocol_version
            or len(self.checkpoint.recent) > 256
        ):
            raise ProtocolConformanceError()


@dataclass(frozen=True, slots=True)
class AcceptedEvent:
    identity: AcceptedEventIdentity
    next_snapshot: HubSnapshot
    publish: bool


@dataclass(frozen=True, slots=True)
class DuplicateEvent: ...


@dataclass(frozen=True, slots=True)
class ReconciliationRequired:
    identity: AcceptedEventIdentity
    pending_vehicle_id: str = field(repr=False)


type EventDecision = AcceptedEvent | DuplicateEvent | ReconciliationRequired


def _identity(dispatch: SseDispatch) -> AcceptedEventIdentity:
    return AcceptedEventIdentity(dispatch.event_id, dispatch.content_sha256)


def _check_duplicate(
    identity: AcceptedEventIdentity, current: ProjectionCommit
) -> DuplicateEvent | None:
    for accepted in current.checkpoint.recent:
        if accepted.event_id != identity.event_id:
            continue
        if accepted == identity:
            return DuplicateEvent()
        raise ProtocolConformanceError()
    return None


def _vehicle_from_current(data: dict[str, object], *, name: str) -> VehicleState:
    return VehicleState(
        vehicle_id=str(data["vehicle_id"]),
        name=name,
        observed_at=datetime.fromisoformat(str(data["observed_at"]).replace("Z", "+00:00")),
        revision=int(data["revision"]),
        state=str(data["state"]),
        battery_level_percent=data["battery_level_percent"],
        range_km=data["range_km"],
        odometer_km=data["odometer_km"],
        inside_temperature_c=data.get("inside_temperature_c"),
        outside_temperature_c=data.get("outside_temperature_c"),
        locked=data["locked"],
        climate_on=data["climate_on"],
        charging_state=data["charging_state"],
        quality=str(data["quality"]["quality"]),
    )


def _replace_vehicle(snapshot: HubSnapshot, vehicle: VehicleState) -> HubSnapshot:
    vehicles = dict(snapshot.vehicles)
    vehicles[vehicle.vehicle_id] = vehicle
    return HubSnapshot.create(info=snapshot.info, vehicles=vehicles.values())


def _visibility_target(document: dict[str, object]) -> str | None:
    if document["event_type"] == "data_quality.changed":
        data = document["data"]
        if data["subject_type"] == "vehicle":
            if document["resource_id"] != data["subject_id"]:
                raise ProtocolConformanceError()
            return str(data["subject_id"])
    value = document["vehicle_id"]
    return None if value is None else str(value)


def validate_and_apply_event(
    dispatch: SseDispatch, current: ProjectionCommit
) -> EventDecision:
    identity = _identity(dispatch)
    duplicate = _check_duplicate(identity, current)
    if duplicate is not None:
        return duplicate
    if dispatch.event_name not in PROFILE_EVENT_NAMES:
        return AcceptedEvent(identity, current.snapshot, False)
    document = strict_load_json_object(dispatch.data)
    validate_document(EVENT_SCHEMA, document)
    validate_event_semantics(document)
    if (
        document["event_id"] != dispatch.event_id
        or document["event_type"] != dispatch.event_name
    ):
        raise ProtocolConformanceError()
    target = _visibility_target(document)
    if target is not None and target not in current.snapshot.vehicles:
        return ReconciliationRequired(identity, target)
    if dispatch.event_name == "vehicle.current.changed":
        before = current.snapshot.vehicles[target]
        vehicle = _vehicle_from_current(document["data"], name=before.name)
        return AcceptedEvent(identity, _replace_vehicle(current.snapshot, vehicle), True)
    if dispatch.event_name == "data_quality.changed" and target is not None:
        before = current.snapshot.vehicles[target]
        vehicle = replace(before, quality=str(document["data"]["quality"]))
        return AcceptedEvent(identity, _replace_vehicle(current.snapshot, vehicle), True)
    return AcceptedEvent(identity, current.snapshot, False)


def accept_event(current: ProjectionCommit, event: AcceptedEvent) -> ProjectionCommit:
    recent = (*current.checkpoint.recent, event.identity)[-256:]
    checkpoint = replace(
        current.checkpoint,
        last_event_id=event.identity.event_id,
        recent=recent,
    )
    return ProjectionCommit(
        current.binding,
        current.projection_generation,
        event.next_snapshot,
        checkpoint,
    )


def reset_checkpoint(current: ProjectionCommit) -> ProjectionCommit:
    return replace(
        current,
        checkpoint=replace(current.checkpoint, last_event_id=None),
    )
```

- [ ] **Step 4: Run event tests and verify green**

Run: `uv run pytest tests/test_event_validation.py tests/test_protocol_schema.py -q`

Expected: PASS.

- [ ] **Step 5: Run event style gate**

Run: `uv run ruff check custom_components/teslatlas_hub/runtime.py custom_components/teslatlas_hub/protocol/events.py tests/test_event_validation.py`

Expected: PASS.

- [ ] **Step 6: Commit Task 2**

```bash
git add custom_components/teslatlas_hub/runtime.py custom_components/teslatlas_hub/protocol/events.py custom_components/teslatlas_hub/models.py tests/test_event_validation.py
git commit -m "feat: validate event visibility and projection"
```

### Task 3: Persist projection and replay checkpoint atomically

**Files:**
- Create: `custom_components/teslatlas_hub/checkpoint.py`
- Create: `tests/test_checkpoint.py`

**Interfaces:**

```python
class ProjectionCheckpointStore:
    def __init__(self, hass, entry_id: str, *, store=None) -> None: ...
    async def async_load(
        self, *, expected_hub_id: str, expected_credential_generation: int
    ) -> ProjectionCommit | None: ...
    async def async_commit(self, commit: ProjectionCommit) -> None: ...
```

The wrapper uses `Store(hass, 1, f"teslatlas_hub.{entry_id}.runtime", private=True, atomic_writes=True)`. One strict JSON object holds schema version, binding, projection generation, allow-listed snapshot, checkpoint, and recent content digests.

- [ ] **Step 1: Write failing atomic-storage tests**

```python
import copy
from dataclasses import replace

import pytest

from custom_components.teslatlas_hub.checkpoint import ProjectionCheckpointStore
from custom_components.teslatlas_hub.errors import StorageError
from custom_components.teslatlas_hub.runtime import (
    AcceptedEvent,
    AcceptedEventIdentity,
    accept_event,
    reset_checkpoint,
)
from tests.helpers import initial_protocol_commit, updated_protocol_snapshot


class MemoryStore:
    def __init__(self, document=None) -> None:
        self.document = copy.deepcopy(document)
        self.saved: list[dict[str, object]] = []
        self.load_error: Exception | None = None
        self.save_error: Exception | None = None

    async def async_load(self):
        if self.load_error is not None:
            raise self.load_error
        return copy.deepcopy(self.document)

    async def async_save(self, document):
        if self.save_error is not None:
            raise self.save_error
        self.document = copy.deepcopy(document)
        self.saved.append(copy.deepcopy(document))


def make_store(hass, backend=None):
    return ProjectionCheckpointStore(
        hass, "entry-id", store=backend if backend is not None else MemoryStore()
    )


def expected_load_kwargs(commit):
    return {
        "expected_hub_id": commit.binding.hub_id,
        "expected_credential_generation": commit.binding.credential_generation,
    }


def projected_event(commit):
    identity = AcceptedEventIdentity("opaque-next", "0" * 64)
    event = AcceptedEvent(identity, updated_protocol_snapshot(), True)
    return accept_event(commit, event)


async def test_projected_event_is_one_projection_checkpoint_write(store, commit):
    next_commit = projected_event(commit)
    await store.async_commit(next_commit)
    restored = await store.async_load(**expected_load_kwargs(next_commit))
    assert restored == next_commit
    assert restored.snapshot.vehicles[VEHICLE_ID].battery_level_percent == 71
    assert restored.checkpoint.last_event_id == "opaque-next"


async def test_store_strict_round_trip(hass):
    commit = initial_protocol_commit()
    backend = MemoryStore()
    store = make_store(hass, backend)
    await store.async_commit(commit)
    assert await store.async_load(**expected_load_kwargs(commit)) == commit
    assert len(backend.saved) == 1


async def test_checkpoint_only_and_empty_reset_keep_one_atomic_document(hass):
    commit = initial_protocol_commit()
    store = make_store(hass)
    accepted = accept_event(
        commit,
        AcceptedEvent(
            AcceptedEventIdentity("opaque-only", "1" * 64),
            commit.snapshot,
            False,
        ),
    )
    await store.async_commit(accepted)
    cleared = reset_checkpoint(accepted)
    await store.async_commit(cleared)
    restored = await store.async_load(**expected_load_kwargs(commit))
    assert restored.snapshot == commit.snapshot
    assert restored.projection_generation == commit.projection_generation
    assert restored.checkpoint.last_event_id is None


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("hub_id", "other"),
        ("credential_generation", 2),
        ("api_origin", "https://other.invalid"),
        ("event_origin", "https://other.invalid"),
        ("protocol_version", "1.1.0"),
        ("normalized_filters", [["vehicle", "other"]]),
    ],
)
async def test_store_binding_mismatch_returns_none(hass, field, value):
    commit = initial_protocol_commit()
    backend = MemoryStore()
    store = make_store(hass, backend)
    await store.async_commit(commit)
    backend.document["binding"][field] = value
    assert await store.async_load(**expected_load_kwargs(commit)) is None


@pytest.mark.parametrize(
    "mutation",
    [
        lambda value: value.pop("checkpoint"),
        lambda value: value.pop("snapshot"),
        lambda value: value.update({"extra": True}),
        lambda value: value["checkpoint"].update({"projection_generation": "other"}),
        lambda value: value["checkpoint"].update({"recent": [{"event_id": "raw"}]}),
        lambda value: value["snapshot"]["vehicles"][0].update({"extra": True}),
    ],
)
async def test_store_rejects_orphan_or_extra_fields(hass, mutation):
    commit = initial_protocol_commit()
    backend = MemoryStore()
    store = make_store(hass, backend)
    await store.async_commit(commit)
    mutation(backend.document)
    assert await store.async_load(**expected_load_kwargs(commit)) is None


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("observed_at", "not-a-timestamp"),
        ("battery_level_percent", float("nan")),
        ("quality", "invented"),
    ],
)
async def test_store_rejects_nonfinite_malformed_timestamp_and_quality(
    hass, field, value
):
    commit = initial_protocol_commit()
    backend = MemoryStore()
    store = make_store(hass, backend)
    await store.async_commit(commit)
    backend.document["snapshot"]["vehicles"][0][field] = value
    assert await store.async_load(**expected_load_kwargs(commit)) is None


async def test_corrupt_store_returns_none_without_publication(hass):
    commit = initial_protocol_commit()
    store = make_store(hass, MemoryStore({"schema_version": 1}))
    assert await store.async_load(**expected_load_kwargs(commit)) is None


async def test_failed_atomic_save_is_safe_and_preserves_last_good(hass):
    commit = initial_protocol_commit()
    backend = MemoryStore()
    store = make_store(hass, backend)
    await store.async_commit(commit)
    last_good = copy.deepcopy(backend.document)
    backend.save_error = OSError("RAW_PATH_SENTINEL")
    with pytest.raises(StorageError) as caught:
        await store.async_commit(projected_event(commit))
    assert backend.document == last_good
    assert "RAW_PATH_SENTINEL" not in str(caught.value)
    assert "RAW_PATH_SENTINEL" not in repr(caught.value)


async def test_store_repr_never_renders_event_identity(hass):
    commit = projected_event(initial_protocol_commit())
    backend = MemoryStore()
    store = make_store(hass, backend)
    await store.async_commit(commit)
    assert commit.checkpoint.last_event_id not in repr(store)
    assert commit.checkpoint.recent[0].content_sha256 not in repr(store)
```

- [ ] **Step 2: Run the focused tests and verify red**

Run: `uv run pytest tests/test_checkpoint.py -q`

Expected: FAIL because `checkpoint.py` is absent.

- [ ] **Step 3: Implement strict serialization and one-file commits**

Use explicit `hub_snapshot_to_dict`/`parse_stored_hub_snapshot` helpers; never serialize dataclasses with `asdict`, raw events, or runtime dictionaries. An absent file, invalid JSON/document, unknown field, or Hub/credential/binding/generation mismatch returns `None` and publishes nothing. An underlying storage read/write I/O failure raises safe `StorageError` with no raw path or exception text. Stored origins/profile/filters are never fresh negotiation proof; full binding freshness is confirmed only by a successful client resume/session handshake. `async_commit` awaits the single atomic save before the caller mutates in-memory state.

Use this complete module. Every stored object has an exact key set; the parsers reject booleans where integers/numbers are required, non-finite numbers, non-UTC timestamps, duplicate vehicle IDs, duplicate recent IDs, and every model/binding inconsistency.

```python
from __future__ import annotations

import math
from datetime import datetime, timezone

from homeassistant.helpers.storage import Store

from .errors import ProtocolConformanceError, StorageError
from .models import (
    CapabilityIdentity,
    HubInfo,
    HubSnapshot,
    ProtocolLimits,
    SessionBinding,
    VehicleState,
)
from .runtime import AcceptedEventIdentity, ProjectionCommit, ReplayCheckpoint

SCHEMA_VERSION = 1
QUALITY_VALUES = frozenset({"complete", "partial", "degraded"})


def _exact(value: object, keys: set[str]) -> dict[str, object]:
    if not isinstance(value, dict) or set(value) != keys:
        raise ValueError
    return value


def _string(value: object) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError
    return value


def _integer(value: object, *, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ValueError
    return value


def _number_or_none(value: object) -> float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int | float):
        raise ValueError
    result = float(value)
    if not math.isfinite(result):
        raise ValueError
    return result


def _bool_or_none(value: object) -> bool | None:
    if value is not None and not isinstance(value, bool):
        raise ValueError
    return value


def _timestamp(value: object) -> datetime:
    text = _string(value)
    if not text.endswith("Z"):
        raise ValueError
    result = datetime.fromisoformat(text[:-1] + "+00:00")
    if result.tzinfo != timezone.utc:
        raise ValueError
    return result


def _timestamp_text(value: datetime) -> str:
    if value.tzinfo is None or value.utcoffset() != timezone.utc.utcoffset(value):
        raise ProtocolConformanceError()
    return value.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def _binding_to_dict(binding: SessionBinding) -> dict[str, object]:
    return {
        "hub_id": binding.hub_id,
        "api_origin": binding.api_origin,
        "event_origin": binding.event_origin,
        "protocol_version": binding.protocol_version,
        "normalized_filters": [list(item) for item in binding.normalized_filters],
        "credential_generation": binding.credential_generation,
    }


def _parse_binding(value: object) -> SessionBinding:
    value = _exact(
        value,
        {
            "hub_id",
            "api_origin",
            "event_origin",
            "protocol_version",
            "normalized_filters",
            "credential_generation",
        },
    )
    filters = value["normalized_filters"]
    if not isinstance(filters, list):
        raise ValueError
    parsed_filters: list[tuple[str, str]] = []
    for item in filters:
        if not isinstance(item, list) or len(item) != 2:
            raise ValueError
        parsed_filters.append((_string(item[0]), _string(item[1])))
    if parsed_filters != sorted(set(parsed_filters)):
        raise ValueError
    return SessionBinding(
        _string(value["hub_id"]),
        _string(value["api_origin"]),
        _string(value["event_origin"]),
        _string(value["protocol_version"]),
        tuple(parsed_filters),
        _integer(value["credential_generation"], minimum=1),
    )


def _info_to_dict(info: HubInfo) -> dict[str, object]:
    return {
        "hub_id": info.hub_id,
        "protocol_version": info.protocol_version,
        "api_origin": info.api_origin,
        "event_origin": info.event_origin,
        "capabilities": [
            {"id": item.capability_id, "version": item.version}
            for item in info.capabilities
        ],
        "limits": {
            "default_page_size": info.limits.default_page_size,
            "max_page_size": info.limits.max_page_size,
            "max_concurrent_requests": info.limits.max_concurrent_requests,
            "max_sse_connections": info.limits.max_sse_connections,
            "event_replay_retention_seconds": info.limits.event_replay_retention_seconds,
        },
    }


def _parse_info(value: object) -> HubInfo:
    value = _exact(
        value,
        {"hub_id", "protocol_version", "api_origin", "event_origin", "capabilities", "limits"},
    )
    capabilities = value["capabilities"]
    if not isinstance(capabilities, list):
        raise ValueError
    parsed_capabilities = tuple(
        CapabilityIdentity(
            _string(_exact(item, {"id", "version"})["id"]),
            _string(_exact(item, {"id", "version"})["version"]),
        )
        for item in capabilities
    )
    limits = _exact(
        value["limits"],
        {
            "default_page_size",
            "max_page_size",
            "max_concurrent_requests",
            "max_sse_connections",
            "event_replay_retention_seconds",
        },
    )
    return HubInfo(
        hub_id=_string(value["hub_id"]),
        protocol_version=_string(value["protocol_version"]),
        capabilities=parsed_capabilities,
        api_origin=_string(value["api_origin"]),
        event_origin=_string(value["event_origin"]),
        limits=ProtocolLimits(
            *(_integer(limits[name], minimum=1) for name in (
                "default_page_size",
                "max_page_size",
                "max_concurrent_requests",
                "max_sse_connections",
                "event_replay_retention_seconds",
            ))
        ),
    )


VEHICLE_KEYS = {
    "vehicle_id", "name", "observed_at", "revision", "state",
    "battery_level_percent", "range_km", "odometer_km",
    "inside_temperature_c", "outside_temperature_c", "locked", "climate_on",
    "charging_state", "quality",
}


def _vehicle_to_dict(vehicle: VehicleState) -> dict[str, object]:
    return {
        "vehicle_id": vehicle.vehicle_id,
        "name": vehicle.name,
        "observed_at": _timestamp_text(vehicle.observed_at),
        "revision": vehicle.revision,
        "state": vehicle.state,
        "battery_level_percent": vehicle.battery_level_percent,
        "range_km": vehicle.range_km,
        "odometer_km": vehicle.odometer_km,
        "inside_temperature_c": vehicle.inside_temperature_c,
        "outside_temperature_c": vehicle.outside_temperature_c,
        "locked": vehicle.locked,
        "climate_on": vehicle.climate_on,
        "charging_state": vehicle.charging_state,
        "quality": vehicle.quality,
    }


def _parse_vehicle(value: object) -> VehicleState:
    value = _exact(value, VEHICLE_KEYS)
    quality = _string(value["quality"])
    if quality not in QUALITY_VALUES:
        raise ValueError
    charging = value["charging_state"]
    if charging is not None and charging not in {
        "charging", "stopped", "complete", "disconnected", "unknown"
    }:
        raise ValueError
    return VehicleState(
        vehicle_id=_string(value["vehicle_id"]),
        name=_string(value["name"]),
        observed_at=_timestamp(value["observed_at"]),
        revision=_integer(value["revision"], minimum=1),
        state=_string(value["state"]),
        battery_level_percent=_number_or_none(value["battery_level_percent"]),
        range_km=_number_or_none(value["range_km"]),
        odometer_km=_number_or_none(value["odometer_km"]),
        inside_temperature_c=_number_or_none(value["inside_temperature_c"]),
        outside_temperature_c=_number_or_none(value["outside_temperature_c"]),
        locked=_bool_or_none(value["locked"]),
        climate_on=_bool_or_none(value["climate_on"]),
        charging_state=charging,
        quality=quality,
    )


def _commit_to_document(commit: ProjectionCommit) -> dict[str, object]:
    return {
        "schema_version": SCHEMA_VERSION,
        "binding": _binding_to_dict(commit.binding),
        "projection_generation": commit.projection_generation,
        "snapshot": {
            "info": _info_to_dict(commit.snapshot.info),
            "vehicles": [
                _vehicle_to_dict(commit.snapshot.vehicles[key])
                for key in sorted(commit.snapshot.vehicles)
            ],
        },
        "checkpoint": {
            "binding": _binding_to_dict(commit.checkpoint.binding),
            "projection_generation": commit.checkpoint.projection_generation,
            "last_event_id": commit.checkpoint.last_event_id,
            "recent": [
                {"event_id": item.event_id, "content_sha256": item.content_sha256}
                for item in commit.checkpoint.recent
            ],
        },
    }


def _parse_document(value: object) -> ProjectionCommit:
    value = _exact(
        value,
        {"schema_version", "binding", "projection_generation", "snapshot", "checkpoint"},
    )
    if value["schema_version"] != SCHEMA_VERSION:
        raise ValueError
    binding = _parse_binding(value["binding"])
    generation = _string(value["projection_generation"])
    snapshot_value = _exact(value["snapshot"], {"info", "vehicles"})
    vehicles_value = snapshot_value["vehicles"]
    if not isinstance(vehicles_value, list):
        raise ValueError
    snapshot = HubSnapshot.create(
        info=_parse_info(snapshot_value["info"]),
        vehicles=(_parse_vehicle(item) for item in vehicles_value),
    )
    checkpoint_value = _exact(
        value["checkpoint"],
        {"binding", "projection_generation", "last_event_id", "recent"},
    )
    last_id = checkpoint_value["last_event_id"]
    if last_id is not None:
        last_id = _string(last_id)
    recent_value = checkpoint_value["recent"]
    if not isinstance(recent_value, list) or len(recent_value) > 256:
        raise ValueError
    recent = tuple(
        AcceptedEventIdentity(
            _string(_exact(item, {"event_id", "content_sha256"})["event_id"]),
            _string(_exact(item, {"event_id", "content_sha256"})["content_sha256"]),
        )
        for item in recent_value
    )
    if len({item.event_id for item in recent}) != len(recent):
        raise ValueError
    checkpoint = ReplayCheckpoint(
        _parse_binding(checkpoint_value["binding"]),
        _string(checkpoint_value["projection_generation"]),
        last_id,
        recent,
    )
    return ProjectionCommit(binding, generation, snapshot, checkpoint)


class ProjectionCheckpointStore:
    def __init__(self, hass, entry_id: str, *, store=None) -> None:
        self._store = store or Store(
            hass,
            SCHEMA_VERSION,
            f"teslatlas_hub.{entry_id}.runtime",
            private=True,
            atomic_writes=True,
        )

    async def async_load(
        self, *, expected_hub_id: str, expected_credential_generation: int
    ) -> ProjectionCommit | None:
        try:
            value = await self._store.async_load()
        except Exception as err:
            raise StorageError() from None
        if value is None:
            return None
        try:
            commit = _parse_document(value)
            if (
                commit.binding.hub_id != expected_hub_id
                or commit.binding.credential_generation != expected_credential_generation
            ):
                return None
            return commit
        except (KeyError, TypeError, ValueError, ProtocolConformanceError):
            return None

    async def async_commit(self, commit: ProjectionCommit) -> None:
        document = _commit_to_document(commit)
        _parse_document(document)
        try:
            await self._store.async_save(document)
        except Exception:
            raise StorageError() from None
```

`asyncio.CancelledError` derives from `BaseException`, so neither backend wrapper catches it. Keep both `except Exception` blocks scoped to the single backend await; parser exceptions remain the `None` path.

- [ ] **Step 4: Run store tests and verify green**

Run: `uv run pytest tests/test_checkpoint.py -q`

Expected: PASS.

- [ ] **Step 5: Run store branch gate**

Run: `uv run pytest tests/test_checkpoint.py --cov=custom_components.teslatlas_hub.checkpoint --cov-branch --cov-fail-under=100 -q`

Expected: PASS at 100% branch coverage.

- [ ] **Step 6: Commit Task 3**

```bash
git add custom_components/teslatlas_hub/checkpoint.py tests/test_checkpoint.py
git commit -m "feat: persist projection checkpoints atomically"
```

### Task 4: Replace racing client methods with fixture-only synchronized sessions

**Files:**
- Modify: `custom_components/teslatlas_hub/client.py`
- Modify: `tests/helpers.py`
- Modify: `tests/test_client.py`

**Interfaces:**

```python
class EventSession(Protocol):
    binding: SessionBinding

    def __aiter__(self) -> AsyncIterator[StreamItem]: ...
    async def async_wait_failure(self) -> HubClientError: ...
    async def async_close(self) -> None: ...


@dataclass(frozen=True, slots=True)
class SynchronizedSession:
    snapshot: HubSnapshot
    boundary_event_id: str | None
    binding: SessionBinding
    events: EventSession


class TeslatlasHubClient(Protocol):
    async def async_open_synchronized_session(self) -> SynchronizedSession: ...
    async def async_resume_events(
        self, checkpoint: ReplayCheckpoint
    ) -> EventSession: ...
```

Delete the public `async_snapshot` plus `async_events(last_event_id)` pairing from the client protocol so new code cannot reintroduce the race. Query-candidate helpers remain internal contract tests, not a runnable production lifecycle.

- [ ] **Step 1: Write failing synchronized-boundary tests**

```python
import asyncio
import json

import pytest

from custom_components.teslatlas_hub.client import (
    DeploymentNotReadyError,
    EventSession,
    SynchronizedSession,
    create_client,
)
from custom_components.teslatlas_hub.errors import (
    EventIdInvalidError,
    RateLimitedError,
    ReplayExpiredError,
    StreamCapacityError,
)
from custom_components.teslatlas_hub.models import HubEndpoint
from custom_components.teslatlas_hub.protocol.body import JsonResponse
from tests.helpers import FixtureHubClient, async_chunks, initial_protocol_commit


def event_response(
    *, status=200, content_type="text/event-stream", version="1.2.0", body=None
):
    headers = (
        ("Content-Type", content_type),
        ("Teslatlas-Protocol-Version", version),
    )
    if status == 429:
        headers += (("Retry-After", "7"),)
    return JsonResponse(
        status,
        headers,
        async_chunks(json.dumps(body).encode() if body is not None else b""),
    )


async def test_fixture_open_returns_snapshot_boundary_binding_and_open_stream():
    session = await FixtureHubClient().async_open_synchronized_session()
    assert session.snapshot.info.hub_id == session.binding.hub_id
    assert session.snapshot.info.api_origin == session.binding.api_origin
    assert session.snapshot.info.event_origin == session.binding.event_origin
    assert session.snapshot.info.protocol_version == session.binding.protocol_version
    assert session.events.binding == session.binding
    assert session.boundary_event_id == "fixture-boundary"


async def test_pending_client_repr_and_methods_leak_nothing(endpoint):
    client = create_client(endpoint, bearer_token="PRIVATE_BEARER")
    with pytest.raises(DeploymentNotReadyError):
        await client.async_open_synchronized_session()
    rendered = repr(client)
    assert rendered == "_PendingProtocolClient(use_tls=True)"


async def test_resume_receives_exact_opaque_checkpoint_and_requires_same_binding():
    commit = initial_protocol_commit(last_event_id="opaque: z/1")
    client = FixtureHubClient(binding=commit.binding)
    session = await client.async_resume_events(commit.checkpoint)
    assert client.resume_checkpoints == [commit.checkpoint]
    assert session.binding == commit.binding
    with pytest.raises(DeploymentNotReadyError):
        await client.async_resume_events(
            replace(
                commit.checkpoint,
                binding=replace(commit.binding, hub_id="other"),
            )
        )


async def test_fixture_owns_one_open_stream_and_close_is_idempotent():
    client = FixtureHubClient()
    session = await client.async_open_synchronized_session()
    with pytest.raises(AssertionError):
        await client.async_open_synchronized_session()
    await session.events.async_close()
    await session.events.async_close()
    assert session.events.close_count == 1


async def test_fixture_session_failure_waiter_is_safe_and_close_cancels_waiter():
    client = FixtureHubClient()
    session = await client.async_open_synchronized_session()
    waiter = asyncio.create_task(session.events.async_wait_failure())
    await asyncio.sleep(0)
    assert waiter.done() is False
    session.events.fail(StreamCapacityError())
    assert type(await waiter) is StreamCapacityError
    second = client.new_event_session()
    waiter = asyncio.create_task(second.async_wait_failure())
    await second.async_close()
    with pytest.raises(asyncio.CancelledError):
        await waiter


async def test_pending_resume_fails_without_transport_or_sensitive_repr():
    commit = initial_protocol_commit(last_event_id="PRIVATE_CHECKPOINT")
    client = create_client(
        HubEndpoint("PRIVATE_HOST", 7443, True), bearer_token="PRIVATE_BEARER"
    )
    with pytest.raises(DeploymentNotReadyError):
        await client.async_resume_events(commit.checkpoint)
    rendered = repr(client)
    assert rendered == "_PendingProtocolClient(use_tls=True)"
    assert "PRIVATE" not in rendered


@pytest.mark.parametrize(
    ("status", "code", "error_type"),
    [
        (400, "event_id_invalid", EventIdInvalidError),
        (410, "event_replay_expired", ReplayExpiredError),
        (429, "rate_limited", RateLimitedError),
    ],
)
async def test_fixture_handshake_problem_is_bounded_and_typed(
    status, code, error_type
):
    body = {
        "type": "about:blank",
        "title": "safe",
        "status": status,
        "code": code,
        "detail": "ignored",
        "instance": "/ignored",
        "request_id": "ignored",
        "retryable": status == 429,
    }
    if status == 429:
        body["retry_after_seconds"] = 7
    client = FixtureHubClient(open_response=event_response(status=status, body=body))
    with pytest.raises(error_type):
        await client.async_open_synchronized_session()


async def test_fixture_success_validates_handshake_before_iterator_exposure():
    client = FixtureHubClient(
        open_response=event_response(content_type="application/json")
    )
    with pytest.raises(ProtocolConformanceError):
        await client.async_open_synchronized_session()
    assert client.exposed_sessions == []
```

- [ ] **Step 2: Run client tests and verify red**

Run: `uv run pytest tests/test_client.py -q`

Expected: FAIL because the old race-prone client methods remain.

- [ ] **Step 3: Change the protocol and fixture only**

Replace the old race-prone methods in `client.py` with these exact interfaces and pending methods; retain the contract-plan probe/pair/access methods unchanged above this block:

```python
class EventSession(Protocol):
    binding: SessionBinding

    def __aiter__(self) -> AsyncIterator[StreamItem]:
        return self

    async def __anext__(self) -> StreamItem: ...
    async def async_wait_failure(self) -> HubClientError: ...
    async def async_close(self) -> None: ...


@dataclass(frozen=True, slots=True)
class SynchronizedSession:
    snapshot: HubSnapshot
    boundary_event_id: str | None
    binding: SessionBinding
    events: EventSession


class TeslatlasHubClient(Protocol):
    async def async_open_synchronized_session(self) -> SynchronizedSession: ...
    async def async_resume_events(
        self, checkpoint: ReplayCheckpoint
    ) -> EventSession: ...
    async def async_close(self) -> None: ...


@dataclass(slots=True, repr=False)
class _PendingProtocolClient:
    endpoint: HubEndpoint
    _bearer_token: str | None = field(default=None, repr=False)

    def __repr__(self) -> str:
        return f"{type(self).__name__}(use_tls={self.endpoint.use_tls!r})"

    async def async_open_synchronized_session(self) -> SynchronizedSession:
        raise DeploymentNotReadyError()

    async def async_resume_events(
        self, checkpoint: ReplayCheckpoint
    ) -> EventSession:
        raise DeploymentNotReadyError()

    async def async_close(self) -> None:
        return None
```

Delete `async_snapshot` and `async_events` from both the protocol and pending client. Add this complete deterministic fixture implementation to `tests/helpers.py`; keep its existing discovery/pair/access methods above the block:

```python
class FixtureEventSession:
    def __init__(
        self,
        binding: SessionBinding,
        items: Iterable[StreamItem] = (),
    ) -> None:
        self.binding = binding
        self._items: asyncio.Queue[StreamItem | object] = asyncio.Queue()
        for item in items:
            self._items.put_nowait(item)
        self._failure: asyncio.Future[HubClientError] = (
            asyncio.get_running_loop().create_future()
        )
        self._closed = False
        self.close_count = 0

    def __aiter__(self):
        return self

    async def __anext__(self) -> StreamItem:
        if self._closed:
            raise StopAsyncIteration
        return await self._items.get()

    async def async_wait_failure(self) -> HubClientError:
        return await asyncio.shield(self._failure)

    def push(self, item: StreamItem) -> None:
        self._items.put_nowait(item)

    def fail(self, error: HubClientError) -> None:
        if not self._failure.done():
            self._failure.set_result(error)

    async def async_close(self) -> None:
        if self._closed:
            return
        self._closed = True
        self.close_count += 1
        if not self._failure.done():
            self._failure.cancel()


class FixtureHubClient:
    def __init__(
        self,
        *,
        snapshot: HubSnapshot | None = None,
        binding: SessionBinding | None = None,
        boundary_event_id: str | None = "fixture-boundary",
        open_response: JsonResponse | None = None,
        resume_error: HubClientError | None = None,
    ) -> None:
        commit = initial_protocol_commit()
        self.snapshot = snapshot or commit.snapshot
        self.binding = binding or commit.binding
        self.boundary_event_id = boundary_event_id
        self.open_response = open_response
        self.resume_error = resume_error
        self.resume_checkpoints: list[ReplayCheckpoint] = []
        self.exposed_sessions: list[FixtureEventSession] = []
        self.closed = False

    def new_event_session(self) -> FixtureEventSession:
        return FixtureEventSession(self.binding)

    def _assert_no_open_session(self) -> None:
        if any(session.close_count == 0 for session in self.exposed_sessions):
            raise AssertionError("fixture permits one open stream")

    async def _async_validate_fixture_response(self) -> None:
        if self.open_response is None:
            return
        if self.open_response.status != 200:
            error = await async_parse_problem_response(
                self.open_response, operation=ProblemOperation.EVENT_STREAM
            )
            raise error
        validate_sse_handshake(
            status=self.open_response.status,
            headers=self.open_response.headers,
            expected_version=self.binding.protocol_version,
        )

    async def async_open_synchronized_session(self) -> SynchronizedSession:
        self._assert_no_open_session()
        await self._async_validate_fixture_response()
        events = self.new_event_session()
        self.exposed_sessions.append(events)
        return SynchronizedSession(
            self.snapshot,
            self.boundary_event_id,
            self.binding,
            events,
        )

    async def async_resume_events(
        self, checkpoint: ReplayCheckpoint
    ) -> FixtureEventSession:
        self._assert_no_open_session()
        self.resume_checkpoints.append(checkpoint)
        if self.resume_error is not None:
            raise self.resume_error
        if checkpoint.binding != self.binding:
            raise DeploymentNotReadyError()
        await self._async_validate_fixture_response()
        events = self.new_event_session()
        self.exposed_sessions.append(events)
        return events

    async def async_close(self) -> None:
        if self.closed:
            return
        self.closed = True
        for session in self.exposed_sessions:
            await session.async_close()
```

Import `Iterable`, the runtime/client/body types, `async_parse_problem_response`, `ProblemOperation`, and `validate_sse_handshake` at the top of `tests/helpers.py`. The fixture exposes raw typed items only; it never accepts raw transport text as a session item.

- [ ] **Step 4: Run client tests and verify green**

Run: `uv run pytest tests/test_client.py tests/test_models.py -q`

Expected: PASS.

- [ ] **Step 5: Run client style gate**

Run: `uv run ruff check custom_components/teslatlas_hub/client.py tests/helpers.py tests/test_client.py`

Expected: PASS.

- [ ] **Step 6: Commit Task 4**

```bash
git add custom_components/teslatlas_hub/client.py tests/helpers.py tests/test_client.py
git commit -m "test: add synchronized fixture sessions"
```

### Task 5: Coordinate startup, replay, recovery, and shutdown explicitly

**Files:**
- Rewrite: `custom_components/teslatlas_hub/coordinator.py`
- Modify: `custom_components/teslatlas_hub/__init__.py`
- Rewrite: `tests/test_coordinator.py`
- Modify: `tests/test_init.py`
- Modify: `tests/test_sensor.py`
- Modify: `tests/test_binary_sensor.py`
- Modify: `tests/test_diagnostics.py`
- Modify: `tests/helpers.py`
- Modify: `custom_components/teslatlas_hub/strings.json`
- Modify: `custom_components/teslatlas_hub/translations/en.json`
- Modify: `tests/test_translations.py`

**Interfaces:**

```python
type Sleep = Callable[[float], Awaitable[None]]
type WaitFor[T] = Callable[[Awaitable[T], float], Awaitable[T]]
type Jitter = Callable[[float], float]
type GenerationFactory = Callable[[], str]


class ConnectionState(StrEnum):
    PENDING = "pending"
    CONNECTING = "connecting"
    LIVE = "live"
    UNAVAILABLE = "unavailable"
    RECONCILING = "reconciling"
    AUTH_REQUIRED = "auth_required"
    TERMINAL = "terminal"
    CLOSED = "closed"


class TeslatlasDataCoordinator(DataUpdateCoordinator[HubSnapshot]):
    connection_state: ConnectionState
    error_code: ClientErrorCode | None
    last_event_id: str | None

    def async_start(self) -> Task[None]: ...
    async def async_shutdown(self) -> None: ...
```

Terminal Home Assistant repair issues use per-entry IDs `f"{entry.entry_id}_{condition.value}"`, no placeholders/data, `is_fixable=False`, `is_persistent=True`, and `IssueSeverity.ERROR`. `RepairCondition(StrEnum)` has exact translation keys `terminal_protocol`, `terminal_stream_ended`, `terminal_storage`, and `terminal_continuity`. A same-binding resume deletes all four only after its fresh handshake succeeds; synchronized startup deletes them only after binding validation and the atomic initial commit succeed. Ordinary unload, reload cleanup, setup-failure shutdown, or failed initial storage preserve active issues. `async_remove_entry(hass, entry)` deletes all four only when the config entry itself is removed.

The injected default reconnect is 3.0 seconds; valid SSE retry is `min(milliseconds, 30_000) / 1000`; production jitter remains within 0-30 seconds. Heartbeat timeout is 20 seconds (15-second guarantee plus five-second grace). Validated rate-limit delay is independent and ranges from 0 through 86,400 seconds.

- [ ] **Step 1: Write failing startup/commit-order tests**

```python
import asyncio
from dataclasses import replace
from unittest.mock import AsyncMock, patch

import pytest
from homeassistant.config_entries import ConfigEntryState
from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from homeassistant.helpers import issue_registry as ir
from homeassistant.helpers.update_coordinator import UpdateFailed
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.checkpoint import ProjectionCheckpointStore
from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_CREDENTIAL_GENERATION,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.coordinator import (
    ALL_REPAIR_CONDITIONS,
    ConnectionState,
    RepairCondition,
    TeslatlasDataCoordinator,
)
from custom_components.teslatlas_hub.errors import (
    HubAuthenticationError,
    HubConnectionError,
    ProtocolConformanceError,
    StorageError,
    StreamCapacityError,
    TerminalStreamError,
)
from custom_components.teslatlas_hub.runtime import ReconciliationRequired
from custom_components.teslatlas_hub.sse import (
    SseCheckpointReset,
    SseDispatch,
    SseHeartbeat,
    SseRetry,
)
from tests.helpers import (
    FixtureHubClient,
    MemoryStore,
    initial_protocol_commit,
    protocol_dispatch,
)


class SleepRecorder:
    def __init__(self) -> None:
        self.delays: list[float] = []

    async def __call__(self, delay: float) -> None:
        self.delays.append(delay)


def runtime_entry(hass: HomeAssistant) -> MockConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Teslatlas Hub",
        unique_id="hub-fixture",
        data={
            CONF_HOST: "hub.example.invalid",
            CONF_PORT: 443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "fixture-bearer",
            CONF_CREDENTIAL_GENERATION: 1,
        },
    )
    entry.add_to_hass(hass)
    entry.mock_state(hass, ConfigEntryState.SETUP_IN_PROGRESS)
    return entry


async def make_coordinator(
    hass,
    *,
    client=None,
    stored=None,
    backend=None,
    generation="generation-new",
    sleep=None,
    wait_for=asyncio.wait_for,
):
    entry = runtime_entry(hass)
    backend = backend or MemoryStore()
    store = ProjectionCheckpointStore(hass, entry.entry_id, store=backend)
    if stored is not None:
        await store.async_commit(stored)
    client = client or FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(
        hass,
        entry,
        client,
        store=store,
        normalized_filters=(),
        generation_factory=lambda: generation,
        sleep=sleep or SleepRecorder(),
        wait_for=wait_for,
        jitter=lambda delay: delay,
    )
    return coordinator, client, backend


async def test_initial_projection_is_persisted_before_publication(hass):
    order: list[str] = []
    backend = MemoryStore(on_save=lambda: order.append("save"))
    coordinator, _, _ = await make_coordinator(hass, backend=backend)
    coordinator.async_add_listener(lambda: order.append("publish"))
    await coordinator.async_config_entry_first_refresh()
    assert order == ["save", "publish"]


async def test_loaded_projection_stays_private_until_resume_handshake(hass):
    client = FixtureHubClient(resume_error=HubConnectionError())
    coordinator, _, _ = await make_coordinator(
        hass, client=client, stored=initial_protocol_commit(last_event_id="opaque")
    )
    with pytest.raises(UpdateFailed):
        await coordinator.async_config_entry_first_refresh()
    assert coordinator.data is None


@pytest.mark.parametrize("stored_kind", ["absent", "invalid", "empty-checkpoint"])
async def test_startup_without_usable_checkpoint_opens_synchronized_session(
    hass, stored_kind
):
    backend = MemoryStore({"invalid": True}) if stored_kind == "invalid" else None
    stored = (
        initial_protocol_commit(last_event_id=None)
        if stored_kind == "empty-checkpoint"
        else None
    )
    coordinator, client, _ = await make_coordinator(
        hass, stored=stored, backend=backend
    )
    await coordinator.async_config_entry_first_refresh()
    assert client.open_calls == 1
    assert client.resume_checkpoints == []
    assert coordinator.connection_state is ConnectionState.LIVE


async def test_same_binding_resume_completes_before_stored_publication(hass):
    order: list[str] = []
    stored = initial_protocol_commit(last_event_id="opaque")
    client = FixtureHubClient(binding=stored.binding, on_resume=lambda: order.append("resume"))
    coordinator, _, _ = await make_coordinator(hass, client=client, stored=stored)
    coordinator.async_add_listener(lambda: order.append("publish"))
    await coordinator.async_config_entry_first_refresh()
    assert order == ["resume", "publish"]
    assert coordinator.data == stored.snapshot


@pytest.mark.parametrize(
    ("target", "field", "value"),
    [
        ("binding", "hub_id", "other"),
        ("binding", "api_origin", "https://other.invalid"),
        ("binding", "event_origin", "https://other.invalid"),
        ("binding", "protocol_version", "1.1.0"),
        ("binding", "normalized_filters", (("vehicle", "other"),)),
        ("binding", "credential_generation", 2),
        ("event_binding", "hub_id", "other"),
        ("snapshot_info", "api_origin", "https://other.invalid"),
    ],
)
async def test_startup_rejects_every_session_binding_mismatch_before_commit(
    hass, target, field, value
):
    client = FixtureHubClient()
    client.mutate_next_synchronized(target=target, field=field, value=value)
    coordinator, _, backend = await make_coordinator(hass, client=client)
    with pytest.raises(UpdateFailed):
        await coordinator.async_config_entry_first_refresh()
    assert backend.saved == []
    assert coordinator.data is None


async def test_fresh_session_uses_injected_generation_and_one_owned_session(hass):
    coordinator, client, _ = await make_coordinator(
        hass, generation="injected-generation"
    )
    await coordinator.async_config_entry_first_refresh()
    assert coordinator._commit.projection_generation == "injected-generation"
    assert client.open_session_count == 1
    assert coordinator.update_interval is None


async def test_initial_storage_failure_never_publishes(hass):
    backend = MemoryStore()
    backend.save_error = OSError("private path")
    coordinator, _, _ = await make_coordinator(hass, backend=backend)
    with pytest.raises(UpdateFailed):
        await coordinator.async_config_entry_first_refresh()
    assert coordinator.data is None


@pytest.mark.parametrize("mode", ["resume", "synchronized"])
async def test_confirmed_continuity_clears_all_stale_terminal_issues(hass, mode):
    stored = initial_protocol_commit(last_event_id="opaque") if mode == "resume" else None
    coordinator, _, _ = await make_coordinator(hass, stored=stored)
    with patch.object(ir, "async_delete_issue") as delete:
        await coordinator.async_config_entry_first_refresh()
    assert {call.args[2] for call in delete.call_args_list} == {
        f"{coordinator.config_entry.entry_id}_{condition.value}"
        for condition in ALL_REPAIR_CONDITIONS
    }


@pytest.mark.parametrize("failure", ["handshake", "commit"])
async def test_failed_initial_continuity_preserves_stale_terminal_issues(
    hass, failure
):
    client = FixtureHubClient(resume_error=HubConnectionError())
    backend = MemoryStore()
    if failure == "commit":
        client = FixtureHubClient()
        backend.save_error = OSError()
    coordinator, _, _ = await make_coordinator(
        hass,
        client=client,
        stored=(initial_protocol_commit(last_event_id="opaque") if failure == "handshake" else None),
        backend=backend,
    )
    with patch.object(ir, "async_delete_issue") as delete:
        with pytest.raises(UpdateFailed):
            await coordinator.async_config_entry_first_refresh()
    delete.assert_not_called()
```

- [ ] **Step 2: Write failing replay/state-machine tests**

```python
async def started(hass, **kwargs):
    coordinator, client, backend = await make_coordinator(hass, **kwargs)
    await coordinator.async_config_entry_first_refresh()
    return coordinator, client, backend


async def test_projected_checkpoint_and_duplicate_commit_order(hass):
    coordinator, _, backend = await started(hass)
    order: list[str] = []
    backend.on_save = lambda: order.append("save")
    coordinator.async_add_listener(lambda: order.append("publish"))
    projected = protocol_dispatch("vehicle.current.changed", event_id="opaque-next")
    await coordinator._async_process_item(projected)
    assert order == ["save", "publish"]
    order.clear()
    checkpoint_only = protocol_dispatch("drive.updated", event_id="opaque-only")
    await coordinator._async_process_item(checkpoint_only)
    assert order == ["save"]
    order.clear()
    await coordinator._async_process_item(checkpoint_only)
    assert order == []


async def test_empty_id_reset_persists_before_next_read(hass):
    coordinator, _, backend = await started(hass)
    await coordinator._async_process_item(
        protocol_dispatch("drive.updated", event_id="opaque")
    )
    await coordinator._async_process_item(SseCheckpointReset())
    assert backend.saved[-1]["checkpoint"]["last_event_id"] is None
    assert coordinator.last_event_id is None


@pytest.mark.parametrize("with_checkpoint", [True, False])
async def test_disconnect_closes_old_before_exact_recovery(hass, with_checkpoint):
    stored = initial_protocol_commit(
        last_event_id="opaque" if with_checkpoint else None
    )
    coordinator, client, _ = await started(hass, stored=stored)
    old = coordinator._session
    await coordinator._async_recover(HubConnectionError())
    assert old.close_count == 1
    if with_checkpoint:
        assert client.resume_checkpoints[-1].last_event_id == "opaque"
    else:
        assert client.open_calls == 1
    assert coordinator.connection_state is ConnectionState.LIVE


async def test_unknown_vehicle_holds_prefix_then_later_boundary_supersedes(hass):
    coordinator, client, backend = await started(hass)
    old = coordinator._session
    decision = ReconciliationRequired(
        protocol_dispatch("future.event", event_id="pending").identity,
        "vehicle-new",
    )
    client.set_next_snapshot_with_vehicle("vehicle-new")
    old.push(SseHeartbeat())
    await coordinator._async_reconcile(decision)
    assert old.close_count == 1
    assert backend.saved[-1]["projection_generation"] != "generation-new"
    assert "vehicle-new" in coordinator.data.vehicles


@pytest.mark.parametrize("failure", ["timeout", "capacity"])
async def test_reconciliation_timeout_or_capacity_is_terminal(hass, failure):
    async def wait_for(awaitable, timeout):
        if timeout == 30:
            awaitable.close()
            raise TimeoutError
        return await awaitable

    coordinator, client, _ = await started(
        hass, wait_for=wait_for if failure == "timeout" else asyncio.wait_for
    )
    decision = ReconciliationRequired(
        protocol_dispatch("future.event", event_id="pending").identity,
        "vehicle-new",
    )
    if failure == "capacity":
        coordinator._session.fail(StreamCapacityError())
        client.block_next_open()
    await coordinator._async_reconcile(decision)
    assert coordinator.connection_state is ConnectionState.TERMINAL
    assert coordinator.repair_condition is RepairCondition.TERMINAL_CONTINUITY


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("hub_id", "other"),
        ("api_origin", "https://other.invalid"),
        ("event_origin", "https://other.invalid"),
        ("protocol_version", "1.1.0"),
        ("normalized_filters", (("vehicle", "other"),)),
        ("credential_generation", 2),
    ],
)
async def test_reconciliation_reuses_full_synchronized_validator_before_commit(
    hass, field, value
):
    coordinator, client, backend = await started(hass)
    saved_count = len(backend.saved)
    client.set_next_snapshot_with_vehicle("vehicle-new")
    client.mutate_next_synchronized(target="binding", field=field, value=value)
    decision = ReconciliationRequired(
        protocol_dispatch("future.event", event_id="pending").identity,
        "vehicle-new",
    )
    await coordinator._async_reconcile(decision)
    assert coordinator.connection_state is ConnectionState.TERMINAL
    assert len(backend.saved) == saved_count


async def test_synchronized_replacement_omission_retires_old_vehicle(hass):
    coordinator, client, _ = await started(hass)
    client.omit_vehicle_from_next_snapshot("vehicle-beta")
    client.set_next_snapshot_with_vehicle("vehicle-new")
    decision = ReconciliationRequired(
        protocol_dispatch("future.event", event_id="pending").identity,
        "vehicle-new",
    )
    await coordinator._async_reconcile(decision)
    assert "vehicle-beta" not in coordinator.data.vehicles


@pytest.mark.parametrize(
    ("error", "validate_calls"),
    [(ReplayExpiredError(), 0), (EventIdInvalidError(), 1)],
)
async def test_cursor_problem_clears_then_synchronizes(hass, error, validate_calls):
    coordinator, client, _ = await started(hass)
    await coordinator._async_recover(error)
    assert coordinator.last_event_id is None
    assert client.validate_access_calls == validate_calls
    assert client.open_calls == 2


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("hub_id", "other"),
        ("credential_generation", 2),
        ("normalized_filters", (("vehicle", "other"),)),
        ("api_origin", "https://other.invalid"),
        ("event_origin", "https://other.invalid"),
        ("protocol_version", "1.1.0"),
    ],
)
async def test_known_event_rechecks_live_binding_before_checkpoint(
    hass, field, value
):
    coordinator, _, backend = await started(hass)
    saved_count = len(backend.saved)
    if field == "credential_generation":
        coordinator.config_entry.data = {
            **coordinator.config_entry.data,
            CONF_CREDENTIAL_GENERATION: value,
        }
    elif field == "normalized_filters":
        coordinator._normalized_filters = value
    else:
        coordinator._session.binding = replace(
            coordinator._session.binding, **{field: value}
        )
    await coordinator._async_process_item(
        protocol_dispatch("vehicle.current.changed", event_id="opaque-next")
    )
    assert coordinator.connection_state is ConnectionState.TERMINAL
    assert len(backend.saved) == saved_count


@pytest.mark.parametrize(
    ("error", "condition"),
    [
        (ProtocolConformanceError(), RepairCondition.TERMINAL_PROTOCOL),
        (StorageError(), RepairCondition.TERMINAL_STORAGE),
        (StreamCapacityError(), RepairCondition.TERMINAL_CONTINUITY),
        (TerminalStreamError(), RepairCondition.TERMINAL_STREAM_ENDED),
    ],
)
async def test_terminal_failures_latch_safe_persistent_repair(
    hass, error, condition
):
    coordinator, _, _ = await started(hass)
    with patch.object(ir, "async_create_issue") as create:
        await coordinator._async_terminal(error)
    assert coordinator.connection_state is ConnectionState.TERMINAL
    kwargs = create.call_args.kwargs
    assert kwargs["translation_key"] == condition.value
    assert kwargs["is_fixable"] is False
    assert kwargs["is_persistent"] is True
    assert "translation_placeholders" not in kwargs
    assert "data" not in kwargs


async def test_terminal_manual_refresh_makes_no_client_call(hass):
    coordinator, client, _ = await started(hass)
    await coordinator._async_terminal(ProtocolConformanceError())
    calls = client.total_runtime_calls
    with pytest.raises(UpdateFailed):
        await coordinator._async_update_data()
    assert client.total_runtime_calls == calls


async def test_retry_rate_limit_heartbeat_exhaustion_and_cancellation(hass):
    sleep = SleepRecorder()
    coordinator, _, _ = await started(hass, sleep=sleep)
    await coordinator._async_process_item(SseRetry(30_001))
    assert coordinator._reconnect_delay == 30
    await coordinator._async_recover(RateLimitedError(7))
    assert sleep.delays == [7]
    assert coordinator._heartbeat_seconds == 20
    task = asyncio.create_task(coordinator._sleep(3))
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task


@pytest.mark.parametrize(
    "error",
    [
        HubConnectionError(),
        ReplayExpiredError(),
        EventIdInvalidError(),
        RateLimitedError(0),
        HubAuthenticationError(),
        ProtocolConformanceError(),
        StorageError(),
        StreamCapacityError(),
        TerminalStreamError(),
    ],
)
async def test_every_failed_session_closes_once_before_transition(hass, error):
    coordinator, client, _ = await started(hass)
    old = coordinator._session
    await coordinator._handle_stream_failure(error)
    assert old.close_count == 1
    assert client.maximum_simultaneous_open <= 1


async def test_shutdown_closes_task_session_and_client_once_preserving_repairs(hass):
    coordinator, client, _ = await started(hass)
    with patch.object(ir, "async_delete_issue") as delete:
        coordinator.async_start()
        await coordinator.async_shutdown()
        await coordinator.async_shutdown()
    assert client.close_count == 1
    assert all(session.close_count == 1 for session in client.exposed_sessions)
    delete.assert_not_called()
```

Add two integration tests to `tests/test_init.py`:

```python
async def test_terminal_issue_survives_setup_cleanup_unload_and_reload(hass):
    entry = runtime_entry(hass)
    client = FixtureHubClient(open_error=ProtocolConformanceError())
    with (
        patch("custom_components.teslatlas_hub.create_client", return_value=client),
        patch.object(ir, "async_delete_issue") as delete,
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is False
        await hass.config_entries.async_unload(entry.entry_id)
    delete.assert_not_called()


async def test_config_entry_removal_deletes_all_terminal_issues(hass):
    entry = runtime_entry(hass)
    with patch.object(ir, "async_delete_issue") as delete:
        await async_remove_entry(hass, entry)
    assert {call.args[2] for call in delete.call_args_list} == {
        f"{entry.entry_id}_{condition.value}" for condition in ALL_REPAIR_CONDITIONS
    }
```

- [ ] **Step 3: Run coordinator tests and verify red**

Run: `uv run pytest tests/test_coordinator.py tests/test_init.py -q`

Expected: FAIL because the old coordinator publishes a racing snapshot, stores no checkpoint, and uses exponential reconnect.

- [ ] **Step 4: Implement synchronized initial refresh**

Load the strict commit using only the config entry's expected Hub ID and credential generation. If it has a non-empty checkpoint, await `async_resume_events`; before returning the stored snapshot, require its freshly negotiated session binding to equal the stored checkpoint/commit binding and the live entry Hub ID, credential generation, and normalized filters. Otherwise call `async_open_synchronized_session`; before creating or saving a commit, require snapshot `HubInfo` Hub ID, API origin, event origin, and protocol version to equal both session/event bindings, then require the binding Hub ID, credential generation, and normalized filters to equal the live entry/coordinator configuration. Create one projection generation, atomically save the initial commit, retain the already-open session, and only then return the snapshot to Home Assistant.

- [ ] **Step 5: Implement the stream transition table**

For each known `SseDispatch`, first require `active_session.binding == current_commit.binding`; require that binding's Hub ID and credential generation to equal the live config entry, its normalized filters to equal the coordinator's current configured filters, and its API origin, event origin, and protocol profile to remain the freshly negotiated values in the active session. Any mismatch is terminal before JSON apply or checkpoint. Then obtain an `EventDecision`. Persist an accepted commit before calling `async_set_updated_data`; checkpoint-only decisions save without publishing; duplicates do neither. A reset item saves `last_event_id=None` before requesting the next stream item. Retry updates only the safe bounded reconnect delay. Heartbeat only resets liveness. Any disconnect, stream exhaustion, or heartbeat timeout first calls `async_set_update_error`, closes the failed active session exactly once, then resumes only from a non-empty checkpoint; otherwise it requests a fresh synchronized fixture boundary. Close the active session once before any `400`, `410`, `429`, authentication, terminal-protocol, storage, capacity, or `204` transition. Outside the explicit reconciliation branch, opening a replacement is forbidden until the old session is closed, so at most one session is open.

On `ReconciliationRequired`, retain the exact pending decision without applying or checkpointing it, mark the coordinator unavailable, and stop consuming the old session so its `BoundedEventBuffer` holds at most 64 later items with the existing five-second producer deadline. Race `old_session.async_wait_failure()` against a fixture synchronized-replacement task under one 30-second timeout. If the failure waiter is done in the first completed set—even when the replacement completes in the same loop turn—failure wins: cancel/await any unfinished replacement work, close any replacement session it produced, close the old session, and enter terminal continuity failure. If synchronization alone wins, cancel and await the failure waiter before evaluating the candidate. A successful boundary strictly after the candidate atomically supersedes the pending event and buffered old-session prefix: persist and publish the new generation, then close the old session and switch to the replacement stream. If the synchronized candidate still omits the pending vehicle or the 30-second timeout expires, cancel/await both race tasks, close every session they own, and enter terminal continuity failure. This is the only permitted discard: a proven later synchronized boundary supersedes the entire held prefix.

Terminal protocol/storage/capacity/204 conditions close all owned sessions, latch `ConnectionState.TERMINAL`, set a classified error, create the exact allow-listed persistent issue through `issue_registry.async_create_issue`, stop, and make later coordinator refresh requests fail without calling the client. Map `ProtocolBodyError`/`ProtocolConformanceError` to `terminal_protocol`, `TerminalStreamError` to `terminal_stream_ended`, `StorageError` to `terminal_storage`, and `StreamCapacityError`/failed reconciliation to `terminal_continuity`. Use no issue data or translation placeholders. Authentication closes the active session, stops, and starts reauth once. A validated 429 closes any active session before sleeping its own seconds; other reconnects use bounded SSE/default delay. Use injected `wait_for(session_iterator.__anext__(), 20)` as the cancellable heartbeat watchdog. Update diagnostics to serialize only `.value` from the state/error enums.

- [ ] **Step 6: Implement idempotent shutdown**

Cancel and await the one coordinator task, close every still-owned session once, close the client once, and call `super().async_shutdown()`. Preserve `CancelledError` at every boundary. Entry setup failure uses the same shutdown path and must not delete a terminal issue. Add the four safe title/description pairs under `issues` in both translation files; they contain no placeholders. Implement `async_remove_entry(hass, entry)` in `__init__.py` to delete all four per-entry issue IDs only when Home Assistant removes that config entry.

- [ ] **Step 7: Run lifecycle and entity gates**

Run: `uv run pytest tests/test_coordinator.py tests/test_init.py tests/test_sensor.py tests/test_binary_sensor.py tests/test_diagnostics.py tests/test_translations.py --cov=custom_components.teslatlas_hub.coordinator --cov-branch --cov-fail-under=100 -q`

Run: `uv run ruff check custom_components/teslatlas_hub/coordinator.py custom_components/teslatlas_hub/__init__.py tests/test_coordinator.py tests/test_init.py`

Expected: PASS at 100% coordinator branch coverage.

- [ ] **Step 8: Commit Task 5**

```bash
git add custom_components/teslatlas_hub/coordinator.py custom_components/teslatlas_hub/__init__.py custom_components/teslatlas_hub/strings.json custom_components/teslatlas_hub/translations/en.json tests/test_coordinator.py tests/test_init.py tests/test_sensor.py tests/test_binary_sensor.py tests/test_diagnostics.py tests/test_translations.py
git commit -m "feat: coordinate synchronized replay sessions"
```
