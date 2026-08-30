#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "openapi" / "teslatlas-v1.openapi.json"
SCHEMA_COMPONENTS = {
    "common.schema.json": "Common",
    "discovery.schema.json": "Discovery",
    "observation.schema.json": "Observation",
    "data-quality.schema.json": "DataQuality",
    "resources.schema.json": "Resources",
    "projection.schema.json": "Projection",
    "error.schema.json": "Problem",
    "command.schema.json": "Command",
    "metadata.schema.json": "Metadata",
    "event.schema.json": "Event",
    "fixture.schema.json": "Fixture",
    "sse-contract.schema.json": "SSEContract",
    "conformance.schema.json": "Conformance",
}


def component(kind: str, name: str) -> dict[str, str]:
    return {"$ref": f"#/components/{kind}/{name}"}


def header(name: str) -> dict[str, str]:
    return component("headers", name)


def parameter(name: str) -> dict[str, str]:
    return component("parameters", name)


def response(name: str) -> dict[str, str]:
    return component("responses", name)


def schema_ref(name: str, fragment: str = "") -> str:
    return f"#/components/schemas/{name}{fragment}"


def embedded_schemas() -> dict[str, Any]:
    loaded: dict[str, dict[str, Any]] = {}
    id_to_component: dict[str, str] = {}
    for filename, name in SCHEMA_COMPONENTS.items():
        schema = json.loads((ROOT / "schemas" / filename).read_text(encoding="utf-8"))
        loaded[name] = schema
        id_to_component[schema["$id"]] = name

    def rewrite(value: Any, owner: str) -> Any:
        if isinstance(value, list):
            return [rewrite(item, owner) for item in value]
        if not isinstance(value, dict):
            return value
        rewritten = {
            key: rewrite(item, owner)
            for key, item in value.items()
            if key not in {"$id", "$schema"}
        }
        reference = value.get("$ref")
        if isinstance(reference, str):
            if reference.startswith("#"):
                rewritten["$ref"] = schema_ref(owner, reference[1:])
            else:
                base, separator, fragment = reference.partition("#")
                target = id_to_component.get(base)
                if target is not None:
                    suffix = f"#{fragment}"[1:] if separator else ""
                    rewritten["$ref"] = schema_ref(target, suffix)
        return rewritten

    return {name: rewrite(schema, name) for name, schema in loaded.items()}


def schema_response(
    description: str, schema_ref: str, example_path: str | None = None
) -> dict[str, Any]:
    media: dict[str, Any] = {"schema": {"$ref": schema_ref}}
    if example_path:
        media["examples"] = {
            "redacted": {"externalValue": f"../examples/{example_path}"}
        }
    return {
        "description": description,
        "headers": {
            "ETag": header("ETag"),
            "Teslatlas-Protocol-Version": header("ProtocolVersion"),
            "Cache-Control": header("CacheControl"),
            "Vary": header("Vary"),
            "Deprecation": header("Deprecation"),
            "Sunset": header("Sunset"),
            "Link": header("Link"),
        },
        "content": {"application/json": media},
    }


COMMON_ERRORS = {
    "400": response("Problem"),
    "401": response("Problem"),
    "403": response("Problem"),
    "404": response("Problem"),
    "426": response("Problem"),
    "429": response("Problem"),
}


def conditional_get(
    operation_id: str,
    summary: str,
    response_name: str,
    tag: str,
    extra_parameters: list[dict[str, str]] | None = None,
    public: bool = False,
) -> dict[str, Any]:
    parameters = [] if public else [parameter("ProtocolVersion")]
    parameters.append(parameter("IfNoneMatch"))
    parameters.extend(extra_parameters or [])
    operation: dict[str, Any] = {
        "tags": [tag],
        "summary": summary,
        "operationId": operation_id,
        "parameters": parameters,
        "responses": {
            "200": response(response_name),
            "304": response("NotModified"),
            **COMMON_ERRORS,
        },
    }
    if public:
        operation["security"] = []
    return operation


def paginated_get(
    operation_id: str,
    summary: str,
    response_name: str,
    tag: str,
    *,
    time_bounds: bool = False,
    extra_parameters: list[dict[str, str]] | None = None,
) -> dict[str, Any]:
    parameters = [parameter("Cursor"), parameter("Limit")]
    if time_bounds:
        parameters.extend([parameter("From"), parameter("To")])
    parameters.extend(extra_parameters or [])
    return conditional_get(
        operation_id,
        summary,
        response_name,
        tag,
        extra_parameters=parameters,
    )


def build_document() -> dict[str, Any]:
    limits = json.loads(
        (ROOT / "examples" / "discovery.json").read_text(encoding="utf-8")
    )["limits"]

    headers: dict[str, Any] = {
        "ETag": {
            "description": "Opaque validator for the selected representation.",
            "schema": {
                "type": "string",
                "pattern": "^(?:W/)?\\\"[^\\\"]+\\\"$",
            },
        },
        "ProtocolVersion": {
            "description": "Protocol version selected for this response.",
            "schema": {
                "type": "string",
                "pattern": "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$",
            },
        },
        "CacheControl": {
            "description": "Representation cache policy.",
            "schema": {"type": "string"},
        },
        "Vary": {
            "description": "Request fields that select the representation.",
            "schema": {"type": "string"},
        },
        "Deprecation": {
            "description": "RFC 9745 deprecation instant when applicable.",
            "schema": {"type": "string"},
        },
        "Sunset": {
            "description": "RFC 8594 sunset date when applicable.",
            "schema": {"type": "string"},
        },
        "Link": {
            "description": "Related documentation, including rel=deprecation.",
            "schema": {"type": "string"},
        },
        "Location": {
            "description": "URI of the created asynchronous job or metadata record.",
            "schema": {"type": "string", "format": "uri-reference"},
        },
        "RequestId": {
            "description": "Opaque request correlation identifier.",
            "schema": {"type": "string", "minLength": 3, "maxLength": 128},
        },
        "RetryAfter": {
            "description": "Seconds before a bounded retry.",
            "schema": {"type": "integer", "minimum": 0, "maximum": 86400},
        },
    }

    parameters: dict[str, Any] = {
        "ProtocolVersion": {
            "name": "Teslatlas-Protocol-Version",
            "in": "header",
            "required": False,
            "description": "Highest protocol version understood by the client.",
            "schema": {
                "type": "string",
                "pattern": "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$",
                "default": "1.0.0",
            },
        },
        "IfNoneMatch": {
            "name": "If-None-Match",
            "in": "header",
            "required": False,
            "description": "Opaque cached ETag; GET uses weak comparison.",
            "schema": {"type": "string", "maxLength": 512},
        },
        "IfMatch": {
            "name": "If-Match",
            "in": "header",
            "required": True,
            "description": "Strong ETag of the metadata revision being replaced or deleted.",
            "schema": {"type": "string", "maxLength": 512},
        },
        "IdempotencyKey": {
            "name": "Idempotency-Key",
            "in": "header",
            "required": True,
            "description": "UUID retained for at least the advertised idempotency window.",
            "schema": {"type": "string", "format": "uuid"},
        },
        "Cursor": {
            "name": "cursor",
            "in": "query",
            "required": False,
            "description": "Opaque continuation token. Never decode or edit it.",
            "schema": {
                "type": "string",
                "minLength": 16,
                "maxLength": 2048,
                "pattern": "^[A-Za-z0-9._~-]+$",
            },
        },
        "Limit": {
            "name": "limit",
            "in": "query",
            "required": False,
            "description": "Maximum result count.",
            "schema": {"type": "integer", "minimum": 1, "maximum": 500, "default": 100},
        },
        "From": {
            "name": "from",
            "in": "query",
            "required": False,
            "description": "Inclusive UTC lower bound.",
            "schema": {
                "type": "string",
                "format": "date-time",
                "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\\.[0-9]{3}Z$",
            },
        },
        "To": {
            "name": "to",
            "in": "query",
            "required": False,
            "description": "Exclusive UTC upper bound.",
            "schema": {
                "type": "string",
                "format": "date-time",
                "pattern": "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\\.[0-9]{3}Z$",
            },
        },
        "VehicleId": {
            "name": "vehicle_id",
            "in": "path",
            "required": True,
            "schema": {"type": "string", "minLength": 3, "maxLength": 128},
        },
        "DriveId": {
            "name": "drive_id",
            "in": "path",
            "required": True,
            "schema": {"type": "string", "minLength": 3, "maxLength": 128},
        },
        "ChargeId": {
            "name": "charge_id",
            "in": "path",
            "required": True,
            "schema": {"type": "string", "minLength": 3, "maxLength": 128},
        },
        "CommandId": {
            "name": "command_id",
            "in": "path",
            "required": True,
            "schema": {"type": "string", "minLength": 3, "maxLength": 128},
        },
        "MetadataId": {
            "name": "metadata_id",
            "in": "path",
            "required": True,
            "schema": {"type": "string", "minLength": 3, "maxLength": 128},
        },
        "LastEventId": {
            "name": "Last-Event-ID",
            "in": "header",
            "required": False,
            "description": "Opaque SSE event ID after which replay resumes.",
            "schema": {"type": "string", "maxLength": 2048},
        },
        "EventVehicle": {
            "name": "vehicle_id",
            "in": "query",
            "required": False,
            "schema": {"type": "string", "minLength": 3, "maxLength": 128},
        },
        "EventTypes": {
            "name": "event_type",
            "in": "query",
            "required": False,
            "style": "form",
            "explode": False,
            "schema": {
                "type": "array",
                "maxItems": 32,
                "uniqueItems": True,
                "items": {"type": "string"},
            },
        },
        "MetadataKind": {
            "name": "kind",
            "in": "query",
            "required": False,
            "schema": {"type": "string"},
        },
    }

    responses: dict[str, Any] = {
        "Problem": {
            "description": "RFC 9457 problem detail with a stable Teslatlas code.",
            "headers": {
                "Teslatlas-Protocol-Version": header("ProtocolVersion"),
                "X-Request-ID": header("RequestId"),
                "Retry-After": header("RetryAfter"),
            },
            "content": {
                "application/problem+json": {
                    "schema": {"$ref": schema_ref("Problem")},
                    "examples": {
                        "invalid-cursor": {"externalValue": "../examples/error.json"}
                    },
                }
            },
        },
        "NotModified": {
            "description": "The selected representation matches If-None-Match.",
            "headers": {
                "ETag": header("ETag"),
                "Teslatlas-Protocol-Version": header("ProtocolVersion"),
                "Cache-Control": header("CacheControl"),
                "Vary": header("Vary"),
                "Deprecation": header("Deprecation"),
                "Sunset": header("Sunset"),
                "Link": header("Link"),
            },
        },
        "Discovery": schema_response(
            "Hub discovery metadata.", schema_ref("Discovery"), "discovery.json"
        ),
        "VehiclesPage": schema_response(
            "Vehicles page.",
            schema_ref("Resources", "/$defs/vehicle_page"),
            "vehicles-page.json",
        ),
        "CurrentState": schema_response(
            "Current projected vehicle state.",
            schema_ref("Resources", "/$defs/current_state"),
            "current-state.json",
        ),
        "DrivesPage": schema_response(
            "Drives page.",
            schema_ref("Resources", "/$defs/drive_page"),
            "drives-page.json",
        ),
        "Drive": schema_response(
            "Drive projection.", schema_ref("Resources", "/$defs/drive")
        ),
        "PositionsPage": schema_response(
            "Drive positions page.",
            schema_ref("Resources", "/$defs/position_page"),
            "positions-page.json",
        ),
        "ChargesPage": schema_response(
            "Charging sessions page.",
            schema_ref("Resources", "/$defs/charge_page"),
            "charges-page.json",
        ),
        "Charge": schema_response(
            "Charging-session projection.", schema_ref("Resources", "/$defs/charge")
        ),
        "ChargeSamplesPage": schema_response(
            "Charging samples page.",
            schema_ref("Resources", "/$defs/charge_sample_page"),
            "charge-samples-page.json",
        ),
        "StatesPage": schema_response(
            "Vehicle state intervals page.",
            schema_ref("Resources", "/$defs/state_page"),
            "states-page.json",
        ),
        "UpdatesPage": schema_response(
            "Software updates page.",
            schema_ref("Resources", "/$defs/update_page"),
            "updates-page.json",
        ),
        "DataQualityPage": schema_response(
            "Data-quality assessments page.",
            schema_ref("Resources", "/$defs/data_quality_page"),
            "data-quality-page.json",
        ),
        "CommandJob": schema_response(
            "Asynchronous command job.",
            schema_ref("Command", "/$defs/command_job"),
            "command-job.json",
        ),
        "MetadataPage": schema_response(
            "Mutable metadata page.",
            schema_ref("Resources", "/$defs/metadata_page"),
            "metadata-page.json",
        ),
        "MetadataRecord": schema_response(
            "Mutable metadata record.",
            schema_ref("Metadata", "/$defs/metadata_record"),
            "metadata-record.json",
        ),
    }

    paths: dict[str, Any] = {
        "/.well-known/teslatlas-hub": {
            "get": conditional_get(
                "discoverHub", "Discover this Hub", "Discovery", "Discovery", public=True
            )
        },
        "/v1/vehicles": {
            "get": paginated_get(
                "listVehicles", "List visible vehicles", "VehiclesPage", "Vehicles"
            )
        },
        "/v1/vehicles/{vehicle_id}/current": {
            "parameters": [parameter("VehicleId")],
            "get": conditional_get(
                "getVehicleCurrentState",
                "Get current projected vehicle state",
                "CurrentState",
                "Vehicles",
            ),
        },
        "/v1/vehicles/{vehicle_id}/drives": {
            "parameters": [parameter("VehicleId")],
            "get": paginated_get(
                "listVehicleDrives",
                "List vehicle drives",
                "DrivesPage",
                "Drives",
                time_bounds=True,
            ),
        },
        "/v1/drives/{drive_id}": {
            "parameters": [parameter("DriveId")],
            "get": conditional_get("getDrive", "Get a drive", "Drive", "Drives"),
        },
        "/v1/drives/{drive_id}/positions": {
            "parameters": [parameter("DriveId")],
            "get": paginated_get(
                "listDrivePositions",
                "List drive positions",
                "PositionsPage",
                "Drives",
                time_bounds=True,
            ),
        },
        "/v1/vehicles/{vehicle_id}/charges": {
            "parameters": [parameter("VehicleId")],
            "get": paginated_get(
                "listVehicleCharges",
                "List vehicle charging sessions",
                "ChargesPage",
                "Charges",
                time_bounds=True,
            ),
        },
        "/v1/charges/{charge_id}": {
            "parameters": [parameter("ChargeId")],
            "get": conditional_get("getCharge", "Get a charging session", "Charge", "Charges"),
        },
        "/v1/charges/{charge_id}/samples": {
            "parameters": [parameter("ChargeId")],
            "get": paginated_get(
                "listChargeSamples",
                "List charging samples",
                "ChargeSamplesPage",
                "Charges",
                time_bounds=True,
            ),
        },
        "/v1/vehicles/{vehicle_id}/states": {
            "parameters": [parameter("VehicleId")],
            "get": paginated_get(
                "listVehicleStates",
                "List vehicle state intervals",
                "StatesPage",
                "States",
                time_bounds=True,
            ),
        },
        "/v1/vehicles/{vehicle_id}/updates": {
            "parameters": [parameter("VehicleId")],
            "get": paginated_get(
                "listVehicleUpdates",
                "List software updates",
                "UpdatesPage",
                "Updates",
                time_bounds=True,
            ),
        },
        "/v1/events": {
            "get": {
                "tags": ["Events"],
                "summary": "Open the server-sent event stream",
                "operationId": "streamEvents",
                "parameters": [
                    parameter("ProtocolVersion"),
                    parameter("LastEventId"),
                    parameter("EventVehicle"),
                    parameter("EventTypes"),
                ],
                "responses": {
                    "200": {
                        "description": "UTF-8 SSE stream. Each data value is an event envelope.",
                        "headers": {
                            "Teslatlas-Protocol-Version": header("ProtocolVersion"),
                            "Cache-Control": header("CacheControl"),
                        },
                        "content": {
                            "text/event-stream": {
                                "schema": {"type": "string"},
                                "x-teslatlas-event-contract": "../events/teslatlas-v1.sse.json",
                            }
                        },
                    },
                    "204": {"description": "Stop reconnecting to this stream."},
                    "400": response("Problem"),
                    "401": response("Problem"),
                    "403": response("Problem"),
                    "410": response("Problem"),
                    "426": response("Problem"),
                    "429": response("Problem"),
                },
            }
        },
        "/v1/data-quality": {
            "get": paginated_get(
                "listDataQuality",
                "List visible data-quality assessments",
                "DataQualityPage",
                "Data Quality",
                time_bounds=True,
                extra_parameters=[parameter("EventVehicle")],
            )
        },
        "/v1/commands": {
            "post": {
                "tags": ["Commands"],
                "summary": "Create an asynchronous command job",
                "operationId": "createCommand",
                "parameters": [parameter("ProtocolVersion"), parameter("IdempotencyKey")],
                "requestBody": {
                    "required": True,
                    "content": {
                        "application/json": {
                            "schema": {
                                "$ref": schema_ref("Command", "/$defs/command_request")
                            },
                            "examples": {
                                "redacted": {"externalValue": "../examples/command-request.json"}
                            },
                        }
                    },
                },
                "responses": {
                    "202": {
                        **responses["CommandJob"],
                        "description": "Command accepted for asynchronous authorisation and execution.",
                        "headers": {
                            **responses["CommandJob"]["headers"],
                            "Location": header("Location"),
                        },
                    },
                    "400": response("Problem"),
                    "401": response("Problem"),
                    "403": response("Problem"),
                    "409": response("Problem"),
                    "413": response("Problem"),
                    "426": response("Problem"),
                    "429": response("Problem"),
                },
            }
        },
        "/v1/commands/{command_id}": {
            "parameters": [parameter("CommandId")],
            "get": conditional_get(
                "getCommand", "Get an asynchronous command job", "CommandJob", "Commands"
            ),
        },
        "/v1/vehicles/{vehicle_id}/metadata": {
            "parameters": [parameter("VehicleId")],
            "get": paginated_get(
                "listVehicleMetadata",
                "List mutable metadata",
                "MetadataPage",
                "Metadata",
                extra_parameters=[parameter("MetadataKind")],
            ),
            "post": {
                "tags": ["Metadata"],
                "summary": "Create a mutable metadata record",
                "operationId": "createMetadata",
                "parameters": [parameter("ProtocolVersion")],
                "requestBody": {
                    "required": True,
                    "content": {
                        "application/json": {
                            "schema": {
                                "$ref": schema_ref("Metadata", "/$defs/metadata_create")
                            }
                        }
                    },
                },
                "responses": {
                    "201": {
                        **responses["MetadataRecord"],
                        "headers": {
                            **responses["MetadataRecord"]["headers"],
                            "Location": header("Location"),
                        },
                    },
                    "400": response("Problem"),
                    "401": response("Problem"),
                    "403": response("Problem"),
                    "413": response("Problem"),
                    "426": response("Problem"),
                    "429": response("Problem"),
                },
            },
        },
        "/v1/metadata/{metadata_id}": {
            "parameters": [parameter("MetadataId")],
            "get": conditional_get(
                "getMetadata", "Get a mutable metadata record", "MetadataRecord", "Metadata"
            ),
            "put": {
                "tags": ["Metadata"],
                "summary": "Replace a mutable metadata value",
                "operationId": "replaceMetadata",
                "parameters": [parameter("ProtocolVersion"), parameter("IfMatch")],
                "requestBody": {
                    "required": True,
                    "content": {
                        "application/json": {
                            "schema": {
                                "$ref": schema_ref("Metadata", "/$defs/metadata_replace")
                            }
                        }
                    },
                },
                "responses": {
                    "200": response("MetadataRecord"),
                    "400": response("Problem"),
                    "401": response("Problem"),
                    "403": response("Problem"),
                    "404": response("Problem"),
                    "409": response("Problem"),
                    "413": response("Problem"),
                    "426": response("Problem"),
                    "428": response("Problem"),
                    "429": response("Problem"),
                },
            },
            "delete": {
                "tags": ["Metadata"],
                "summary": "Delete a mutable metadata record",
                "operationId": "deleteMetadata",
                "parameters": [parameter("ProtocolVersion"), parameter("IfMatch")],
                "responses": {
                    "204": {
                        "description": "Metadata deleted.",
                        "headers": {
                            "Teslatlas-Protocol-Version": header("ProtocolVersion")
                        },
                    },
                    "400": response("Problem"),
                    "401": response("Problem"),
                    "403": response("Problem"),
                    "404": response("Problem"),
                    "409": response("Problem"),
                    "426": response("Problem"),
                    "428": response("Problem"),
                    "429": response("Problem"),
                },
            },
        },
    }

    return {
        "openapi": "3.1.1",
        "jsonSchemaDialect": "https://json-schema.org/draft/2020-12/schema",
        "info": {
            "title": "Teslatlas Hub public protocol",
            "version": "1.2.0",
            "description": "Implementation-neutral query, event, command, and metadata boundary.",
            "license": {
                "name": "Apache License 2.0",
                "identifier": "Apache-2.0",
            },
        },
        "servers": [{"url": "https://hub.example.invalid", "description": "Redacted example Hub"}],
        "security": [{"bearerAuth": []}],
        "tags": [
            {"name": "Discovery", "description": "Public Hub identity and capabilities."},
            {"name": "Vehicles", "description": "Vehicles and current state."},
            {"name": "Drives", "description": "Drive projections and positions."},
            {"name": "Charges", "description": "Charge projections and samples."},
            {"name": "States", "description": "Vehicle state intervals."},
            {"name": "Updates", "description": "Software update projections."},
            {"name": "Events", "description": "Server-sent events."},
            {"name": "Data Quality", "description": "Visible completeness and data issues."},
            {"name": "Commands", "description": "Scoped asynchronous command jobs."},
            {"name": "Metadata", "description": "Versioned audited user metadata."},
        ],
        "paths": paths,
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "bearerFormat": "opaque paired-device token",
                    "description": "Credential acquisition and rotation are deployment concerns; never place a token in discovery or fixtures.",
                }
            },
            "headers": headers,
            "parameters": parameters,
            "responses": responses,
            "schemas": embedded_schemas(),
        },
        "x-teslatlas-limits": limits,
        "x-teslatlas-cursors": {
            "opaque": True,
            "url_safe": True,
            "bound_to": [
                "endpoint",
                "normalized_query",
                "principal",
                "authorization_revision",
                "snapshot",
            ],
            "errors": [
                "invalid_cursor",
                "cursor_expired",
                "cursor_query_mismatch",
                "cursor_scope_changed",
            ],
        },
        "x-teslatlas-etags": {
            "validator_of": "representation",
            "if_none_match_comparison": "weak",
            "if_match_comparison": "strong",
            "not_modified_status": 304,
        },
        "x-teslatlas-versioning": {
            "current": "1.2.0",
            "supported": ["1.0.0", "1.1.0", "1.2.0"],
            "selection": "highest-compatible-not-newer-than-client",
            "unsupported_status": 426,
        },
        "x-teslatlas-deprecation": {
            "minimum_later_minor_versions": 2,
            "minimum_days": 180,
            "headers": ["Deprecation", "Sunset", "Link"],
        },
    }


def render() -> str:
    return json.dumps(build_document(), indent=2, ensure_ascii=False) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Build the deterministic OpenAPI document")
    parser.add_argument("--check", action="store_true", help="fail when the committed document differs")
    args = parser.parse_args()
    expected = render()
    if args.check:
        if not OUTPUT.is_file() or OUTPUT.read_text(encoding="utf-8") != expected:
            print(f"out of date: {OUTPUT.relative_to(ROOT)}")
            return 1
        print(f"up to date: {OUTPUT.relative_to(ROOT)}")
        return 0
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(expected, encoding="utf-8")
    print(f"wrote {OUTPUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
