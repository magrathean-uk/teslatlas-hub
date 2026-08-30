#!/usr/bin/env python3
from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
CASE_DIR = ROOT / "conformance" / "cases"
COMPATIBILITY_DIR = ROOT / "compatibility"

DISCOVERY_SCHEMA = "urn:teslatlas:protocol:schema:discovery:1.2.0"
RESOURCE_SCHEMA = "urn:teslatlas:protocol:schema:resources:1.2.0"
ERROR_SCHEMA = "urn:teslatlas:protocol:schema:error:1.2.0"
EVENT_SCHEMA = "urn:teslatlas:protocol:schema:event:1.2.0"
COMMAND_SCHEMA = "urn:teslatlas:protocol:schema:command:1.2.0"
METADATA_SCHEMA = "urn:teslatlas:protocol:schema:metadata:1.2.0"

ETAG_DISCOVERY = '"y6Kk9pX2mV4qT7sN"'
ETAG_MINIMUM_VERSION = '"sH4wQ8nC2kT6mP9x"'
ETAG_VEHICLES = '"aP3xD8mQ5vR2nK7t"'
ETAG_DRIVES_FIRST = '"fN4qL9sB2wH6cM8z"'
ETAG_DRIVES_SECOND = '"uC7pE3kR9xT5mV2d"'
ETAG_CURRENT_FIRST = '"bY8nJ4qW2sF7kL5p"'
ETAG_CURRENT_SECOND = '"rM3vT9cX6gP2hK8w"'
ETAG_COMMAND = '"zQ5dN8wL3pV7tC2m"'
ETAG_NUISANCE = '"kF2sH6yR9nB4qX7v"'
ETAG_METADATA_FIRST = '"mC7pL2xW9dR5tN8q"'
ETAG_METADATA_SECOND = '"vT4kB8qM2yH6sP9n"'
ETAG_METADATA_DELETED = '"hR9mX3cF7pL2wQ6d"'
ETAG_DEPRECATED_DISCOVERY = '"dN6qV2tK8xC4mP7s"'


def load_example(name: str) -> dict[str, Any]:
    def reject_constant(value: str) -> None:
        raise ValueError(f"non-finite JSON constant: {value}")

    return json.loads(
        (ROOT / "examples" / name).read_text(encoding="utf-8"),
        parse_constant=reject_constant,
    )


def request(
    method: str,
    path: str,
    *,
    headers: dict[str, str] | None = None,
    query: dict[str, Any] | None = None,
    body: Any = None,
    body_bytes: int | None = None,
    omit_protocol_version: bool = False,
) -> dict[str, Any]:
    request_headers = dict(headers or {})
    if path.startswith("/v1/") and not omit_protocol_version:
        request_headers.setdefault("Teslatlas-Protocol-Version", "${profile}")
    value: dict[str, Any] = {
        "method": method,
        "path": path,
        "headers": request_headers,
        "query": query or {},
    }
    if body is not None:
        value["body"] = body
    if body_bytes is not None:
        value["body_bytes"] = body_bytes
    return value


def json_headers(
    etag: str | None = None, *, protocol_version: str = "${profile}"
) -> dict[str, str]:
    value = {
        "Content-Type": "application/json",
        "Teslatlas-Protocol-Version": protocol_version,
        "Cache-Control": "private, max-age=0, must-revalidate",
        "Vary": "Authorization, Teslatlas-Protocol-Version",
    }
    if etag is not None:
        value["ETag"] = etag
    return value


def schema_definition(schema_id: str, name: str) -> str:
    return f"{schema_id}#/$defs/{name}"


def problem(
    status: int,
    code: str,
    title: str,
    *,
    retryable: bool = False,
    detail: str | None = None,
) -> dict[str, Any]:
    value: dict[str, Any] = {
        "type": f"urn:teslatlas:problem:{code.replace('_', '-')}",
        "title": title,
        "status": status,
        "instance": f"/requests/request_{code}",
        "code": code,
        "request_id": f"request_{code}",
        "retryable": retryable,
    }
    if detail is not None:
        value["detail"] = detail
    return value


def problem_response(status: int, code: str, title: str, **kwargs: Any) -> dict[str, Any]:
    return {
        "status": status,
        "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": f"request_{code}",
        },
        "body": problem(status, code, title, **kwargs),
    }


def expected_problem(status: int, code: str) -> dict[str, Any]:
    return {
        "status": status,
        "headers_present": ["Content-Type", "Teslatlas-Protocol-Version", "X-Request-ID"],
        "headers_equal": {"Content-Type": "application/problem+json"},
        "body_schema": ERROR_SCHEMA,
        "assertions": [
            {"path": "/body/status", "op": "equals", "value": status},
            {"path": "/body/code", "op": "equals", "value": code},
        ],
    }


def step(
    step_id: str,
    request_value: dict[str, Any],
    expect: dict[str, Any],
    reference_response: dict[str, Any],
) -> dict[str, Any]:
    return {
        "step_id": step_id,
        "request": request_value,
        "expect": expect,
        "reference_response": reference_response,
    }


def case(
    case_id: str,
    introduced_in: str,
    capability: str,
    description: str,
    steps: list[dict[str, Any]],
) -> dict[str, Any]:
    return {
        "kind": "case",
        "case_id": case_id,
        "introduced_in": introduced_in,
        "capability": capability,
        "description": description,
        "steps": steps,
    }


def build_cases() -> list[dict[str, Any]]:
    discovery = load_example("discovery.json")
    minimum_protocol_version = discovery["protocol"]["minimum_client_version"]
    vehicles = load_example("vehicles-page.json")
    drives_one = load_example("drives-page.json")
    drives_two = copy.deepcopy(drives_one)
    drives_two["items"][0]["drive_id"] = "drive_demo_0002"
    drives_two["items"][0]["quality"]["subject_id"] = "drive_demo_0002"
    drives_two["items"][0]["start_at"] = "2026-08-31T12:00:00.000Z"
    drives_two["items"][0]["end_at"] = "2026-08-31T12:30:00.000Z"
    drives_two["items"][0]["quality"]["assessed_at"] = "2026-08-31T12:30:00.000Z"
    drives_two["next_cursor"] = None
    drives_two["generated_at"] = "2026-08-31T12:31:00.000Z"

    current = load_example("current-state.json")
    changed_current = copy.deepcopy(current)
    changed_current["revision"] = 43
    changed_current["battery_level_percent"] = 77
    changed_current["observed_at"] = "2026-08-30T12:01:00.000Z"
    changed_current["quality"]["assessed_at"] = "2026-08-30T12:01:01.250Z"

    event_one = load_example("event-envelope.json")
    event_two = copy.deepcopy(event_one)
    event_two["event_id"] = "event_demo_0043"
    event_two["occurred_at"] = "2026-08-30T12:01:01.250Z"
    event_two["revision"] = 43
    event_two["data"] = changed_current

    command_request = load_example("command-request.json")
    command_job = load_example("command-job.json")
    nuisance_request = {
        "vehicle_id": "vehicle_demo_alpha",
        "command": "honk_horn",
        "command_class": "nuisance",
        "parameters": {},
        "expected_state": {"honk_sequence": 1},
        "expires_at": "2026-08-30T12:05:00.000Z",
        "confirmation": {
            "confirmed_at": "2026-08-30T12:00:00.000Z",
            "confirmed_by": "user_demo_owner",
        },
    }
    nuisance_job = copy.deepcopy(command_job)
    nuisance_job["command_id"] = "command_demo_nuisance_0001"
    nuisance_job["command"] = "honk_horn"
    nuisance_job["command_class"] = "nuisance"
    nuisance_job["retry_policy"] = "none"
    nuisance_job["links"]["self"] = "/v1/commands/command_demo_nuisance_0001"

    metadata = load_example("metadata-record.json")
    metadata_tombstone = load_example("metadata-tombstone.json")
    metadata_updated = copy.deepcopy(metadata)
    metadata_updated["revision"] = 2
    metadata_updated["value"] = {"text": "Updated redacted demonstration trip."}
    metadata_updated["updated_at"] = "2026-08-30T12:45:00.000Z"
    metadata_updated["audit"].append(
        {
            "revision": 2,
            "action": "updated",
            "at": "2026-08-30T12:45:00.000Z",
            "actor_id": "user_demo_owner",
            "previous_hash": metadata["audit"][0]["new_hash"],
            "new_hash": "bc" * 32,
        }
    )
    metadata_empty_page = load_example("metadata-page.json")
    metadata_empty_page["items"] = []
    metadata_empty_page["snapshot_revision"] = "snapshot_demo_metadata_deleted"
    metadata_empty_page["generated_at"] = "2026-08-30T12:50:01.000Z"
    metadata_deleted_event = {
        "event_id": "event_demo_metadata_deleted_0001",
        "event_type": "metadata.changed",
        "occurred_at": metadata_tombstone["audit"]["deletion"]["at"],
        "vehicle_id": metadata_tombstone["vehicle_id"],
        "resource_id": metadata_tombstone["metadata_id"],
        "revision": metadata_tombstone["audit"]["deletion"]["revision"],
        "data": metadata_tombstone,
    }

    deprecated_discovery = copy.deepcopy(discovery)
    deprecated_discovery["capabilities"][0]["status"] = "deprecated"
    deprecated_discovery["capabilities"][0]["deprecation"] = {
        "deprecated_at": "2030-01-01T00:00:00.000Z",
        "sunset_at": "2030-07-01T00:00:00.000Z",
        "successor": "query.vehicles.next",
        "documentation": "https://protocol.example.invalid/deprecations/query-vehicles",
    }

    cases = [
        case(
            "discovery-version-negotiation",
            "1.0.0",
            "query.vehicles",
            "Discovery is public, version selection is explicit, and incompatible majors fail closed.",
            [
                step(
                    "discover",
                    request("GET", "/.well-known/teslatlas-hub"),
                    {
                        "status": 200,
                        "headers_present": ["Content-Type", "ETag"],
                        "body_schema": DISCOVERY_SCHEMA,
                        "assertions": [
                            {
                                "path": "/body/protocol/current_version",
                                "op": "equals",
                                "value": "1.2.0",
                            }
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_DISCOVERY),
                        "body_file": "examples/discovery.json",
                    },
                ),
                step(
                    "omitted-version-selects-minimum",
                    request(
                        "GET",
                        "/v1/vehicles",
                        omit_protocol_version=True,
                    ),
                    {
                        "status": 200,
                        "headers_equal": {
                            "Teslatlas-Protocol-Version": "${discover.body.protocol.minimum_client_version}"
                        },
                        "body_schema": schema_definition(
                            RESOURCE_SCHEMA, "vehicle_page"
                        ),
                    },
                    {
                        "status": 200,
                        "headers": json_headers(
                            ETAG_MINIMUM_VERSION,
                            protocol_version=minimum_protocol_version,
                        ),
                        "body_file": "examples/vehicles-page.json",
                    },
                ),
                step(
                    "select-profile",
                    request(
                        "GET",
                        "/v1/vehicles",
                        headers={"Teslatlas-Protocol-Version": "${profile}"},
                    ),
                    {
                        "status": 200,
                        "headers_equal": {"Teslatlas-Protocol-Version": "${profile}"},
                        "body_schema": schema_definition(RESOURCE_SCHEMA, "vehicle_page"),
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_VEHICLES),
                        "body_file": "examples/vehicles-page.json",
                    },
                ),
                step(
                    "unsupported-major",
                    request(
                        "GET",
                        "/v1/vehicles",
                        headers={"Teslatlas-Protocol-Version": "2.0.0"},
                    ),
                    expected_problem(426, "unsupported_protocol_version"),
                    problem_response(
                        426, "unsupported_protocol_version", "Unsupported protocol version"
                    ),
                ),
            ],
        ),
        case(
            "cursor-pagination",
            "1.0.0",
            "query.history",
            "Cursors are opaque, query-bound, permission-bound, and explicitly expire.",
            [
                step(
                    "initial",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/drives",
                        headers={"Teslatlas-Protocol-Version": "${profile}"},
                        query={
                            "from": "2026-08-01T00:00:00.000Z",
                            "to": "2026-09-01T00:00:00.000Z",
                            "limit": 1,
                        },
                    ),
                    {
                        "status": 200,
                        "body_schema": schema_definition(RESOURCE_SCHEMA, "drive_page"),
                        "assertions": [
                            {"path": "/body/next_cursor", "op": "exists"},
                            {"path": "/body/items/0/drive_id", "op": "exists"},
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_DRIVES_FIRST),
                        "body_file": "examples/drives-page.json",
                    },
                ),
                step(
                    "next",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/drives",
                        headers={"Teslatlas-Protocol-Version": "${profile}"},
                        query={
                            "from": "2026-08-01T00:00:00.000Z",
                            "to": "2026-09-01T00:00:00.000Z",
                            "limit": 1,
                            "cursor": "${initial.body.next_cursor}",
                        },
                    ),
                    {
                        "status": 200,
                        "body_schema": schema_definition(RESOURCE_SCHEMA, "drive_page"),
                        "assertions": [
                            {
                                "path": "/body/items/0/drive_id",
                                "op": "not_same_as",
                                "ref": "initial.body.items.0.drive_id",
                            }
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_DRIVES_SECOND),
                        "body": drives_two,
                    },
                ),
                step(
                    "query-mismatch",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/drives",
                        headers={"Teslatlas-Protocol-Version": "${profile}"},
                        query={
                            "from": "2026-08-02T00:00:00.000Z",
                            "to": "2026-09-01T00:00:00.000Z",
                            "limit": 1,
                            "cursor": "${initial.body.next_cursor}",
                        },
                    ),
                    expected_problem(409, "cursor_query_mismatch"),
                    problem_response(409, "cursor_query_mismatch", "Cursor query mismatch"),
                ),
                step(
                    "expired",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/drives",
                        headers={"Teslatlas-Protocol-Version": "${profile}"},
                        query={"cursor": "cursor_demo_expired_0001"},
                    ),
                    expected_problem(410, "cursor_expired"),
                    problem_response(410, "cursor_expired", "Cursor expired"),
                ),
                step(
                    "scope-changed",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/drives",
                        headers={
                            "Teslatlas-Protocol-Version": "${profile}",
                            "Teslatlas-Conformance-Authorization-Revision": "2",
                        },
                        query={"cursor": "${initial.body.next_cursor}"},
                    ),
                    expected_problem(403, "cursor_scope_changed"),
                    problem_response(403, "cursor_scope_changed", "Cursor scope changed"),
                ),
                step(
                    "invalid",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/drives",
                        headers={"Teslatlas-Protocol-Version": "${profile}"},
                        query={"cursor": "not-a-valid-token?"},
                    ),
                    expected_problem(400, "invalid_cursor"),
                    problem_response(400, "invalid_cursor", "Invalid cursor"),
                ),
            ],
        ),
        case(
            "etag-conditional-get",
            "1.0.0",
            "query.vehicles",
            "ETags validate representations and a matching If-None-Match returns an empty 304.",
            [
                step(
                    "initial",
                    request("GET", "/v1/vehicles/vehicle_demo_alpha/current"),
                    {
                        "status": 200,
                        "headers_present": ["ETag"],
                        "body_schema": schema_definition(RESOURCE_SCHEMA, "current_state"),
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_CURRENT_FIRST),
                        "body_file": "examples/current-state.json",
                    },
                ),
                step(
                    "not-modified",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/current",
                        headers={"If-None-Match": "${initial.headers.ETag}"},
                    ),
                    {
                        "status": 304,
                        "headers_equal": {"ETag": "${initial.headers.ETag}"},
                        "body_absent": True,
                    },
                    {
                        "status": 304,
                        "headers": {
                            "ETag": ETAG_CURRENT_FIRST,
                            "Teslatlas-Protocol-Version": "${profile}",
                            "Cache-Control": "private, max-age=0, must-revalidate",
                            "Vary": "Authorization, Teslatlas-Protocol-Version",
                        },
                    },
                ),
                step(
                    "changed",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/current",
                        headers={"Teslatlas-Conformance-Revision": "43"},
                    ),
                    {
                        "status": 200,
                        "headers_present": ["ETag"],
                        "body_schema": schema_definition(RESOURCE_SCHEMA, "current_state"),
                        "assertions": [
                            {
                                "path": "/headers/ETag",
                                "op": "not_same_as",
                                "ref": "initial.headers.ETag",
                            },
                            {"path": "/body/revision", "op": "equals", "value": 43},
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_CURRENT_SECOND),
                        "body": changed_current,
                    },
                ),
            ],
        ),
        case(
            "problem-details",
            "1.0.0",
            "query.history",
            "Invalid requests return RFC 9457 problem details with stable codes and ignorable extensions.",
            [
                step(
                    "invalid-range",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/drives",
                        query={
                            "from": "2026-09-01T00:00:00.000Z",
                            "to": "2026-08-01T00:00:00.000Z",
                        },
                    ),
                    {
                        **expected_problem(400, "invalid_time_range"),
                        "assertions": [
                            {"path": "/body/status", "op": "equals", "value": 400},
                            {
                                "path": "/body/code",
                                "op": "equals",
                                "value": "invalid_time_range",
                            },
                            {"path": "/body/future_extension", "op": "exists"},
                        ],
                    },
                    {
                        **problem_response(400, "invalid_time_range", "Invalid time range"),
                        "body": {
                            **problem(400, "invalid_time_range", "Invalid time range"),
                            "future_extension": {"safe_to_ignore": True},
                        },
                    },
                )
            ],
        ),
        case(
            "sse-last-event-id",
            "1.0.0",
            "events.sse",
            "Last-Event-ID resumes strictly after the named event and expired or invalid IDs are explicit.",
            [
                step(
                    "initial",
                    request("GET", "/v1/events"),
                    {
                        "status": 200,
                        "headers_equal": {"Content-Type": "text/event-stream"},
                        "event_schema": EVENT_SCHEMA,
                        "assertions": [
                            {"path": "/events/0/id", "op": "exists"},
                            {"path": "/events/1/id", "op": "exists"},
                        ],
                    },
                    {
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/event-stream",
                            "Teslatlas-Protocol-Version": "${profile}",
                        },
                        "events": [
                            {
                                "id": event_one["event_id"],
                                "event": event_one["event_type"],
                                "data": event_one,
                            },
                            {
                                "id": event_two["event_id"],
                                "event": event_two["event_type"],
                                "data": event_two,
                            },
                        ],
                        "last_event_id_after": event_two["event_id"],
                    },
                ),
                step(
                    "resume",
                    request(
                        "GET",
                        "/v1/events",
                        headers={"Last-Event-ID": "${initial.events.0.id}"},
                    ),
                    {
                        "status": 200,
                        "event_schema": EVENT_SCHEMA,
                        "assertions": [
                            {
                                "path": "/events/0/id",
                                "op": "same_as",
                                "ref": "initial.events.1.id",
                            }
                        ],
                    },
                    {
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/event-stream",
                            "Teslatlas-Protocol-Version": "${profile}",
                        },
                        "events": [
                            {
                                "id": event_two["event_id"],
                                "event": event_two["event_type"],
                                "data": event_two,
                            }
                        ],
                        "last_event_id_after": event_two["event_id"],
                    },
                ),
                step(
                    "expired",
                    request(
                        "GET",
                        "/v1/events",
                        headers={"Last-Event-ID": "event_demo_expired_0001"},
                    ),
                    expected_problem(410, "event_replay_expired"),
                    problem_response(410, "event_replay_expired", "Event replay expired"),
                ),
                step(
                    "invalid",
                    request(
                        "GET", "/v1/events", headers={"Last-Event-ID": "invalid event id"}
                    ),
                    expected_problem(400, "event_id_invalid"),
                    problem_response(400, "event_id_invalid", "Invalid event ID"),
                ),
            ],
        ),
        case(
            "sse-empty-id-reset",
            "1.0.0",
            "events.sse",
            "An empty id field resets the reconnect buffer exactly as defined by SSE.",
            [
                step(
                    "reset",
                    request(
                        "GET",
                        "/v1/events",
                        headers={"Teslatlas-Conformance-Scenario": "empty-id-reset"},
                    ),
                    {
                        "status": 200,
                        "headers_equal": {"Content-Type": "text/event-stream"},
                        "assertions": [
                            {"path": "/last_event_id_after", "op": "is_empty"}
                        ],
                    },
                    {
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/event-stream",
                            "Teslatlas-Protocol-Version": "${profile}",
                        },
                        "last_event_id_after": "",
                    },
                )
            ],
        ),
        case(
            "sse-terminal-204",
            "1.0.0",
            "events.sse",
            "HTTP 204 explicitly tells a conforming event client to stop reconnecting.",
            [
                step(
                    "terminal",
                    request(
                        "GET",
                        "/v1/events",
                        headers={"Teslatlas-Conformance-Scenario": "terminal"},
                    ),
                    {"status": 204, "body_absent": True},
                    {
                        "status": 204,
                        "headers": {
                            "Teslatlas-Protocol-Version": "${profile}"
                        },
                    },
                )
            ],
        ),
        case(
            "documented-limits",
            "1.0.0",
            "query.history",
            "Page, dense-range, request-body, and concurrency limits fail with stable bounded errors.",
            [
                step(
                    "page-size",
                    request("GET", "/v1/vehicles", query={"limit": 501}),
                    expected_problem(400, "invalid_request"),
                    problem_response(400, "invalid_request", "Invalid page limit"),
                ),
                step(
                    "dense-range",
                    request(
                        "GET",
                        "/v1/drives/drive_demo_0001/positions",
                        query={
                            "from": "2026-01-01T00:00:00.000Z",
                            "to": "2026-02-02T00:00:00.000Z",
                        },
                    ),
                    expected_problem(400, "range_too_large"),
                    problem_response(400, "range_too_large", "Range too large"),
                ),
                step(
                    "request-body",
                    request(
                        "POST",
                        "/v1/commands",
                        body=command_request,
                        body_bytes=262145,
                    ),
                    expected_problem(413, "request_too_large"),
                    problem_response(413, "request_too_large", "Request too large"),
                ),
                step(
                    "concurrency",
                    request(
                        "GET",
                        "/v1/vehicles",
                        headers={"Teslatlas-Conformance-Scenario": "concurrency-limit"},
                    ),
                    {
                        **expected_problem(429, "concurrency_limit"),
                        "headers_present": [
                            "Content-Type",
                            "Teslatlas-Protocol-Version",
                            "X-Request-ID",
                            "Retry-After",
                        ],
                    },
                    {
                        **problem_response(
                            429,
                            "concurrency_limit",
                            "Concurrency limit",
                            retryable=True,
                        ),
                        "headers": {
                            "Content-Type": "application/problem+json",
                            "Teslatlas-Protocol-Version": "${profile}",
                            "X-Request-ID": "request_concurrency_limit",
                            "Retry-After": "1",
                        },
                    },
                ),
            ],
        ),
        case(
            "command-idempotency",
            "1.1.0",
            "commands.async",
            "Commands return asynchronous jobs, replay equal idempotency keys, reject conflicts, and avoid blind nuisance retries.",
            [
                step(
                    "create",
                    request(
                        "POST",
                        "/v1/commands",
                        headers={"Idempotency-Key": "11111111-1111-4111-8111-111111111111"},
                        body=command_request,
                    ),
                    {
                        "status": 202,
                        "headers_present": ["Location", "ETag"],
                        "body_schema": schema_definition(COMMAND_SCHEMA, "command_job"),
                        "assertions": [
                            {"path": "/body/state", "op": "equals", "value": "accepted"}
                        ],
                    },
                    {
                        "status": 202,
                        "headers": {
                            **json_headers(ETAG_COMMAND),
                            "Location": "/v1/commands/command_demo_0001",
                        },
                        "body_file": "examples/command-job.json",
                    },
                ),
                step(
                    "replay",
                    request(
                        "POST",
                        "/v1/commands",
                        headers={"Idempotency-Key": "11111111-1111-4111-8111-111111111111"},
                        body=command_request,
                    ),
                    {
                        "status": 202,
                        "body_schema": schema_definition(COMMAND_SCHEMA, "command_job"),
                        "assertions": [
                            {
                                "path": "/body/command_id",
                                "op": "same_as",
                                "ref": "create.body.command_id",
                            }
                        ],
                    },
                    {
                        "status": 202,
                        "headers": {
                            **json_headers(ETAG_COMMAND),
                            "Location": "/v1/commands/command_demo_0001",
                        },
                        "body_file": "examples/command-job.json",
                    },
                ),
                step(
                    "conflict",
                    request(
                        "POST",
                        "/v1/commands",
                        headers={"Idempotency-Key": "11111111-1111-4111-8111-111111111111"},
                        body={
                            **command_request,
                            "parameters": {"percent": 81},
                            "expected_state": {"charge_limit_percent": 81},
                        },
                    ),
                    expected_problem(409, "idempotency_conflict"),
                    problem_response(409, "idempotency_conflict", "Idempotency conflict"),
                ),
                step(
                    "missing-confirmation",
                    request(
                        "POST",
                        "/v1/commands",
                        headers={"Idempotency-Key": "33333333-3333-4333-8333-333333333333"},
                        body={
                            key: value
                            for key, value in command_request.items()
                            if key != "confirmation"
                        },
                    ),
                    expected_problem(400, "invalid_request"),
                    problem_response(
                        400,
                        "invalid_request",
                        "Confirmation required",
                        detail="The advertised command descriptor requires confirmation.",
                    ),
                ),
                step(
                    "nuisance-no-retry",
                    request(
                        "POST",
                        "/v1/commands",
                        headers={"Idempotency-Key": "22222222-2222-4222-8222-222222222222"},
                        body=nuisance_request,
                    ),
                    {
                        "status": 202,
                        "body_schema": schema_definition(COMMAND_SCHEMA, "command_job"),
                        "assertions": [
                            {"path": "/body/retry_policy", "op": "equals", "value": "none"}
                        ],
                    },
                    {
                        "status": 202,
                        "headers": {
                            **json_headers(ETAG_NUISANCE),
                            "Location": "/v1/commands/command_demo_nuisance_0001",
                        },
                        "body": nuisance_job,
                    },
                ),
            ],
        ),
        case(
            "metadata-if-match",
            "1.2.0",
            "metadata.mutable",
            "Mutable metadata uses strong If-Match, increments revisions, and exposes stale or missing preconditions.",
            [
                step(
                    "read",
                    request("GET", "/v1/metadata/metadata_demo_note_0001"),
                    {
                        "status": 200,
                        "headers_present": ["ETag"],
                        "body_schema": schema_definition(METADATA_SCHEMA, "metadata_record"),
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_METADATA_FIRST),
                        "body_file": "examples/metadata-record.json",
                    },
                ),
                step(
                    "update",
                    request(
                        "PUT",
                        "/v1/metadata/metadata_demo_note_0001",
                        headers={"If-Match": "${read.headers.ETag}"},
                        body={"value": {"text": "Updated redacted demonstration trip."}},
                    ),
                    {
                        "status": 200,
                        "headers_present": ["ETag"],
                        "body_schema": schema_definition(METADATA_SCHEMA, "metadata_record"),
                        "assertions": [
                            {"path": "/body/revision", "op": "equals", "value": 2},
                            {
                                "path": "/body/audit/1/action",
                                "op": "equals",
                                "value": "updated",
                            },
                            {
                                "path": "/body/audit/1/revision",
                                "op": "equals",
                                "value": 2,
                            },
                            {
                                "path": "/body/audit/1/previous_hash",
                                "op": "same_as",
                                "ref": "read.body.audit.0.new_hash",
                            },
                            {
                                "path": "/headers/ETag",
                                "op": "not_same_as",
                                "ref": "read.headers.ETag",
                            },
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_METADATA_SECOND),
                        "body": metadata_updated,
                    },
                ),
                step(
                    "stale",
                    request(
                        "PUT",
                        "/v1/metadata/metadata_demo_note_0001",
                        headers={"If-Match": "${read.headers.ETag}"},
                        body={"value": {"text": "Stale value."}},
                    ),
                    expected_problem(409, "metadata_revision_conflict"),
                    problem_response(
                        409, "metadata_revision_conflict", "Metadata revision conflict"
                    ),
                ),
                step(
                    "missing-precondition",
                    request(
                        "PUT",
                        "/v1/metadata/metadata_demo_note_0001",
                        body={"value": {"text": "Missing precondition."}},
                    ),
                    expected_problem(428, "precondition_required"),
                    problem_response(428, "precondition_required", "Precondition required"),
                ),
                step(
                    "delete",
                    request(
                        "DELETE",
                        "/v1/metadata/metadata_demo_note_0001",
                        headers={"If-Match": "${update.headers.ETag}"},
                    ),
                    {
                        "status": 200,
                        "headers_present": ["ETag"],
                        "body_schema": schema_definition(METADATA_SCHEMA, "metadata_tombstone"),
                        "assertions": [
                            {
                                "path": "/body/audit/deletion/revision",
                                "op": "equals",
                                "value": 3,
                            },
                            {
                                "path": "/body/audit/deletion/action",
                                "op": "equals",
                                "value": "deleted",
                            },
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_METADATA_DELETED),
                        "body": metadata_tombstone,
                    },
                ),
                step(
                    "get-tombstone",
                    request("GET", "/v1/metadata/metadata_demo_note_0001"),
                    {
                        "status": 200,
                        "headers_equal": {"ETag": "${delete.headers.ETag}"},
                        "body_schema": schema_definition(METADATA_SCHEMA, "metadata_tombstone"),
                        "assertions": [
                            {
                                "path": "/body/audit/deletion/action",
                                "op": "equals",
                                "value": "deleted",
                            }
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_METADATA_DELETED),
                        "body": metadata_tombstone,
                    },
                ),
                step(
                    "list-excludes-tombstone",
                    request(
                        "GET",
                        "/v1/vehicles/vehicle_demo_alpha/metadata",
                        query={"kind": "note"},
                    ),
                    {
                        "status": 200,
                        "body_schema": schema_definition(RESOURCE_SCHEMA, "metadata_page"),
                        "assertions": [
                            {"path": "/body/items", "op": "is_empty"}
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers('"qB5nD9sK3xM7vT2p"'),
                        "body": metadata_empty_page,
                    },
                ),
                step(
                    "event-tombstone",
                    request(
                        "GET",
                        "/v1/events",
                        headers={
                            "Teslatlas-Conformance-Scenario": "metadata-deleted"
                        },
                        query={"event_type": ["metadata.changed"]},
                    ),
                    {
                        "status": 200,
                        "headers_equal": {"Content-Type": "text/event-stream"},
                        "event_schema": EVENT_SCHEMA,
                        "assertions": [
                            {
                                "path": "/events/0/data/data/audit/deletion/action",
                                "op": "equals",
                                "value": "deleted",
                            }
                        ],
                    },
                    {
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/event-stream",
                            "Teslatlas-Protocol-Version": "${profile}",
                            "Cache-Control": "no-cache",
                        },
                        "events": [
                            {
                                "id": metadata_deleted_event["event_id"],
                                "event": metadata_deleted_event["event_type"],
                                "data": metadata_deleted_event,
                            }
                        ],
                    },
                ),
            ],
        ),
        case(
            "deprecation-sunset",
            "1.2.0",
            "query.vehicles",
            "Deprecated capabilities appear in discovery and responses carry Deprecation, Link, and Sunset metadata.",
            [
                step(
                    "discovery",
                    request(
                        "GET",
                        "/.well-known/teslatlas-hub",
                        headers={"Teslatlas-Conformance-Scenario": "deprecated-capability"},
                    ),
                    {
                        "status": 200,
                        "body_schema": DISCOVERY_SCHEMA,
                        "assertions": [
                            {
                                "path": "/body/capabilities/0/status",
                                "op": "equals",
                                "value": "deprecated",
                            },
                            {
                                "path": "/body/capabilities/0/deprecation/successor",
                                "op": "equals",
                                "value": "query.vehicles.next",
                            },
                        ],
                    },
                    {
                        "status": 200,
                        "headers": json_headers(ETAG_DEPRECATED_DISCOVERY),
                        "body": deprecated_discovery,
                    },
                ),
                step(
                    "response-headers",
                    request(
                        "GET",
                        "/v1/vehicles",
                        headers={"Teslatlas-Conformance-Scenario": "deprecated-capability"},
                    ),
                    {
                        "status": 200,
                        "headers_present": ["Deprecation", "Link", "Sunset"],
                        "headers_equal": {"Deprecation": "@1893456000"},
                        "body_schema": schema_definition(RESOURCE_SCHEMA, "vehicle_page"),
                        "assertions": [
                            {
                                "path": "/headers/Link",
                                "op": "matches",
                                "value": "rel=\\\"deprecation\\\"",
                            },
                            {
                                "path": "/headers/Sunset",
                                "op": "matches",
                                "value": "GMT$",
                            },
                        ],
                    },
                    {
                        "status": 200,
                        "headers": {
                            **json_headers(ETAG_VEHICLES),
                            "Deprecation": "@1893456000",
                            "Sunset": "Mon, 01 Jul 2030 00:00:00 GMT",
                            "Link": "<https://protocol.example.invalid/deprecations/query-vehicles>; rel=\"deprecation\"; type=\"text/html\"",
                        },
                        "body_file": "examples/vehicles-page.json",
                    },
                ),
            ],
        ),
    ]
    return cases


def build_outputs() -> dict[Path, bytes]:
    cases = build_cases()
    outputs = {
        CASE_DIR / f"{item['case_id']}.json": render_bytes(item) for item in cases
    }
    base_cases = [item["case_id"] for item in cases if item["introduced_in"] == "1.0.0"]
    command_cases = [item["case_id"] for item in cases if item["introduced_in"] == "1.1.0"]
    metadata_cases = [item["case_id"] for item in cases if item["introduced_in"] == "1.2.0"]
    profiles = [
        {
            "kind": "profile",
            "protocol_version": "1.0.0",
            "extends": None,
            "capabilities": [
                "query.vehicles",
                "query.history",
                "events.sse",
                "data-quality",
            ],
            "cases": base_cases,
        },
        {
            "kind": "profile",
            "protocol_version": "1.1.0",
            "extends": "1.0.0",
            "capabilities": [
                "query.vehicles",
                "query.history",
                "events.sse",
                "data-quality",
                "commands.async",
            ],
            "cases": [*base_cases, *command_cases],
        },
        {
            "kind": "profile",
            "protocol_version": "1.2.0",
            "extends": "1.1.0",
            "capabilities": [
                "query.vehicles",
                "query.history",
                "events.sse",
                "data-quality",
                "commands.async",
                "metadata.mutable",
            ],
            "cases": [*base_cases, *command_cases, *metadata_cases],
        },
    ]
    for profile in profiles:
        path = COMPATIBILITY_DIR / profile["protocol_version"] / "profile.json"
        outputs[path] = render_bytes(profile)
    manifest = {
        "kind": "manifest",
        "runner_protocol": "teslatlas-conformance/1",
        "current_version": "1.2.0",
        "supported_profiles": ["1.0.0", "1.1.0", "1.2.0"],
        "profiles": [
            {
                "version": profile["protocol_version"],
                "path": f"compatibility/{profile['protocol_version']}/profile.json",
            }
            for profile in profiles
        ],
    }
    outputs[COMPATIBILITY_DIR / "manifest.json"] = render_bytes(manifest)
    return outputs


def render_bytes(value: Any) -> bytes:
    return (
        json.dumps(
            value, indent=2, sort_keys=True, ensure_ascii=False, allow_nan=False
        )
        + "\n"
    ).encode("utf-8")


def check_outputs(outputs: dict[Path, bytes]) -> list[str]:
    errors = []
    for path, expected in outputs.items():
        if not path.is_file():
            errors.append(f"missing: {path.relative_to(ROOT)}")
        elif path.read_bytes() != expected:
            errors.append(f"out of date: {path.relative_to(ROOT)}")
    expected_cases = {path for path in outputs if path.parent == CASE_DIR}
    actual_cases = set(CASE_DIR.glob("*.json")) if CASE_DIR.is_dir() else set()
    for unexpected in sorted(actual_cases - expected_cases):
        errors.append(f"unexpected: {unexpected.relative_to(ROOT)}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description="Build language-neutral conformance vectors")
    parser.add_argument("--check", action="store_true", help="fail if committed vectors differ")
    args = parser.parse_args()
    outputs = build_outputs()
    if args.check:
        errors = check_outputs(outputs)
        if errors:
            print("\n".join(errors))
            return 1
        print("up to date: 11 cases and 3 compatibility profiles")
        return 0
    for path, payload in outputs.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payload)
    print("wrote 11 cases and 3 compatibility profiles")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
