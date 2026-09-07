#!/usr/bin/env python3
"""Export the independently authored Edge delivery v2 public contract.

All expected identifiers and wire bytes are frozen literals. This generator
does not import Edge or Hub source and does not calculate expected identities.
"""

import argparse
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1] / "profiles/edge-delivery-v2/2.0.0"
PROFILE_ID = "edge-delivery-v2@2.0.0"
DRAFT = "https://json-schema.org/draft/2020-12/schema"
DIGEST = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
I64 = {
    "type": "integer",
    "minimum": -(2**63),
    "maximum": 2**63 - 1,
}

BASE_ID = "8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9"
BASE_ALIAS = "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd"
REPLAY_ALIAS = "5fe1588aca9e75808ce9d1292f7b683e489d9724d5772fae5dfd941714fe0bd1"
CHANGED_ID = "8960ff655b442db799b8e61c949e7b97d0d07454b75f5c7a580a8686a893be14"
LOST_ID = "6b69134908b9c1866e56028a80cc7bfbc88372df17e32489da72ed47d4c9b525"
ALERT_ID = "42424c8baa6532299915b8026625a0dc271769fd97e19647b6366df523866d1f"
ALERT_ALIAS = "e9e9bee52f0bda000acdd63a1d79c245e72bdfeca9c3b9c4932c6f5ec0e8e618"
ERROR_ID = "6f1d19ab8fc5a1b9ec2b6d4b0323a2fd16948b8d3389fdb5fd0a681bd37e06f7"
ERROR_ALIAS = "b13b7660c9d9e9256298660a696961431af720fc16a857a45fe80d51391a6fd3"
CONNECTIVITY_ID = "42b51e71fd1afb779369fbaf3f63cc1ee2741e44bbe5ad412717303a68fe24c2"
CONNECTIVITY_ALIAS = "ecf44df6ac19d8194669379fd1d6dadb7805a951730d4ee01954e0dcaf8a2611"
NOTICE_ID = "73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2"
EVIDENCE_ID = "c2fceaa38e73c41b78386cad195a383e5ae4505db9f71c0e43e7f669a8380e53"
BATCH_ID = "01d91f49aa9e06ae80070976797616a9ec74cf6ef22c6862f615b5a28371a0ef"


def envelope(txid, tx_type, received_at_ms, timestamp_ms, payload):
    return {
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": txid,
        "tx_type": tx_type,
        "received_at_ms": received_at_ms,
        "timestamp_ms": timestamp_ms,
        "payload": payload,
    }


PROJECTED_PAYLOAD = {
    "vin": "5YJ3E1EA7KF000001",
    "createdAt": "2027-01-15T08:00:00Z",
    "data": {"Soc": {"intValue": "80"}},
}
BASE = envelope(
    "edge-projected-0001", "V", 1_800_000_000_100, 1_800_000_000_000, PROJECTED_PAYLOAD
)
REPLAY = dict(BASE, received_at_ms=1_800_000_005_100)
CHANGED = json.loads(json.dumps(BASE))
CHANGED["payload"]["data"]["Soc"]["intValue"] = "81"
LOST = envelope(
    "edge-expired-0002",
    "V",
    1_800_000_000_200,
    1_800_000_000_100,
    {
        "vin": "5YJ3E1EA7KF000001",
        "createdAt": "2027-01-15T08:00:00.100Z",
        "data": {"Soc": {"intValue": "79"}},
    },
)
ALERT = envelope(
    "edge-alert-0003",
    "alerts",
    1_800_000_000_300,
    1_800_000_000_200,
    {"name": "userPresent", "createdAt": "2027-01-15T08:00:00.200Z"},
)
ERROR = envelope(
    "edge-error-0004",
    "errors",
    1_800_000_000_400,
    1_800_000_000_300,
    {"name": "invalidData", "message": "synthetic malformed field"},
)
CONNECTIVITY = envelope(
    "edge-connectivity-0005",
    "connectivity",
    1_800_000_000_500,
    1_800_000_000_400,
    {"status": "connected"},
)


ENVELOPE_SCHEMA = {
    "$schema": DRAFT,
    "$id": "https://protocol.teslatlas.example/profiles/edge-delivery-v2/2.0.0/receiver-envelope.schema.json",
    "title": "Strict Edge receiver envelope retained by delivery v2",
    "type": "object",
    "additionalProperties": False,
    "required": [
        "version",
        "vin",
        "txid",
        "tx_type",
        "received_at_ms",
        "timestamp_ms",
        "payload",
    ],
    "properties": {
        "version": {"const": 1},
        "vin": {"type": "string", "pattern": "^[A-HJ-NPR-Z0-9]{17}$"},
        "txid": {"type": "string", "pattern": "^[!-~]{1,128}$"},
        "tx_type": {
            "type": "string",
            "pattern": "^[!-~]{1,64}$",
            "x-teslatlas-normalize": "remove non-ASCII-alphanumeric bytes, then lowercase",
            "x-teslatlas-accepted-normalized-values": [
                "v",
                "data",
                "vehicledata",
                "connectivity",
                "alerts",
                "errors",
            ],
        },
        "received_at_ms": dict(I64, minimum=946_684_800_000),
        "timestamp_ms": dict(I64, minimum=946_684_800_000),
        "payload": {"type": "object"},
        "device_client_version": {"type": "string", "pattern": "^[!-~]{1,64}$"},
        "firmware_version": {"type": "string", "pattern": "^[!-~]{1,64}$"},
    },
    "x-teslatlas-event-clock-rule": "timestamp_ms <= received_at_ms + 300000",
}


def record_schema():
    return {
        "type": "object",
        "additionalProperties": False,
        "required": ["record_id", "legacy_record_id", "spool_seq", "received_at_ms", "envelope"],
        "properties": {
            "record_id": DIGEST,
            "legacy_record_id": DIGEST,
            "spool_seq": {"type": "integer", "minimum": 1, "maximum": 2**64 - 1},
            "received_at_ms": I64,
            "envelope": ENVELOPE_SCHEMA,
        },
    }


def gap_schema():
    return {
        "type": "object",
        "additionalProperties": False,
        "required": ["notice_id", "spool_seq", "occurred_at_ms", "reason", "evidence_sha256"],
        "properties": {
            "notice_id": DIGEST,
            "spool_seq": {"type": "integer", "minimum": 1, "maximum": 2**64 - 1},
            "occurred_at_ms": I64,
            "reason": {"enum": ["retention_expired", "integrity_quarantine"]},
            "evidence_sha256": DIGEST,
        },
    }


DELIVERY_SCHEMA = {
    "$schema": DRAFT,
    "$id": "https://protocol.teslatlas.example/profiles/edge-delivery-v2/2.0.0/delivery.schema.json",
    "title": "Edge delivery v2 batch and acknowledgement messages",
    "$defs": {
        "batch_v2": {
            "type": "object",
            "additionalProperties": False,
            "required": ["version", "batch_id", "records", "gaps"],
            "properties": {
                "version": {"const": 2},
                "batch_id": DIGEST,
                "records": {"type": "array", "maxItems": 1024, "items": record_schema()},
                "gaps": {"type": "array", "maxItems": 1024, "items": gap_schema()},
            },
            "x-teslatlas-max-total-items": 1024,
            "x-teslatlas-max-encoded-item-bytes-sum": 4_194_304,
            "x-teslatlas-max-encoded-response-bytes": 4_195_441,
            "x-teslatlas-distinct-spool-sequences": True,
            "x-teslatlas-distinct-record-and-notice-identities": True,
        },
        "ack_v2": {
            "type": "object",
            "additionalProperties": False,
            "required": ["version", "batch_id", "accepted_record_ids", "accepted_gap_notice_ids"],
            "properties": {
                "version": {"const": 2},
                "batch_id": DIGEST,
                "accepted_record_ids": {
                    "type": "array",
                    "maxItems": 256,
                    "uniqueItems": True,
                    "items": DIGEST,
                },
                "accepted_gap_notice_ids": {
                    "type": "array",
                    "maxItems": 256,
                    "uniqueItems": True,
                    "items": DIGEST,
                },
            },
            "x-teslatlas-max-encoded-bytes": 131_072,
            "x-teslatlas-accepted-items": "one contiguous prefix of merged batch spool_seq order",
        },
        "ack_result_v2": {
            "type": "object",
            "additionalProperties": False,
            "required": [
                "version",
                "acknowledged_record_ids",
                "acknowledged_gap_notice_ids",
                "unknown_record_ids",
                "unknown_gap_notice_ids",
            ],
            "properties": {
                "version": {"const": 2},
                "acknowledged_record_ids": {"type": "array", "items": DIGEST},
                "acknowledged_gap_notice_ids": {"type": "array", "items": DIGEST},
                "unknown_record_ids": {"type": "array", "items": DIGEST},
                "unknown_gap_notice_ids": {"type": "array", "items": DIGEST},
            },
        },
        "error": {
            "type": "object",
            "additionalProperties": False,
            "required": ["error"],
            "properties": {"error": {"type": "string", "minLength": 1, "maxLength": 128}},
        },
    },
}


DISPOSITION_SCHEMA = {
    "$schema": DRAFT,
    "$id": "https://protocol.teslatlas.example/profiles/edge-delivery-v2/2.0.0/consumer-disposition.schema.json",
    "title": "Durable Hub consumer disposition evidence",
    "description": "A source-neutral persistence contract; this object is not sent to Edge.",
    "type": "object",
    "additionalProperties": False,
    "required": [
        "edge_installation_id",
        "edge_lineage",
        "spool_seq",
        "item_id",
        "item_kind",
        "category",
        "reason",
        "committed_at_ms",
    ],
    "properties": {
        "edge_installation_id": {"type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,127}$"},
        "edge_lineage": {"type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,127}$"},
        "spool_seq": {"type": "integer", "minimum": 1, "maximum": 2**64 - 1},
        "item_id": DIGEST,
        "item_kind": {"enum": ["record", "gap"]},
        "category": {
            "enum": [
                "projected_telemetry",
                "durable_non_projection_event",
                "durable_gap",
                "duplicate",
                "rejected_invalid_envelope",
            ]
        },
        "reason": {"type": "string", "pattern": "^[a-z0-9][a-z0-9_]{0,127}$"},
        "legacy_record_id": DIGEST,
        "payload_sha256": DIGEST,
        "occurred_at_ms": I64,
        "evidence_sha256": DIGEST,
        "committed_at_ms": I64,
    },
    "allOf": [
        {
            "if": {"properties": {"item_kind": {"const": "record"}}},
            "then": {
                "required": ["legacy_record_id", "payload_sha256"],
                "properties": {
                    "category": {
                        "enum": [
                            "projected_telemetry",
                            "durable_non_projection_event",
                            "duplicate",
                            "rejected_invalid_envelope",
                        ]
                    }
                },
                "not": {
                    "anyOf": [
                        {"required": ["occurred_at_ms"]},
                        {"required": ["evidence_sha256"]},
                    ]
                },
            },
        },
        {
            "if": {"properties": {"item_kind": {"const": "gap"}}},
            "then": {
                "required": ["occurred_at_ms", "evidence_sha256"],
                "properties": {"category": {"const": "durable_gap"}},
                "not": {
                    "anyOf": [
                        {"required": ["legacy_record_id"]},
                        {"required": ["payload_sha256"]},
                    ]
                },
            },
        },
    ],
}


GAP = {
    "notice_id": NOTICE_ID,
    "spool_seq": 11,
    "occurred_at_ms": 1_800_000_060_200,
    "reason": "retention_expired",
    "evidence_sha256": EVIDENCE_ID,
}
BASE_RECORD = {
    "record_id": BASE_ID,
    "legacy_record_id": BASE_ALIAS,
    "spool_seq": 10,
    "received_at_ms": 1_800_000_000_100,
    "envelope": BASE,
}
ALERT_RECORD = {
    "record_id": ALERT_ID,
    "legacy_record_id": ALERT_ALIAS,
    "spool_seq": 12,
    "received_at_ms": 1_800_000_000_300,
    "envelope": ALERT,
}
BATCH = {"version": 2, "batch_id": BATCH_ID, "records": [BASE_RECORD, ALERT_RECORD], "gaps": [GAP]}


def ack_prefix(record_ids, gap_ids):
    request = {
        "version": 2,
        "batch_id": BATCH_ID,
        "accepted_record_ids": record_ids,
        "accepted_gap_notice_ids": gap_ids,
    }
    response = {
        "version": 2,
        "acknowledged_record_ids": record_ids,
        "acknowledged_gap_notice_ids": gap_ids,
        "unknown_record_ids": [],
        "unknown_gap_notice_ids": [],
    }
    return {
        "request": request,
        "request_body_utf8": json.dumps(request, separators=(",", ":")),
        "response": response,
        "response_body_utf8": json.dumps(response, separators=(",", ":")),
    }


PREFIXES = [
    dict(ack_prefix([], []), through_spool_seq=None),
    dict(ack_prefix([BASE_ID], []), through_spool_seq=10),
    dict(ack_prefix([BASE_ID], [NOTICE_ID]), through_spool_seq=11),
    dict(ack_prefix([BASE_ID, ALERT_ID], [NOTICE_ID]), through_spool_seq=12),
]
STALE_ACK = {
    "version": 2,
    "batch_id": "0" * 64,
    "accepted_record_ids": [BASE_ID],
    "accepted_gap_notice_ids": [],
}
NON_CONTIGUOUS_ACK = {
    "version": 2,
    "batch_id": BATCH_ID,
    "accepted_record_ids": [ALERT_ID],
    "accepted_gap_notice_ids": [],
}
DUPLICATE_KEY_ACK = (
    '{"version":2,"batch_id":"'
    + BATCH_ID
    + '","batch_id":"'
    + BATCH_ID
    + '","accepted_record_ids":[],"accepted_gap_notice_ids":[]}'
)
OVERSIZED_BATCH = {
    "version": 2,
    "batch_id": BATCH_ID,
    "records": [BASE_RECORD] * 1024 + [ALERT_RECORD],
    "gaps": [],
}


PROFILE = {
    "profile_id": PROFILE_ID,
    "profile_name": "edge-delivery-v2",
    "revision": "2.0.0",
    "status": "candidate",
    "license_boundary": "Apache-2.0 source-neutral public contract",
    "routes": [
        {"method": "GET", "path": "/v2/hub/batches/next", "success_status": 200},
        {"method": "POST", "path": "/v2/hub/acks", "success_status": 200},
    ],
    "authentication": {
        "tls": "mutual TLS with an Edge-configured dedicated Hub client CA",
        "authorization": "Authorization: Bearer tte1.<credential-id>.<secret>",
        "requirements_are_conjunctive": True,
    },
    "retained_wire_versions": [1, 2],
    "spool_format": 2,
    "selection": {"required_for_hub_consumer": 2, "automatic_v1_downgrade": False},
    "limits": {
        "receiver_body_bytes": 262_144,
        "ack_body_bytes": 131_072,
        "accepted_record_ids": 256,
        "accepted_gap_notice_ids": 256,
        "batch_max_items_default": 256,
        "batch_max_items_hard": 1024,
        "batch_max_bytes_default": 1_048_576,
        "batch_max_bytes_hard": 4_194_304,
        "batch_max_bytes_measurement": "sum_of_compact_json_encoded_records_and_gaps",
        "batch_response_body_bytes_hard": 4_195_441,
    },
    "identity": {
        "canonicalization": "RFC 8785 JSON Canonicalization Scheme",
        "stable_record_id": "lower_hex(SHA-256(hex_decode(hash_parameters.domains.stable_record_id.bytes_hex) || JCS(stable_identity)))",
        "stable_identity_fields": [
            "version=2",
            "vin",
            "txid",
            "tx_type",
            "timestamp_ms",
            "payload",
            "device_client_version when present",
            "firmware_version when present",
        ],
        "excluded_stable_identity_fields": ["received_at_ms"],
        "legacy_record_id": "lower_hex(SHA-256(hex_decode(hash_parameters.domains.legacy_record_id.bytes_hex) || JCS(receiver_envelope)))",
        "gap_evidence": "lower_hex(SHA-256(hex_decode(hash_parameters.domains.gap_evidence.bytes_hex) || spool_seq_u64_be || stable_record_id_utf8 || reason_utf8))",
        "gap_notice": "lower_hex(SHA-256(hex_decode(hash_parameters.domains.gap_notice.bytes_hex) || spool_seq_u64_be || reason_utf8 || evidence_sha256_utf8))",
        "batch": "lower_hex(SHA-256(hex_decode(hash_parameters.domains.batch.bytes_hex) || each merged item: hex_decode(hash_parameters.batch_kind_bytes[item.kind].hex) || spool_seq_u64_be || item_id_utf8 || hex_decode(hash_parameters.batch_item_terminator_hex)))",
    },
    "hash_parameters": {
        "domains": {
            "legacy_record_id": {
                "prefix_utf8": "teslatlas-edge-record-v1",
                "terminal_hex": "00",
                "bytes_hex": "7465736c61746c61732d656467652d7265636f72642d763100",
            },
            "stable_record_id": {
                "prefix_utf8": "teslatlas-edge-record-v2",
                "terminal_hex": "00",
                "bytes_hex": "7465736c61746c61732d656467652d7265636f72642d763200",
            },
            "gap_evidence": {
                "prefix_utf8": "teslatlas-edge-gap-evidence-v2",
                "terminal_hex": "00",
                "bytes_hex": "7465736c61746c61732d656467652d6761702d65766964656e63652d763200",
            },
            "gap_notice": {
                "prefix_utf8": "teslatlas-edge-gap-notice-v2",
                "terminal_hex": "00",
                "bytes_hex": "7465736c61746c61732d656467652d6761702d6e6f746963652d763200",
            },
            "batch": {
                "prefix_utf8": "teslatlas-edge-batch-v2",
                "terminal_hex": "00",
                "bytes_hex": "7465736c61746c61732d656467652d62617463682d763200",
            },
        },
        "hex_encoding": "bytes_hex and every field ending in _hex represent bytes decoded from lowercase hexadecimal; the hexadecimal text itself is never hashed.",
        "domain_termination": "Each decoded bytes_hex value contains exactly one terminal NUL byte (hex 00); no separator is added after it.",
        "batch_kind_bytes": {
            "record": {"ascii": "r", "hex": "72"},
            "gap": {"ascii": "g", "hex": "67"},
        },
        "spool_seq_u64": {"byte_order": "big", "width_bytes": 8},
        "batch_item_terminator_hex": "00",
    },
    "timestamp_semantics": {
        "batch_record_received_at_ms": "edge_durable_admission_time",
        "envelope_received_at_ms": "receiver_supplied_arrival_time_in_v1_alias_only",
        "envelope_timestamp_ms": "telemetry_event_time",
    },
    "sequence": {
        "ordering": "ascending spool_seq after merging records and gaps",
        "uniqueness": "one item per spool_seq across both arrays",
        "acknowledgement": "accepted IDs form exactly one contiguous prefix; empty prefix is valid",
        "replay": "same stable ID at a later spool_seq is a durable duplicate disposition with its new alias, then ACK eligible",
        "lineage": "sequence reset or spool replacement requires a new explicitly configured Edge lineage",
    },
    "consumer_commit": {
        "rule": "receipt, sequence disposition, aliases, Hub data effect and recoverable derived-export work commit atomically before ACK",
        "valid_non_projection": "persist bounded identity, reason and payload digest before ACK",
        "invalid": "authentication, identity, alias, sequence or envelope conflict is visible and cannot advance the ACK prefix",
    },
    "schemas": [
        "receiver-envelope.schema.json",
        "delivery.schema.json",
        "consumer-disposition.schema.json",
    ],
    "vectors": "vectors.json",
    "examples": [
        {"kind": "receiver-envelope", "path": "examples/projected-envelope.json"},
        {"kind": "receiver-envelope", "path": "examples/alert-envelope.json"},
        {"kind": "receiver-envelope", "path": "examples/error-envelope.json"},
        {"kind": "batch-v2", "path": "examples/batch.json"},
        {"kind": "ack-v2", "path": "examples/ack-prefix.json"},
        {"kind": "ack-result-v2", "path": "examples/ack-result.json"},
        {"kind": "consumer-disposition", "path": "examples/non-projection-disposition.json"},
        {"kind": "consumer-gap-disposition", "path": "examples/gap-disposition.json"},
    ],
}


OPENAPI = {
    "openapi": "3.1.0",
    "info": {"title": "Teslatlas Edge delivery v2", "version": "2.0.0"},
    "components": {
        "securitySchemes": {
            "hubMutualTLS": {"type": "mutualTLS"},
            "edgeBearer": {"type": "http", "scheme": "bearer"},
        }
    },
    "security": [{"hubMutualTLS": [], "edgeBearer": []}],
    "paths": {
        "/v2/hub/batches/next": {
            "get": {
                "operationId": "nextEdgeDeliveryBatchV2",
                "responses": {
                    "200": {
                        "description": "Current batch, including an empty batch",
                        "content": {"application/json": {"schema": DELIVERY_SCHEMA["$defs"]["batch_v2"]}},
                    },
                    "401": {"description": "Missing, invalid, expired or revoked bearer"},
                    "503": {"description": "Spool or credential state unavailable"},
                },
            }
        },
        "/v2/hub/acks": {
            "post": {
                "operationId": "acknowledgeEdgeDeliveryBatchV2",
                "requestBody": {
                    "required": True,
                    "content": {"application/json": {"schema": DELIVERY_SCHEMA["$defs"]["ack_v2"]}},
                    "x-teslatlas-max-encoded-bytes": 131_072,
                },
                "responses": {
                    "200": {
                        "description": "Persisted acknowledgement receipt and exact deletion result",
                        "content": {"application/json": {"schema": DELIVERY_SCHEMA["$defs"]["ack_result_v2"]}},
                    },
                    "400": {"description": "Invalid, non-prefix or stale acknowledgement"},
                    "401": {"description": "Missing, invalid, expired or revoked bearer"},
                    "413": {"description": "Request body exceeds the fixed limit"},
                    "503": {"description": "Spool or credential state unavailable"},
                },
            }
        },
    },
}


VECTORS = {
    "profile_id": PROFILE_ID,
    "identity": {
        "base_envelope": BASE,
        "stable_record_id": BASE_ID,
        "legacy_record_id": BASE_ALIAS,
        "changed_arrival_envelope": REPLAY,
        "changed_arrival_stable_record_id": BASE_ID,
        "changed_arrival_legacy_record_id": REPLAY_ALIAS,
        "changed_payload_envelope": CHANGED,
        "changed_payload_stable_record_id": CHANGED_ID,
    },
    "re_enqueued_after_completed_ack": {
        "spool_seq": 13,
        "record_id": BASE_ID,
        "legacy_record_id": REPLAY_ALIAS,
        "envelope": REPLAY,
        "expected_disposition": "duplicate",
        "application_count_after_commit": 1,
        "ack_eligible_after_atomic_commit": True,
    },
    "gap": {"lost_envelope": LOST, "lost_stable_record_id": LOST_ID, "notice": GAP},
    "sequence": {
        "batch": BATCH,
        "merged_order": [
            {"spool_seq": 10, "kind": "record", "id": BASE_ID},
            {"spool_seq": 11, "kind": "gap", "id": NOTICE_ID},
            {"spool_seq": 12, "kind": "record", "id": ALERT_ID},
        ],
        "ack_prefixes": PREFIXES,
    },
    "non_projection_envelopes": [
        {"kind": "alerts", "record_id": ALERT_ID, "legacy_record_id": ALERT_ALIAS, "envelope": ALERT, "payload_sha256": "2b8aa76b27b1b29810087e1692c1efcfe3b04e340be83700464570f0924e8fd7", "expected_disposition": "durable_non_projection_event", "reason": "alert_event_retained", "ack_eligible_after_atomic_commit": True},
        {"kind": "errors", "record_id": ERROR_ID, "legacy_record_id": ERROR_ALIAS, "envelope": ERROR, "payload_sha256": "215c31ed5e908b2375188f04c103c1f304364386e843a179006487c9cad0c250", "expected_disposition": "durable_non_projection_event", "reason": "error_event_retained", "ack_eligible_after_atomic_commit": True},
        {"kind": "connectivity", "record_id": CONNECTIVITY_ID, "legacy_record_id": CONNECTIVITY_ALIAS, "envelope": CONNECTIVITY, "payload_sha256": "d94c7553aab24fd59e0342acb897099ebf83dc9f84a28791a05592f3d0772d07", "expected_disposition": "durable_non_projection_event", "reason": "connectivity_event_retained", "ack_eligible_after_atomic_commit": True},
    ],
    "dispositions": [
        {
            "category": "projected_telemetry",
            "record_id": BASE_ID,
            "reason": "fleet_protojson_projected",
            "required_durable_fields": ["edge_installation_id", "edge_lineage", "spool_seq", "record_id", "legacy_record_id", "payload_sha256"],
            "receipt_transaction": "same_as_hub_data_effect",
            "ack_eligible_after_commit": True,
        },
        {
            "category": "durable_non_projection_event",
            "record_id": ALERT_ID,
            "reason": "alert_event_retained",
            "required_durable_fields": ["edge_installation_id", "edge_lineage", "spool_seq", "record_id", "legacy_record_id", "reason", "payload_sha256"],
            "receipt_transaction": "same_as_hub_data_effect",
            "ack_eligible_after_commit": True,
        },
        {
            "category": "durable_gap",
            "record_id": NOTICE_ID,
            "reason": "retention_expired",
            "required_durable_fields": ["edge_installation_id", "edge_lineage", "spool_seq", "notice_id", "reason", "occurred_at_ms", "evidence_sha256"],
            "receipt_transaction": "same_as_hub_data_effect",
            "ack_eligible_after_commit": True,
        },
        {
            "category": "duplicate",
            "record_id": BASE_ID,
            "reason": "stable_record_already_applied",
            "required_durable_fields": ["edge_installation_id", "edge_lineage", "spool_seq", "record_id", "legacy_record_id", "payload_sha256"],
            "receipt_transaction": "same_as_hub_data_effect",
            "ack_eligible_after_commit": True,
        },
        {
            "category": "rejected_invalid_envelope",
            "record_id": BASE_ID,
            "reason": "identity_mismatch",
            "required_durable_fields": ["visible_redacted_failure"],
            "receipt_transaction": "none",
            "ack_eligible_after_commit": False,
            "ack_prefix_effect": "no_advance",
        },
    ],
    "negative_cases": [
        {"id": "missing-client-certificate", "expected_status": "tls_failure", "ack_prefix_effect": "no_advance"},
        {"id": "missing-or-invalid-bearer", "expected_status": 401, "ack_prefix_effect": "no_advance"},
        {"id": "stale-batch-correlation", "expected_status": 400, "batch_id": "0" * 64, "request": STALE_ACK, "wire_path": "negative/stale-ack.json", "ack_prefix_effect": "no_advance"},
        {"id": "duplicate-json-key", "expected_status": 400, "request_body_utf8": DUPLICATE_KEY_ACK, "wire_path": "negative/duplicate-key-ack.json", "ack_prefix_effect": "no_advance"},
        {"id": "duplicate-accepted-id", "expected_status": 400, "accepted_record_ids": [BASE_ID, BASE_ID], "ack_prefix_effect": "no_advance"},
        {"id": "duplicate-record-id", "expected_consumer_result": "reject_batch", "conflicting_spool_sequences": [10, 12], "record_id": BASE_ID, "ack_prefix_effect": "no_advance"},
        {"id": "record-alias-conflict", "expected_consumer_result": "reject_batch", "record_id": BASE_ID, "envelope": BASE, "expected_legacy_record_id": BASE_ALIAS, "presented_legacy_record_id": REPLAY_ALIAS, "ack_prefix_effect": "no_advance"},
        {"id": "non-contiguous-prefix", "expected_status": 400, "request": NON_CONTIGUOUS_ACK, "wire_path": "negative/non-contiguous-ack.json", "ack_prefix_effect": "no_advance"},
        {"id": "oversized-batch-items", "expected_consumer_result": "reject_before_apply", "item_count": 1025, "wire_path": "negative/oversized-items-batch.json", "ack_prefix_effect": "no_advance"},
        {"id": "oversized-batch-bytes", "expected_consumer_result": "reject_before_apply", "encoded_item_bytes_sum": 4_194_305, "construction": "pad one payload string until the sum of compact encoded record and gap items is exactly encoded_item_bytes_sum", "ack_prefix_effect": "no_advance"},
        {"id": "oversized-ack-body", "expected_status": 413, "body_bytes": 131_073, "wire_path": "negative/oversized-ack.json", "ack_prefix_effect": "no_advance"},
        {"id": "lost-ack-response", "request_body_utf8": PREFIXES[2]["request_body_utf8"], "first_response": "connection_lost_after_edge_receipt_commit", "retry_result": "same_ack_result_bytes", "apply_count": 1},
    ],
}


NON_PROJECTION_DISPOSITION = {
    "edge_installation_id": "home-edge",
    "edge_lineage": "spool-2026-09-05",
    "spool_seq": 12,
    "item_id": ALERT_ID,
    "item_kind": "record",
    "category": "durable_non_projection_event",
    "reason": "alert_event_retained",
    "legacy_record_id": ALERT_ALIAS,
    "payload_sha256": "2b8aa76b27b1b29810087e1692c1efcfe3b04e340be83700464570f0924e8fd7",
    "committed_at_ms": 1_800_000_001_000,
}

GAP_DISPOSITION = {
    "edge_installation_id": "home-edge",
    "edge_lineage": "spool-2026-09-05",
    "spool_seq": 11,
    "item_id": NOTICE_ID,
    "item_kind": "gap",
    "category": "durable_gap",
    "reason": "retention_expired",
    "occurred_at_ms": 1_800_000_060_200,
    "evidence_sha256": EVIDENCE_ID,
    "committed_at_ms": 1_800_000_061_000,
}


def encoded(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()


def bundle():
    compact_ack = PREFIXES[2]["request_body_utf8"].encode()
    oversized_ack = compact_ack + b" " * (131_073 - len(compact_ack))
    files = {
        "profile.json": encoded(PROFILE),
        "openapi.json": encoded(OPENAPI),
        "receiver-envelope.schema.json": encoded(ENVELOPE_SCHEMA),
        "delivery.schema.json": encoded(DELIVERY_SCHEMA),
        "consumer-disposition.schema.json": encoded(DISPOSITION_SCHEMA),
        "vectors.json": encoded(VECTORS),
        "examples/projected-envelope.json": encoded(BASE),
        "examples/alert-envelope.json": encoded(ALERT),
        "examples/error-envelope.json": encoded(ERROR),
        "examples/batch.json": encoded(BATCH),
        "examples/ack-prefix.json": encoded(PREFIXES[2]["request"]),
        "examples/ack-result.json": encoded(PREFIXES[2]["response"]),
        "examples/non-projection-disposition.json": encoded(NON_PROJECTION_DISPOSITION),
        "examples/gap-disposition.json": encoded(GAP_DISPOSITION),
        "negative/duplicate-key-ack.json": DUPLICATE_KEY_ACK.encode(),
        "negative/non-contiguous-ack.json": encoded(NON_CONTIGUOUS_ACK),
        "negative/oversized-ack.json": oversized_ack,
        "negative/oversized-items-batch.json": encoded(OVERSIZED_BATCH),
        "negative/stale-ack.json": encoded(STALE_ACK),
    }
    files["SHA256SUMS"] = "".join(
        hashlib.sha256(data).hexdigest() + "  " + name + "\n"
        for name, data in sorted(files.items())
    ).encode()
    return files


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    files = bundle()
    if args.check:
        wrong = [
            name
            for name, data in files.items()
            if not (ROOT / name).is_file() or (ROOT / name).read_bytes() != data
        ]
        if wrong:
            raise SystemExit("stale Edge delivery v2 artifacts: " + ", ".join(wrong))
    else:
        for name, data in files.items():
            (ROOT / name).parent.mkdir(parents=True, exist_ok=True)
            (ROOT / name).write_bytes(data)
    print(
        PROFILE_ID
        + " sha256="
        + hashlib.sha256(files["SHA256SUMS"]).hexdigest()
    )


if __name__ == "__main__":
    main()
