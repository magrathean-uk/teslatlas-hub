// @generated
export const protocolCases: readonly unknown[] = Object.freeze([
  {
    "current_version": "1.2.0",
    "kind": "manifest",
    "profiles": [
      {
        "path": "compatibility/1.0.0/profile.json",
        "version": "1.0.0"
      },
      {
        "path": "compatibility/1.1.0/profile.json",
        "version": "1.1.0"
      },
      {
        "path": "compatibility/1.2.0/profile.json",
        "version": "1.2.0"
      }
    ],
    "runner_protocol": "teslatlas-conformance/1",
    "supported_profiles": [
      "1.0.0",
      "1.1.0",
      "1.2.0"
    ]
  },
  {
    "capabilities": [
      "query.vehicles",
      "query.history",
      "events.sse",
      "data-quality"
    ],
    "cases": [
      "discovery-version-negotiation",
      "cursor-pagination",
      "etag-conditional-get",
      "problem-details",
      "sse-last-event-id",
      "sse-principal-visibility",
      "sse-empty-id-reset",
      "sse-terminal-204",
      "documented-limits"
    ],
    "extends": null,
    "kind": "profile",
    "protocol_version": "1.0.0"
  },
  {
    "capabilities": [
      "query.vehicles",
      "query.history",
      "events.sse",
      "data-quality",
      "commands.async"
    ],
    "cases": [
      "discovery-version-negotiation",
      "cursor-pagination",
      "etag-conditional-get",
      "problem-details",
      "sse-last-event-id",
      "sse-principal-visibility",
      "sse-empty-id-reset",
      "sse-terminal-204",
      "documented-limits",
      "command-idempotency"
    ],
    "extends": "1.0.0",
    "kind": "profile",
    "protocol_version": "1.1.0"
  },
  {
    "capabilities": [
      "query.vehicles",
      "query.history",
      "events.sse",
      "data-quality",
      "commands.async",
      "metadata.mutable"
    ],
    "cases": [
      "discovery-version-negotiation",
      "cursor-pagination",
      "etag-conditional-get",
      "problem-details",
      "sse-last-event-id",
      "sse-principal-visibility",
      "sse-empty-id-reset",
      "sse-terminal-204",
      "documented-limits",
      "command-idempotency",
      "metadata-if-match",
      "deprecation-sunset"
    ],
    "extends": "1.1.0",
    "kind": "profile",
    "protocol_version": "1.2.0"
  },
  {
    "capability": "commands.async",
    "case_id": "command-idempotency",
    "description": "Commands return asynchronous jobs, replay equal idempotency keys, reject conflicts, and avoid blind nuisance retries.",
    "introduced_in": "1.1.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/state",
              "value": "accepted"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:command:1.2.0#/$defs/command_job",
          "headers_present": [
            "Location",
            "ETag"
          ],
          "status": 202
        },
        "reference_response": {
          "body_file": "examples/command-job.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"zQ5dN8wL3pV7tC2m\"",
            "Location": "/v1/commands/command_demo_0001",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 202
        },
        "request": {
          "body": {
            "command": "set_charge_limit",
            "command_class": "charging",
            "confirmation": {
              "confirmed_at": "2026-08-30T12:00:00.000Z",
              "confirmed_by": "user_demo_owner"
            },
            "expected_state": {
              "charge_limit_percent": 80
            },
            "expires_at": "2026-08-30T12:05:00.000Z",
            "parameters": {
              "percent": 80
            },
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Idempotency-Key": "11111111-1111-4111-8111-111111111111",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "POST",
          "path": "/v1/commands",
          "query": {}
        },
        "step_id": "create"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "same_as",
              "path": "/body/command_id",
              "ref": "create.body.command_id"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:command:1.2.0#/$defs/command_job",
          "status": 202
        },
        "reference_response": {
          "body_file": "examples/command-job.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"zQ5dN8wL3pV7tC2m\"",
            "Location": "/v1/commands/command_demo_0001",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 202
        },
        "request": {
          "body": {
            "command": "set_charge_limit",
            "command_class": "charging",
            "confirmation": {
              "confirmed_at": "2026-08-30T12:00:00.000Z",
              "confirmed_by": "user_demo_owner"
            },
            "expected_state": {
              "charge_limit_percent": 80
            },
            "expires_at": "2026-08-30T12:05:00.000Z",
            "parameters": {
              "percent": 80
            },
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Idempotency-Key": "11111111-1111-4111-8111-111111111111",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "POST",
          "path": "/v1/commands",
          "query": {}
        },
        "step_id": "replay"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 409
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "idempotency_conflict"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 409
        },
        "reference_response": {
          "body": {
            "code": "idempotency_conflict",
            "instance": "/requests/request_idempotency_conflict",
            "request_id": "request_idempotency_conflict",
            "retryable": false,
            "status": 409,
            "title": "Idempotency conflict",
            "type": "urn:teslatlas:problem:idempotency-conflict"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_idempotency_conflict"
          },
          "status": 409
        },
        "request": {
          "body": {
            "command": "set_charge_limit",
            "command_class": "charging",
            "confirmation": {
              "confirmed_at": "2026-08-30T12:00:00.000Z",
              "confirmed_by": "user_demo_owner"
            },
            "expected_state": {
              "charge_limit_percent": 81
            },
            "expires_at": "2026-08-30T12:05:00.000Z",
            "parameters": {
              "percent": 81
            },
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Idempotency-Key": "11111111-1111-4111-8111-111111111111",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "POST",
          "path": "/v1/commands",
          "query": {}
        },
        "step_id": "conflict"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 400
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "invalid_request"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 400
        },
        "reference_response": {
          "body": {
            "code": "invalid_request",
            "detail": "The advertised command descriptor requires confirmation.",
            "instance": "/requests/request_invalid_request",
            "request_id": "request_invalid_request",
            "retryable": false,
            "status": 400,
            "title": "Confirmation required",
            "type": "urn:teslatlas:problem:invalid-request"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_invalid_request"
          },
          "status": 400
        },
        "request": {
          "body": {
            "command": "set_charge_limit",
            "command_class": "charging",
            "expected_state": {
              "charge_limit_percent": 80
            },
            "expires_at": "2026-08-30T12:05:00.000Z",
            "parameters": {
              "percent": 80
            },
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Idempotency-Key": "33333333-3333-4333-8333-333333333333",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "POST",
          "path": "/v1/commands",
          "query": {}
        },
        "step_id": "missing-confirmation"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/retry_policy",
              "value": "none"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:command:1.2.0#/$defs/command_job",
          "status": 202
        },
        "reference_response": {
          "body": {
            "attempt_count": 0,
            "audit": [
              {
                "actor_id": "service_hub",
                "at": "2026-08-30T12:00:00.100Z",
                "state": "accepted"
              }
            ],
            "command": "honk_horn",
            "command_class": "nuisance",
            "command_id": "command_demo_nuisance_0001",
            "created_at": "2026-08-30T12:00:00.100Z",
            "expires_at": "2026-08-30T12:05:00.000Z",
            "links": {
              "self": "/v1/commands/command_demo_nuisance_0001"
            },
            "retry_policy": "none",
            "state": "accepted",
            "updated_at": "2026-08-30T12:00:00.100Z",
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"kF2sH6yR9nB4qX7v\"",
            "Location": "/v1/commands/command_demo_nuisance_0001",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 202
        },
        "request": {
          "body": {
            "command": "honk_horn",
            "command_class": "nuisance",
            "confirmation": {
              "confirmed_at": "2026-08-30T12:00:00.000Z",
              "confirmed_by": "user_demo_owner"
            },
            "expected_state": {
              "honk_sequence": 1
            },
            "expires_at": "2026-08-30T12:05:00.000Z",
            "parameters": {},
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Idempotency-Key": "22222222-2222-4222-8222-222222222222",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "POST",
          "path": "/v1/commands",
          "query": {}
        },
        "step_id": "nuisance-no-retry"
      }
    ]
  },
  {
    "capability": "query.history",
    "case_id": "cursor-pagination",
    "description": "Cursors are opaque, query-bound, permission-bound, and explicitly expire.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "exists",
              "path": "/body/next_cursor"
            },
            {
              "op": "exists",
              "path": "/body/items/0/drive_id"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/drive_page",
          "status": 200
        },
        "reference_response": {
          "body_file": "examples/drives-page.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"fN4qL9sB2wH6cM8z\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/drives",
          "query": {
            "from": "2026-08-01T00:00:00.000Z",
            "limit": 1,
            "to": "2026-09-01T00:00:00.000Z"
          }
        },
        "step_id": "initial"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "not_same_as",
              "path": "/body/items/0/drive_id",
              "ref": "initial.body.items.0.drive_id"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/drive_page",
          "status": 200
        },
        "reference_response": {
          "body": {
            "generated_at": "2026-08-31T12:31:00.000Z",
            "items": [
              {
                "distance_km": 24.2,
                "drive_id": "drive_demo_0002",
                "duration_seconds": 1800,
                "efficiency_wh_per_km": 169.4,
                "end_at": "2026-08-31T12:30:00.000Z",
                "end_odometer_km": 12024.7,
                "energy_used_kwh": 4.1,
                "quality": {
                  "assessed_at": "2026-08-31T12:30:00.000Z",
                  "derived_fields": [
                    "energy_used_kwh"
                  ],
                  "gap_count": 1,
                  "issues": [
                    {
                      "affected_fields": [
                        "location",
                        "speed_km_h"
                      ],
                      "code": "missing_interval",
                      "from": "2026-08-30T12:14:00.000Z",
                      "message": "A redacted 47-second telemetry interval is unavailable.",
                      "severity": "warning",
                      "to": "2026-08-30T12:14:47.000Z"
                    }
                  ],
                  "largest_gap_seconds": 47,
                  "projection_version": "3.1.0",
                  "quality": "partial",
                  "sources": [
                    "fleet_telemetry",
                    "fleet_api"
                  ],
                  "subject_id": "drive_demo_0002",
                  "subject_type": "drive"
                },
                "resource_type": "drive",
                "start_at": "2026-08-31T12:00:00.000Z",
                "start_odometer_km": 12000.5,
                "vehicle_id": "vehicle_demo_alpha"
              }
            ],
            "next_cursor": null,
            "resource_type": "drive_page",
            "snapshot_revision": "snapshot_demo_0042"
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"uC7pE3kR9xT5mV2d\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/drives",
          "query": {
            "cursor": "${initial.body.next_cursor}",
            "from": "2026-08-01T00:00:00.000Z",
            "limit": 1,
            "to": "2026-09-01T00:00:00.000Z"
          }
        },
        "step_id": "next"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 409
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "cursor_query_mismatch"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 409
        },
        "reference_response": {
          "body": {
            "code": "cursor_query_mismatch",
            "instance": "/requests/request_cursor_query_mismatch",
            "request_id": "request_cursor_query_mismatch",
            "retryable": false,
            "status": 409,
            "title": "Cursor query mismatch",
            "type": "urn:teslatlas:problem:cursor-query-mismatch"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_cursor_query_mismatch"
          },
          "status": 409
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/drives",
          "query": {
            "cursor": "${initial.body.next_cursor}",
            "from": "2026-08-02T00:00:00.000Z",
            "limit": 1,
            "to": "2026-09-01T00:00:00.000Z"
          }
        },
        "step_id": "query-mismatch"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 410
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "cursor_expired"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 410
        },
        "reference_response": {
          "body": {
            "code": "cursor_expired",
            "instance": "/requests/request_cursor_expired",
            "request_id": "request_cursor_expired",
            "retryable": false,
            "status": 410,
            "title": "Cursor expired",
            "type": "urn:teslatlas:problem:cursor-expired"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_cursor_expired"
          },
          "status": 410
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/drives",
          "query": {
            "cursor": "cursor_demo_expired_0001"
          }
        },
        "step_id": "expired"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 403
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "cursor_scope_changed"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 403
        },
        "reference_response": {
          "body": {
            "code": "cursor_scope_changed",
            "instance": "/requests/request_cursor_scope_changed",
            "request_id": "request_cursor_scope_changed",
            "retryable": false,
            "status": 403,
            "title": "Cursor scope changed",
            "type": "urn:teslatlas:problem:cursor-scope-changed"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_cursor_scope_changed"
          },
          "status": 403
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Authorization-Revision": "2",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/drives",
          "query": {
            "cursor": "${initial.body.next_cursor}"
          }
        },
        "step_id": "scope-changed"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 400
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "invalid_cursor"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 400
        },
        "reference_response": {
          "body": {
            "code": "invalid_cursor",
            "instance": "/requests/request_invalid_cursor",
            "request_id": "request_invalid_cursor",
            "retryable": false,
            "status": 400,
            "title": "Invalid cursor",
            "type": "urn:teslatlas:problem:invalid-cursor"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_invalid_cursor"
          },
          "status": 400
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/drives",
          "query": {
            "cursor": "not-a-valid-token?"
          }
        },
        "step_id": "invalid"
      }
    ]
  },
  {
    "capability": "query.vehicles",
    "case_id": "deprecation-sunset",
    "description": "Deprecated capabilities appear in discovery and responses carry Deprecation, Link, and Sunset metadata.",
    "introduced_in": "1.2.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/capabilities/0/status",
              "value": "deprecated"
            },
            {
              "op": "equals",
              "path": "/body/capabilities/0/deprecation/successor",
              "value": "query.vehicles.next"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:discovery:1.2.0",
          "status": 200
        },
        "reference_response": {
          "body": {
            "capabilities": [
              {
                "deprecation": {
                  "deprecated_at": "2030-01-01T00:00:00.000Z",
                  "documentation": "https://protocol.example.invalid/deprecations/query-vehicles",
                  "successor": "query.vehicles.next",
                  "sunset_at": "2030-07-01T00:00:00.000Z"
                },
                "href": "/v1/vehicles",
                "id": "query.vehicles",
                "introduced_in": "1.0.0",
                "status": "deprecated",
                "version": "1.0.0"
              },
              {
                "href": "/v1/vehicles/{vehicle_id}/drives",
                "id": "query.history",
                "introduced_in": "1.0.0",
                "status": "stable",
                "version": "1.0.0"
              },
              {
                "href": "/v1/events",
                "id": "events.sse",
                "introduced_in": "1.0.0",
                "status": "stable",
                "version": "1.0.0"
              },
              {
                "href": "/v1/data-quality",
                "id": "data-quality",
                "introduced_in": "1.0.0",
                "status": "stable",
                "version": "1.0.0"
              },
              {
                "commands": [
                  {
                    "command_class": "charging",
                    "confirmation_required": true,
                    "expected_state_schema": {
                      "$schema": "https://json-schema.org/draft/2020-12/schema",
                      "additionalProperties": false,
                      "properties": {
                        "charge_limit_percent": {
                          "maximum": 100,
                          "minimum": 50,
                          "type": "integer"
                        }
                      },
                      "required": [
                        "charge_limit_percent"
                      ],
                      "type": "object"
                    },
                    "name": "set_charge_limit",
                    "parameters_schema": {
                      "$schema": "https://json-schema.org/draft/2020-12/schema",
                      "additionalProperties": false,
                      "properties": {
                        "percent": {
                          "maximum": 100,
                          "minimum": 50,
                          "type": "integer"
                        }
                      },
                      "required": [
                        "percent"
                      ],
                      "type": "object"
                    },
                    "required_scope": "vehicle.commands.charging",
                    "retry_policy": "state_verified"
                  },
                  {
                    "command_class": "nuisance",
                    "confirmation_required": true,
                    "expected_state_schema": {
                      "$schema": "https://json-schema.org/draft/2020-12/schema",
                      "additionalProperties": false,
                      "properties": {
                        "honk_sequence": {
                          "const": 1
                        }
                      },
                      "required": [
                        "honk_sequence"
                      ],
                      "type": "object"
                    },
                    "name": "honk_horn",
                    "parameters_schema": {
                      "$schema": "https://json-schema.org/draft/2020-12/schema",
                      "additionalProperties": false,
                      "type": "object"
                    },
                    "required_scope": "vehicle.commands.nuisance",
                    "retry_policy": "none"
                  }
                ],
                "href": "/v1/commands",
                "id": "commands.async",
                "introduced_in": "1.1.0",
                "status": "stable",
                "version": "1.0.0"
              },
              {
                "href": "/v1/vehicles/{vehicle_id}/metadata",
                "id": "metadata.mutable",
                "introduced_in": "1.2.0",
                "status": "stable",
                "version": "1.0.0"
              }
            ],
            "endpoints": {
              "api": "https://hub.example.invalid/v1",
              "events": "https://hub.example.invalid/v1/events",
              "openapi": "https://hub.example.invalid/openapi/teslatlas-v1.openapi.json",
              "well_known": "https://hub.example.invalid/.well-known/teslatlas-hub"
            },
            "hub_id": "urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d42",
            "limits": {
              "default_page_size": 100,
              "event_replay_retention_seconds": 86400,
              "idempotency_retention_seconds": 86400,
              "max_concurrent_requests": 8,
              "max_dense_range_days": 31,
              "max_history_range_days": 366,
              "max_page_size": 500,
              "max_request_body_bytes": 262144,
              "max_sse_connections": 2
            },
            "protocol": {
              "current_version": "1.2.0",
              "minimum_client_version": "1.0.0",
              "selection": "highest-compatible-not-newer-than-client",
              "supported_versions": [
                "1.0.0",
                "1.1.0",
                "1.2.0"
              ],
              "version_header": "Teslatlas-Protocol-Version"
            }
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"dN6qV2tK8xC4mP7s\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Scenario": "deprecated-capability"
          },
          "method": "GET",
          "path": "/.well-known/teslatlas-hub",
          "query": {}
        },
        "step_id": "discovery"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "matches",
              "path": "/headers/Link",
              "value": "rel=\\\"deprecation\\\""
            },
            {
              "op": "matches",
              "path": "/headers/Sunset",
              "value": "GMT$"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/vehicle_page",
          "headers_equal": {
            "Deprecation": "@1893456000"
          },
          "headers_present": [
            "Deprecation",
            "Link",
            "Sunset"
          ],
          "status": 200
        },
        "reference_response": {
          "body_file": "examples/vehicles-page.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "Deprecation": "@1893456000",
            "ETag": "\"aP3xD8mQ5vR2nK7t\"",
            "Link": "<https://protocol.example.invalid/deprecations/query-vehicles>; rel=\"deprecation\"; type=\"text/html\"",
            "Sunset": "Mon, 01 Jul 2030 00:00:00 GMT",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Scenario": "deprecated-capability",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles",
          "query": {}
        },
        "step_id": "response-headers"
      }
    ]
  },
  {
    "capability": "query.vehicles",
    "case_id": "discovery-version-negotiation",
    "description": "Discovery is public, version selection is explicit, and incompatible majors fail closed.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/protocol/current_version",
              "value": "1.2.0"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:discovery:1.2.0",
          "headers_present": [
            "Content-Type",
            "ETag"
          ],
          "status": 200
        },
        "reference_response": {
          "body_file": "examples/discovery.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"y6Kk9pX2mV4qT7sN\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {},
          "method": "GET",
          "path": "/.well-known/teslatlas-hub",
          "query": {}
        },
        "step_id": "discover"
      },
      {
        "expect": {
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/vehicle_page",
          "headers_equal": {
            "Teslatlas-Protocol-Version": "${discover.body.protocol.minimum_client_version}"
          },
          "status": 200
        },
        "reference_response": {
          "body_file": "examples/vehicles-page.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"sH4wQ8nC2kT6mP9x\"",
            "Teslatlas-Protocol-Version": "1.0.0",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {},
          "method": "GET",
          "path": "/v1/vehicles",
          "query": {}
        },
        "step_id": "omitted-version-selects-minimum"
      },
      {
        "expect": {
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/vehicle_page",
          "headers_equal": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "status": 200
        },
        "reference_response": {
          "body_file": "examples/vehicles-page.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"aP3xD8mQ5vR2nK7t\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles",
          "query": {}
        },
        "step_id": "select-profile"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 426
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "unsupported_protocol_version"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 426
        },
        "reference_response": {
          "body": {
            "code": "unsupported_protocol_version",
            "instance": "/requests/request_unsupported_protocol_version",
            "request_id": "request_unsupported_protocol_version",
            "retryable": false,
            "status": 426,
            "title": "Unsupported protocol version",
            "type": "urn:teslatlas:problem:unsupported-protocol-version"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_unsupported_protocol_version"
          },
          "status": 426
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "2.0.0"
          },
          "method": "GET",
          "path": "/v1/vehicles",
          "query": {}
        },
        "step_id": "unsupported-major"
      }
    ]
  },
  {
    "capability": "query.history",
    "case_id": "documented-limits",
    "description": "Page, dense-range, request-body, and concurrency limits fail with stable bounded errors.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 400
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "invalid_request"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 400
        },
        "reference_response": {
          "body": {
            "code": "invalid_request",
            "instance": "/requests/request_invalid_request",
            "request_id": "request_invalid_request",
            "retryable": false,
            "status": 400,
            "title": "Invalid page limit",
            "type": "urn:teslatlas:problem:invalid-request"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_invalid_request"
          },
          "status": 400
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles",
          "query": {
            "limit": 501
          }
        },
        "step_id": "page-size"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 400
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "range_too_large"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 400
        },
        "reference_response": {
          "body": {
            "code": "range_too_large",
            "instance": "/requests/request_range_too_large",
            "request_id": "request_range_too_large",
            "retryable": false,
            "status": 400,
            "title": "Range too large",
            "type": "urn:teslatlas:problem:range-too-large"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_range_too_large"
          },
          "status": 400
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/drives/drive_demo_0001/positions",
          "query": {
            "from": "2026-01-01T00:00:00.000Z",
            "to": "2026-02-02T00:00:00.000Z"
          }
        },
        "step_id": "dense-range"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 413
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "request_too_large"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 413
        },
        "reference_response": {
          "body": {
            "code": "request_too_large",
            "instance": "/requests/request_request_too_large",
            "request_id": "request_request_too_large",
            "retryable": false,
            "status": 413,
            "title": "Request too large",
            "type": "urn:teslatlas:problem:request-too-large"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_request_too_large"
          },
          "status": 413
        },
        "request": {
          "body": {
            "command": "set_charge_limit",
            "command_class": "charging",
            "confirmation": {
              "confirmed_at": "2026-08-30T12:00:00.000Z",
              "confirmed_by": "user_demo_owner"
            },
            "expected_state": {
              "charge_limit_percent": 80
            },
            "expires_at": "2026-08-30T12:05:00.000Z",
            "parameters": {
              "percent": 80
            },
            "vehicle_id": "vehicle_demo_alpha"
          },
          "body_bytes": 262145,
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "POST",
          "path": "/v1/commands",
          "query": {}
        },
        "step_id": "request-body"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 429
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "concurrency_limit"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID",
            "Retry-After"
          ],
          "status": 429
        },
        "reference_response": {
          "body": {
            "code": "concurrency_limit",
            "instance": "/requests/request_concurrency_limit",
            "request_id": "request_concurrency_limit",
            "retryable": true,
            "status": 429,
            "title": "Concurrency limit",
            "type": "urn:teslatlas:problem:concurrency-limit"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Retry-After": "1",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_concurrency_limit"
          },
          "status": 429
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Scenario": "concurrency-limit",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles",
          "query": {}
        },
        "step_id": "concurrency"
      }
    ]
  },
  {
    "capability": "query.vehicles",
    "case_id": "etag-conditional-get",
    "description": "ETags validate representations and a matching If-None-Match returns an empty 304.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/current_state",
          "headers_present": [
            "ETag"
          ],
          "status": 200
        },
        "reference_response": {
          "body_file": "examples/current-state.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"bY8nJ4qW2sF7kL5p\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/current",
          "query": {}
        },
        "step_id": "initial"
      },
      {
        "expect": {
          "body_absent": true,
          "headers_equal": {
            "ETag": "${initial.headers.ETag}"
          },
          "status": 304
        },
        "reference_response": {
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "ETag": "\"bY8nJ4qW2sF7kL5p\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 304
        },
        "request": {
          "headers": {
            "If-None-Match": "${initial.headers.ETag}",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/current",
          "query": {}
        },
        "step_id": "not-modified"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "not_same_as",
              "path": "/headers/ETag",
              "ref": "initial.headers.ETag"
            },
            {
              "op": "equals",
              "path": "/body/revision",
              "value": 43
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/current_state",
          "headers_present": [
            "ETag"
          ],
          "status": 200
        },
        "reference_response": {
          "body": {
            "battery_level_percent": 77,
            "charging_state": "disconnected",
            "climate_on": false,
            "inside_temperature_c": 21.5,
            "location": null,
            "locked": true,
            "observed_at": "2026-08-30T12:01:00.000Z",
            "odometer_km": 12000.5,
            "outside_temperature_c": 17,
            "quality": {
              "assessed_at": "2026-08-30T12:01:01.250Z",
              "derived_fields": [],
              "gap_count": 0,
              "issues": [],
              "largest_gap_seconds": 0,
              "projection_version": "3.1.0",
              "quality": "complete",
              "sources": [
                "fleet_telemetry"
              ],
              "subject_id": "vehicle_demo_alpha",
              "subject_type": "vehicle"
            },
            "range_km": 338.4,
            "resource_type": "current_state",
            "revision": 43,
            "state": "online",
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"rM3vT9cX6gP2hK8w\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Revision": "43",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/current",
          "query": {}
        },
        "step_id": "changed"
      }
    ]
  },
  {
    "capability": "metadata.mutable",
    "case_id": "metadata-if-match",
    "description": "Mutable metadata uses strong If-Match, increments revisions, and exposes stale or missing preconditions.",
    "introduced_in": "1.2.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "body_schema": "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_record",
          "headers_present": [
            "ETag"
          ],
          "status": 200
        },
        "reference_response": {
          "body_file": "examples/metadata-record.json",
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"mC7pL2xW9dR5tN8q\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/metadata/metadata_demo_note_0001",
          "query": {}
        },
        "step_id": "read"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/revision",
              "value": 2
            },
            {
              "op": "equals",
              "path": "/body/audit/1/action",
              "value": "updated"
            },
            {
              "op": "equals",
              "path": "/body/audit/1/revision",
              "value": 2
            },
            {
              "op": "same_as",
              "path": "/body/audit/1/previous_hash",
              "ref": "read.body.audit.0.new_hash"
            },
            {
              "op": "not_same_as",
              "path": "/headers/ETag",
              "ref": "read.headers.ETag"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_record",
          "headers_present": [
            "ETag"
          ],
          "status": 200
        },
        "reference_response": {
          "body": {
            "audit": [
              {
                "action": "created",
                "actor_id": "user_demo_owner",
                "at": "2026-08-30T12:40:00.000Z",
                "new_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345",
                "previous_hash": null,
                "revision": 1
              },
              {
                "action": "updated",
                "actor_id": "user_demo_owner",
                "at": "2026-08-30T12:45:00.000Z",
                "new_hash": "bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc",
                "previous_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345",
                "revision": 2
              }
            ],
            "created_at": "2026-08-30T12:40:00.000Z",
            "created_by": "user_demo_owner",
            "kind": "note",
            "metadata_id": "metadata_demo_note_0001",
            "revision": 2,
            "target": {
              "resource_id": "drive_demo_0001",
              "resource_type": "drive"
            },
            "updated_at": "2026-08-30T12:45:00.000Z",
            "updated_by": "user_demo_owner",
            "value": {
              "text": "Updated redacted demonstration trip."
            },
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"vT4kB8qM2yH6sP9n\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "body": {
            "value": {
              "text": "Updated redacted demonstration trip."
            }
          },
          "headers": {
            "If-Match": "${read.headers.ETag}",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "PUT",
          "path": "/v1/metadata/metadata_demo_note_0001",
          "query": {}
        },
        "step_id": "update"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 409
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "metadata_revision_conflict"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 409
        },
        "reference_response": {
          "body": {
            "code": "metadata_revision_conflict",
            "instance": "/requests/request_metadata_revision_conflict",
            "request_id": "request_metadata_revision_conflict",
            "retryable": false,
            "status": 409,
            "title": "Metadata revision conflict",
            "type": "urn:teslatlas:problem:metadata-revision-conflict"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_metadata_revision_conflict"
          },
          "status": 409
        },
        "request": {
          "body": {
            "value": {
              "text": "Stale value."
            }
          },
          "headers": {
            "If-Match": "${read.headers.ETag}",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "PUT",
          "path": "/v1/metadata/metadata_demo_note_0001",
          "query": {}
        },
        "step_id": "stale"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 428
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "precondition_required"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 428
        },
        "reference_response": {
          "body": {
            "code": "precondition_required",
            "instance": "/requests/request_precondition_required",
            "request_id": "request_precondition_required",
            "retryable": false,
            "status": 428,
            "title": "Precondition required",
            "type": "urn:teslatlas:problem:precondition-required"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_precondition_required"
          },
          "status": 428
        },
        "request": {
          "body": {
            "value": {
              "text": "Missing precondition."
            }
          },
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "PUT",
          "path": "/v1/metadata/metadata_demo_note_0001",
          "query": {}
        },
        "step_id": "missing-precondition"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/audit/deletion/revision",
              "value": 3
            },
            {
              "op": "equals",
              "path": "/body/audit/deletion/action",
              "value": "deleted"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_tombstone",
          "headers_present": [
            "ETag"
          ],
          "status": 200
        },
        "reference_response": {
          "body": {
            "audit": {
              "deletion": {
                "action": "deleted",
                "actor_id": "user_demo_owner",
                "at": "2026-08-30T12:50:00.000Z",
                "revision": 3
              },
              "history": [
                {
                  "action": "created",
                  "actor_id": "user_demo_owner",
                  "at": "2026-08-30T12:40:00.000Z",
                  "new_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345",
                  "previous_hash": null,
                  "revision": 1
                },
                {
                  "action": "updated",
                  "actor_id": "user_demo_owner",
                  "at": "2026-08-30T12:45:00.000Z",
                  "new_hash": "bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc",
                  "previous_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345",
                  "revision": 2
                }
              ]
            },
            "kind": "note",
            "metadata_id": "metadata_demo_note_0001",
            "target": {
              "resource_id": "drive_demo_0001",
              "resource_type": "drive"
            },
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"hR9mX3cF7pL2wQ6d\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "If-Match": "${update.headers.ETag}",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "DELETE",
          "path": "/v1/metadata/metadata_demo_note_0001",
          "query": {}
        },
        "step_id": "delete"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/audit/deletion/action",
              "value": "deleted"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:metadata:1.2.0#/$defs/metadata_tombstone",
          "headers_equal": {
            "ETag": "${delete.headers.ETag}"
          },
          "status": 200
        },
        "reference_response": {
          "body": {
            "audit": {
              "deletion": {
                "action": "deleted",
                "actor_id": "user_demo_owner",
                "at": "2026-08-30T12:50:00.000Z",
                "revision": 3
              },
              "history": [
                {
                  "action": "created",
                  "actor_id": "user_demo_owner",
                  "at": "2026-08-30T12:40:00.000Z",
                  "new_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345",
                  "previous_hash": null,
                  "revision": 1
                },
                {
                  "action": "updated",
                  "actor_id": "user_demo_owner",
                  "at": "2026-08-30T12:45:00.000Z",
                  "new_hash": "bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc",
                  "previous_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345",
                  "revision": 2
                }
              ]
            },
            "kind": "note",
            "metadata_id": "metadata_demo_note_0001",
            "target": {
              "resource_id": "drive_demo_0001",
              "resource_type": "drive"
            },
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"hR9mX3cF7pL2wQ6d\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/metadata/metadata_demo_note_0001",
          "query": {}
        },
        "step_id": "get-tombstone"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "is_empty",
              "path": "/body/items"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/metadata_page",
          "status": 200
        },
        "reference_response": {
          "body": {
            "generated_at": "2026-08-30T12:50:01.000Z",
            "items": [],
            "next_cursor": null,
            "resource_type": "metadata_page",
            "snapshot_revision": "snapshot_demo_metadata_deleted"
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"qB5nD9sK3xM7vT2p\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/metadata",
          "query": {
            "kind": "note"
          }
        },
        "step_id": "list-excludes-tombstone"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/events/0/data/data/audit/deletion/action",
              "value": "deleted"
            }
          ],
          "event_schema": "urn:teslatlas:protocol:schema:event:1.2.0",
          "headers_equal": {
            "Content-Type": "text/event-stream"
          },
          "status": 200
        },
        "reference_response": {
          "events": [
            {
              "data": {
                "data": {
                  "audit": {
                    "deletion": {
                      "action": "deleted",
                      "actor_id": "user_demo_owner",
                      "at": "2026-08-30T12:50:00.000Z",
                      "revision": 3
                    },
                    "history": [
                      {
                        "action": "created",
                        "actor_id": "user_demo_owner",
                        "at": "2026-08-30T12:40:00.000Z",
                        "new_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345",
                        "previous_hash": null,
                        "revision": 1
                      },
                      {
                        "action": "updated",
                        "actor_id": "user_demo_owner",
                        "at": "2026-08-30T12:45:00.000Z",
                        "new_hash": "bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc",
                        "previous_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345",
                        "revision": 2
                      }
                    ]
                  },
                  "kind": "note",
                  "metadata_id": "metadata_demo_note_0001",
                  "target": {
                    "resource_id": "drive_demo_0001",
                    "resource_type": "drive"
                  },
                  "vehicle_id": "vehicle_demo_alpha"
                },
                "event_id": "event_demo_metadata_deleted_0001",
                "event_type": "metadata.changed",
                "occurred_at": "2026-08-30T12:50:00.000Z",
                "resource_id": "metadata_demo_note_0001",
                "revision": 3,
                "vehicle_id": "vehicle_demo_alpha"
              },
              "event": "metadata.changed",
              "id": "event_demo_metadata_deleted_0001"
            }
          ],
          "headers": {
            "Cache-Control": "no-cache",
            "Content-Type": "text/event-stream",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Scenario": "metadata-deleted",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {
            "event_type": [
              "metadata.changed"
            ]
          }
        },
        "step_id": "event-tombstone"
      }
    ]
  },
  {
    "capability": "query.history",
    "case_id": "problem-details",
    "description": "Invalid requests return RFC 9457 problem details with stable codes and ignorable extensions.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 400
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "invalid_time_range"
            },
            {
              "op": "exists",
              "path": "/body/future_extension"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 400
        },
        "reference_response": {
          "body": {
            "code": "invalid_time_range",
            "future_extension": {
              "safe_to_ignore": true
            },
            "instance": "/requests/request_invalid_time_range",
            "request_id": "request_invalid_time_range",
            "retryable": false,
            "status": 400,
            "title": "Invalid time range",
            "type": "urn:teslatlas:problem:invalid-time-range"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_invalid_time_range"
          },
          "status": 400
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/drives",
          "query": {
            "from": "2026-09-01T00:00:00.000Z",
            "to": "2026-08-01T00:00:00.000Z"
          }
        },
        "step_id": "invalid-range"
      }
    ]
  },
  {
    "capability": "events.sse",
    "case_id": "sse-empty-id-reset",
    "description": "An empty id field resets the reconnect buffer exactly as defined by SSE.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "is_empty",
              "path": "/last_event_id_after"
            }
          ],
          "headers_equal": {
            "Content-Type": "text/event-stream"
          },
          "status": 200
        },
        "reference_response": {
          "headers": {
            "Content-Type": "text/event-stream",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "last_event_id_after": "",
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Scenario": "empty-id-reset",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {}
        },
        "step_id": "reset"
      }
    ]
  },
  {
    "capability": "events.sse",
    "case_id": "sse-last-event-id",
    "description": "Last-Event-ID resumes strictly after the named event and expired or invalid IDs are explicit.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "exists",
              "path": "/events/0/id"
            },
            {
              "op": "exists",
              "path": "/events/1/id"
            }
          ],
          "event_schema": "urn:teslatlas:protocol:schema:event:1.2.0",
          "headers_equal": {
            "Content-Type": "text/event-stream"
          },
          "status": 200
        },
        "reference_response": {
          "events": [
            {
              "data": {
                "data": {
                  "battery_level_percent": 78,
                  "charging_state": "disconnected",
                  "climate_on": false,
                  "inside_temperature_c": 21.5,
                  "location": null,
                  "locked": true,
                  "observed_at": "2026-08-30T12:00:00.000Z",
                  "odometer_km": 12000.5,
                  "outside_temperature_c": 17,
                  "quality": {
                    "assessed_at": "2026-08-30T12:00:01.250Z",
                    "derived_fields": [],
                    "gap_count": 0,
                    "issues": [],
                    "largest_gap_seconds": 0,
                    "projection_version": "3.1.0",
                    "quality": "complete",
                    "sources": [
                      "fleet_telemetry"
                    ],
                    "subject_id": "vehicle_demo_alpha",
                    "subject_type": "vehicle"
                  },
                  "range_km": 338.4,
                  "resource_type": "current_state",
                  "revision": 42,
                  "state": "online",
                  "vehicle_id": "vehicle_demo_alpha"
                },
                "event_id": "event_demo_0042",
                "event_type": "vehicle.current.changed",
                "occurred_at": "2026-08-30T12:00:01.250Z",
                "resource_id": "vehicle_demo_alpha",
                "revision": 42,
                "vehicle_id": "vehicle_demo_alpha"
              },
              "event": "vehicle.current.changed",
              "id": "event_demo_0042"
            },
            {
              "data": {
                "data": {
                  "battery_level_percent": 77,
                  "charging_state": "disconnected",
                  "climate_on": false,
                  "inside_temperature_c": 21.5,
                  "location": null,
                  "locked": true,
                  "observed_at": "2026-08-30T12:01:00.000Z",
                  "odometer_km": 12000.5,
                  "outside_temperature_c": 17,
                  "quality": {
                    "assessed_at": "2026-08-30T12:01:01.250Z",
                    "derived_fields": [],
                    "gap_count": 0,
                    "issues": [],
                    "largest_gap_seconds": 0,
                    "projection_version": "3.1.0",
                    "quality": "complete",
                    "sources": [
                      "fleet_telemetry"
                    ],
                    "subject_id": "vehicle_demo_alpha",
                    "subject_type": "vehicle"
                  },
                  "range_km": 338.4,
                  "resource_type": "current_state",
                  "revision": 43,
                  "state": "online",
                  "vehicle_id": "vehicle_demo_alpha"
                },
                "event_id": "event_demo_0043",
                "event_type": "vehicle.current.changed",
                "occurred_at": "2026-08-30T12:01:01.250Z",
                "resource_id": "vehicle_demo_alpha",
                "revision": 43,
                "vehicle_id": "vehicle_demo_alpha"
              },
              "event": "vehicle.current.changed",
              "id": "event_demo_0043"
            }
          ],
          "headers": {
            "Content-Type": "text/event-stream",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "last_event_id_after": "event_demo_0043",
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {}
        },
        "step_id": "initial"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "same_as",
              "path": "/events/0/id",
              "ref": "initial.events.1.id"
            }
          ],
          "event_schema": "urn:teslatlas:protocol:schema:event:1.2.0",
          "status": 200
        },
        "reference_response": {
          "events": [
            {
              "data": {
                "data": {
                  "battery_level_percent": 77,
                  "charging_state": "disconnected",
                  "climate_on": false,
                  "inside_temperature_c": 21.5,
                  "location": null,
                  "locked": true,
                  "observed_at": "2026-08-30T12:01:00.000Z",
                  "odometer_km": 12000.5,
                  "outside_temperature_c": 17,
                  "quality": {
                    "assessed_at": "2026-08-30T12:01:01.250Z",
                    "derived_fields": [],
                    "gap_count": 0,
                    "issues": [],
                    "largest_gap_seconds": 0,
                    "projection_version": "3.1.0",
                    "quality": "complete",
                    "sources": [
                      "fleet_telemetry"
                    ],
                    "subject_id": "vehicle_demo_alpha",
                    "subject_type": "vehicle"
                  },
                  "range_km": 338.4,
                  "resource_type": "current_state",
                  "revision": 43,
                  "state": "online",
                  "vehicle_id": "vehicle_demo_alpha"
                },
                "event_id": "event_demo_0043",
                "event_type": "vehicle.current.changed",
                "occurred_at": "2026-08-30T12:01:01.250Z",
                "resource_id": "vehicle_demo_alpha",
                "revision": 43,
                "vehicle_id": "vehicle_demo_alpha"
              },
              "event": "vehicle.current.changed",
              "id": "event_demo_0043"
            }
          ],
          "headers": {
            "Content-Type": "text/event-stream",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "last_event_id_after": "event_demo_0043",
          "status": 200
        },
        "request": {
          "headers": {
            "Last-Event-ID": "${initial.events.0.id}",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {}
        },
        "step_id": "resume"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 410
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "event_replay_expired"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 410
        },
        "reference_response": {
          "body": {
            "code": "event_replay_expired",
            "instance": "/requests/request_event_replay_expired",
            "request_id": "request_event_replay_expired",
            "retryable": false,
            "status": 410,
            "title": "Event replay expired",
            "type": "urn:teslatlas:problem:event-replay-expired"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_event_replay_expired"
          },
          "status": 410
        },
        "request": {
          "headers": {
            "Last-Event-ID": "event_demo_expired_0001",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {}
        },
        "step_id": "expired"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 400
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "event_id_invalid"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 400
        },
        "reference_response": {
          "body": {
            "code": "event_id_invalid",
            "instance": "/requests/request_event_id_invalid",
            "request_id": "request_event_id_invalid",
            "retryable": false,
            "status": 400,
            "title": "Invalid event ID",
            "type": "urn:teslatlas:problem:event-id-invalid"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_event_id_invalid"
          },
          "status": 400
        },
        "request": {
          "headers": {
            "Last-Event-ID": "invalid event id",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {}
        },
        "step_id": "invalid"
      }
    ]
  },
  {
    "capability": "events.sse",
    "case_id": "sse-principal-visibility",
    "description": "Live events, resource visibility, and replay IDs remain bound to one authenticated principal.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/vehicle_id",
              "value": "vehicle_demo_alpha"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/current_state",
          "status": 200
        },
        "reference_response": {
          "body": {
            "battery_level_percent": 78,
            "charging_state": "disconnected",
            "climate_on": false,
            "inside_temperature_c": 21.5,
            "location": null,
            "locked": true,
            "observed_at": "2026-08-30T12:00:00.000Z",
            "odometer_km": 12000.5,
            "outside_temperature_c": 17,
            "quality": {
              "assessed_at": "2026-08-30T12:00:01.250Z",
              "derived_fields": [],
              "gap_count": 0,
              "issues": [],
              "largest_gap_seconds": 0,
              "projection_version": "3.1.0",
              "quality": "complete",
              "sources": [
                "fleet_telemetry"
              ],
              "subject_id": "vehicle_demo_alpha",
              "subject_type": "vehicle"
            },
            "range_km": 338.4,
            "resource_type": "current_state",
            "revision": 42,
            "state": "online",
            "vehicle_id": "vehicle_demo_alpha"
          },
          "headers": {
            "Cache-Control": "private, max-age=0, must-revalidate",
            "Content-Type": "application/json",
            "ETag": "\"bY8nJ4qW2sF7kL5p\"",
            "Teslatlas-Protocol-Version": "${profile}",
            "Vary": "Authorization, Teslatlas-Protocol-Version"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Principal": "principal-a",
            "Teslatlas-Conformance-Scenario": "principal-visibility",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/current",
          "query": {}
        },
        "step_id": "principal-a-resource"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "same_as",
              "path": "/events/0/data/data",
              "ref": "principal-a-resource.body"
            }
          ],
          "event_schema": "urn:teslatlas:protocol:schema:event:1.2.0",
          "headers_equal": {
            "Content-Type": "text/event-stream"
          },
          "status": 200
        },
        "reference_response": {
          "events": [
            {
              "data": {
                "data": {
                  "battery_level_percent": 78,
                  "charging_state": "disconnected",
                  "climate_on": false,
                  "inside_temperature_c": 21.5,
                  "location": null,
                  "locked": true,
                  "observed_at": "2026-08-30T12:00:00.000Z",
                  "odometer_km": 12000.5,
                  "outside_temperature_c": 17,
                  "quality": {
                    "assessed_at": "2026-08-30T12:00:01.250Z",
                    "derived_fields": [],
                    "gap_count": 0,
                    "issues": [],
                    "largest_gap_seconds": 0,
                    "projection_version": "3.1.0",
                    "quality": "complete",
                    "sources": [
                      "fleet_telemetry"
                    ],
                    "subject_id": "vehicle_demo_alpha",
                    "subject_type": "vehicle"
                  },
                  "range_km": 338.4,
                  "resource_type": "current_state",
                  "revision": 42,
                  "state": "online",
                  "vehicle_id": "vehicle_demo_alpha"
                },
                "event_id": "event_demo_0042",
                "event_type": "vehicle.current.changed",
                "occurred_at": "2026-08-30T12:00:01.250Z",
                "resource_id": "vehicle_demo_alpha",
                "revision": 42,
                "vehicle_id": "vehicle_demo_alpha"
              },
              "event": "vehicle.current.changed",
              "id": "event_demo_0042"
            }
          ],
          "headers": {
            "Cache-Control": "no-cache",
            "Content-Type": "text/event-stream",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "last_event_id_after": "event_demo_0042",
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Principal": "principal-a",
            "Teslatlas-Conformance-Scenario": "principal-visibility",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {
            "vehicle_id": "vehicle_demo_alpha"
          }
        },
        "step_id": "principal-a-live"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 404
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "not_found"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 404
        },
        "reference_response": {
          "body": {
            "code": "not_found",
            "instance": "/requests/request_not_found",
            "request_id": "request_not_found",
            "retryable": false,
            "status": 404,
            "title": "Not found",
            "type": "urn:teslatlas:problem:not-found"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_not_found"
          },
          "status": 404
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Principal": "principal-b",
            "Teslatlas-Conformance-Scenario": "principal-visibility",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/vehicles/vehicle_demo_alpha/current",
          "query": {}
        },
        "step_id": "principal-b-resource"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "is_empty",
              "path": "/events"
            }
          ],
          "event_schema": "urn:teslatlas:protocol:schema:event:1.2.0",
          "headers_equal": {
            "Content-Type": "text/event-stream"
          },
          "status": 200
        },
        "reference_response": {
          "events": [],
          "headers": {
            "Cache-Control": "no-cache",
            "Content-Type": "text/event-stream",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "status": 200
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Principal": "principal-b",
            "Teslatlas-Conformance-Scenario": "principal-visibility",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {
            "vehicle_id": "vehicle_demo_alpha"
          }
        },
        "step_id": "principal-b-live"
      },
      {
        "expect": {
          "assertions": [
            {
              "op": "equals",
              "path": "/body/status",
              "value": 400
            },
            {
              "op": "equals",
              "path": "/body/code",
              "value": "event_id_invalid"
            }
          ],
          "body_schema": "urn:teslatlas:protocol:schema:error:1.2.0",
          "headers_equal": {
            "Content-Type": "application/problem+json"
          },
          "headers_present": [
            "Content-Type",
            "Teslatlas-Protocol-Version",
            "X-Request-ID"
          ],
          "status": 400
        },
        "reference_response": {
          "body": {
            "code": "event_id_invalid",
            "instance": "/requests/request_event_id_invalid",
            "request_id": "request_event_id_invalid",
            "retryable": false,
            "status": 400,
            "title": "Invalid event ID",
            "type": "urn:teslatlas:problem:event-id-invalid"
          },
          "headers": {
            "Content-Type": "application/problem+json",
            "Teslatlas-Protocol-Version": "${profile}",
            "X-Request-ID": "request_event_id_invalid"
          },
          "status": 400
        },
        "request": {
          "headers": {
            "Last-Event-ID": "${principal-a-live.events.0.id}",
            "Teslatlas-Conformance-Principal": "principal-b",
            "Teslatlas-Conformance-Scenario": "principal-visibility",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {
            "vehicle_id": "vehicle_demo_alpha"
          }
        },
        "step_id": "cross-principal-replay"
      }
    ]
  },
  {
    "capability": "events.sse",
    "case_id": "sse-terminal-204",
    "description": "HTTP 204 explicitly tells a conforming event client to stop reconnecting.",
    "introduced_in": "1.0.0",
    "kind": "case",
    "steps": [
      {
        "expect": {
          "body_absent": true,
          "status": 204
        },
        "reference_response": {
          "headers": {
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "status": 204
        },
        "request": {
          "headers": {
            "Teslatlas-Conformance-Scenario": "terminal",
            "Teslatlas-Protocol-Version": "${profile}"
          },
          "method": "GET",
          "path": "/v1/events",
          "query": {}
        },
        "step_id": "terminal"
      }
    ]
  }
]);
export const protocolCaseBodies: Readonly<Record<string, unknown>> = Object.freeze({
  "examples/command-job.json": {
    "command_id": "command_demo_0001",
    "vehicle_id": "vehicle_demo_alpha",
    "command": "set_charge_limit",
    "command_class": "charging",
    "state": "accepted",
    "created_at": "2026-08-30T12:00:00.100Z",
    "updated_at": "2026-08-30T12:00:00.100Z",
    "expires_at": "2026-08-30T12:05:00.000Z",
    "attempt_count": 0,
    "retry_policy": "state_verified",
    "audit": [
      {
        "state": "accepted",
        "at": "2026-08-30T12:00:00.100Z",
        "actor_id": "service_hub"
      }
    ],
    "links": {
      "self": "/v1/commands/command_demo_0001"
    }
  },
  "examples/current-state.json": {
    "resource_type": "current_state",
    "vehicle_id": "vehicle_demo_alpha",
    "observed_at": "2026-08-30T12:00:00.000Z",
    "revision": 42,
    "state": "online",
    "battery_level_percent": 78,
    "range_km": 338.4,
    "odometer_km": 12000.5,
    "inside_temperature_c": 21.5,
    "outside_temperature_c": 17,
    "locked": true,
    "climate_on": false,
    "charging_state": "disconnected",
    "location": null,
    "quality": {
      "subject_type": "vehicle",
      "subject_id": "vehicle_demo_alpha",
      "quality": "complete",
      "sources": [
        "fleet_telemetry"
      ],
      "gap_count": 0,
      "largest_gap_seconds": 0,
      "derived_fields": [],
      "projection_version": "3.1.0",
      "assessed_at": "2026-08-30T12:00:01.250Z",
      "issues": []
    }
  },
  "examples/discovery.json": {
    "hub_id": "urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d42",
    "protocol": {
      "current_version": "1.2.0",
      "supported_versions": [
        "1.0.0",
        "1.1.0",
        "1.2.0"
      ],
      "minimum_client_version": "1.0.0",
      "version_header": "Teslatlas-Protocol-Version",
      "selection": "highest-compatible-not-newer-than-client"
    },
    "capabilities": [
      {
        "id": "query.vehicles",
        "version": "1.0.0",
        "introduced_in": "1.0.0",
        "status": "stable",
        "href": "/v1/vehicles"
      },
      {
        "id": "query.history",
        "version": "1.0.0",
        "introduced_in": "1.0.0",
        "status": "stable",
        "href": "/v1/vehicles/{vehicle_id}/drives"
      },
      {
        "id": "events.sse",
        "version": "1.0.0",
        "introduced_in": "1.0.0",
        "status": "stable",
        "href": "/v1/events"
      },
      {
        "id": "data-quality",
        "version": "1.0.0",
        "introduced_in": "1.0.0",
        "status": "stable",
        "href": "/v1/data-quality"
      },
      {
        "id": "commands.async",
        "version": "1.0.0",
        "introduced_in": "1.1.0",
        "status": "stable",
        "href": "/v1/commands",
        "commands": [
          {
            "name": "set_charge_limit",
            "command_class": "charging",
            "required_scope": "vehicle.commands.charging",
            "retry_policy": "state_verified",
            "confirmation_required": true,
            "parameters_schema": {
              "$schema": "https://json-schema.org/draft/2020-12/schema",
              "type": "object",
              "additionalProperties": false,
              "required": [
                "percent"
              ],
              "properties": {
                "percent": {
                  "type": "integer",
                  "minimum": 50,
                  "maximum": 100
                }
              }
            },
            "expected_state_schema": {
              "$schema": "https://json-schema.org/draft/2020-12/schema",
              "type": "object",
              "additionalProperties": false,
              "required": [
                "charge_limit_percent"
              ],
              "properties": {
                "charge_limit_percent": {
                  "type": "integer",
                  "minimum": 50,
                  "maximum": 100
                }
              }
            }
          },
          {
            "name": "honk_horn",
            "command_class": "nuisance",
            "required_scope": "vehicle.commands.nuisance",
            "retry_policy": "none",
            "confirmation_required": true,
            "parameters_schema": {
              "$schema": "https://json-schema.org/draft/2020-12/schema",
              "type": "object",
              "additionalProperties": false
            },
            "expected_state_schema": {
              "$schema": "https://json-schema.org/draft/2020-12/schema",
              "type": "object",
              "additionalProperties": false,
              "required": [
                "honk_sequence"
              ],
              "properties": {
                "honk_sequence": {
                  "const": 1
                }
              }
            }
          }
        ]
      },
      {
        "id": "metadata.mutable",
        "version": "1.0.0",
        "introduced_in": "1.2.0",
        "status": "stable",
        "href": "/v1/vehicles/{vehicle_id}/metadata"
      }
    ],
    "endpoints": {
      "well_known": "https://hub.example.invalid/.well-known/teslatlas-hub",
      "api": "https://hub.example.invalid/v1",
      "events": "https://hub.example.invalid/v1/events",
      "openapi": "https://hub.example.invalid/openapi/teslatlas-v1.openapi.json"
    },
    "limits": {
      "max_request_body_bytes": 262144,
      "default_page_size": 100,
      "max_page_size": 500,
      "max_history_range_days": 366,
      "max_dense_range_days": 31,
      "max_concurrent_requests": 8,
      "max_sse_connections": 2,
      "event_replay_retention_seconds": 86400,
      "idempotency_retention_seconds": 86400
    }
  },
  "examples/drives-page.json": {
    "resource_type": "drive_page",
    "items": [
      {
        "resource_type": "drive",
        "drive_id": "drive_demo_0001",
        "vehicle_id": "vehicle_demo_alpha",
        "start_at": "2026-08-30T12:00:00.000Z",
        "end_at": "2026-08-30T12:30:00.000Z",
        "start_odometer_km": 12000.5,
        "end_odometer_km": 12024.7,
        "distance_km": 24.2,
        "duration_seconds": 1800,
        "energy_used_kwh": 4.1,
        "efficiency_wh_per_km": 169.4,
        "quality": {
          "subject_type": "drive",
          "subject_id": "drive_demo_0001",
          "quality": "partial",
          "sources": [
            "fleet_telemetry",
            "fleet_api"
          ],
          "gap_count": 1,
          "largest_gap_seconds": 47,
          "derived_fields": [
            "energy_used_kwh"
          ],
          "projection_version": "3.1.0",
          "assessed_at": "2026-08-30T12:30:00.000Z",
          "issues": [
            {
              "code": "missing_interval",
              "severity": "warning",
              "message": "A redacted 47-second telemetry interval is unavailable.",
              "from": "2026-08-30T12:14:00.000Z",
              "to": "2026-08-30T12:14:47.000Z",
              "affected_fields": [
                "location",
                "speed_km_h"
              ]
            }
          ]
        }
      }
    ],
    "next_cursor": "cursor_demo_drives_page_0002",
    "snapshot_revision": "snapshot_demo_0042",
    "generated_at": "2026-08-30T12:31:00.000Z"
  },
  "examples/metadata-record.json": {
    "metadata_id": "metadata_demo_note_0001",
    "vehicle_id": "vehicle_demo_alpha",
    "kind": "note",
    "target": {
      "resource_type": "drive",
      "resource_id": "drive_demo_0001"
    },
    "value": {
      "text": "Redacted demonstration trip."
    },
    "revision": 1,
    "created_at": "2026-08-30T12:40:00.000Z",
    "created_by": "user_demo_owner",
    "updated_at": "2026-08-30T12:40:00.000Z",
    "updated_by": "user_demo_owner",
    "audit": [
      {
        "revision": 1,
        "action": "created",
        "at": "2026-08-30T12:40:00.000Z",
        "actor_id": "user_demo_owner",
        "previous_hash": null,
        "new_hash": "ab2f5c6bd01e7f3a4b2d997f0d7f8b865ede7f0b72d3e4c5a6b7c8d9e0f12345"
      }
    ]
  },
  "examples/vehicles-page.json": {
    "resource_type": "vehicle_page",
    "items": [
      {
        "resource_type": "vehicle",
        "vehicle_id": "vehicle_demo_alpha",
        "display_name": "Demo Vehicle",
        "state": "online",
        "last_observed_at": "2026-08-30T12:00:00.000Z",
        "revision": 42
      }
    ],
    "next_cursor": null,
    "snapshot_revision": "snapshot_demo_0042",
    "generated_at": "2026-08-30T12:00:01.250Z"
  }
});
export const protocolEventCatalog: readonly unknown[] = Object.freeze([
  {
    "name": "observation.admitted",
    "introduced_in": "1.0.0"
  },
  {
    "name": "vehicle.current.changed",
    "introduced_in": "1.0.0"
  },
  {
    "name": "drive.started",
    "introduced_in": "1.0.0"
  },
  {
    "name": "drive.updated",
    "introduced_in": "1.0.0"
  },
  {
    "name": "drive.ended",
    "introduced_in": "1.0.0"
  },
  {
    "name": "charge.started",
    "introduced_in": "1.0.0"
  },
  {
    "name": "charge.updated",
    "introduced_in": "1.0.0"
  },
  {
    "name": "charge.ended",
    "introduced_in": "1.0.0"
  },
  {
    "name": "state.changed",
    "introduced_in": "1.0.0"
  },
  {
    "name": "software_update.changed",
    "introduced_in": "1.0.0"
  },
  {
    "name": "data_quality.changed",
    "introduced_in": "1.0.0"
  },
  {
    "name": "command.changed",
    "introduced_in": "1.1.0"
  },
  {
    "name": "metadata.changed",
    "introduced_in": "1.2.0"
  }
]);
