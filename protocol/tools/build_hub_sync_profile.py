#!/usr/bin/env python3
"""Export the source-neutral Hub changes-since contract."""
import argparse
import base64
import copy
import hashlib
import json
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.3.0"
PROFILE_ID = "hub-sync-v1@1.3.0"
BOOTSTRAP_PROFILE_HEADER = "x-teslatlas-sync-profile"
SUPPORTED_SCHEMAS_HEADER = "x-teslatlas-supported-schemas"
SUPPORTED_SCHEMAS_VALUE = "2.1,2.2"
DRAFT = "https://json-schema.org/draft/2020-12/schema"
DIGEST = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
OPAQUE = {"type": "string", "minLength": 1, "maxLength": 4096, "pattern": "^[A-Za-z0-9._~-]+$"}
UUID = {"type": "string", "format": "uuid", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"}
SCHEMA_VERSION = {"type": "string", "enum": ["2.1", "2.2"]}
REQUEST_SCHEMA_VERSION = {"type": "string", "pattern": "^(?:0|[1-9][0-9]{0,2})\\.(?:0|[1-9][0-9]{0,2})$"}
SIGNATURE = {"type": "string", "pattern": "^[A-Za-z0-9+/]{86}==$"}
PUBLIC_KEY = {"type": "string", "pattern": "^[A-Za-z0-9+/]{43}=$"}
MAX_MANIFEST_CHUNKS = 1771
MAX_PREPARED_ROUTES = 497
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
# RFC 8032 test-vector seed. It is a public, deterministic fixture value only.
FIXTURE_SIGNING_SEED = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
FIXTURE_PUBLIC_KEY_BYTES = Ed25519PrivateKey.from_private_bytes(FIXTURE_SIGNING_SEED).public_key().public_bytes(
    serialization.Encoding.Raw, serialization.PublicFormat.Raw
)
FIXTURE_KEY_ID = "ed25519-sha256-" + hashlib.sha256(FIXTURE_PUBLIC_KEY_BYTES).hexdigest()


def strict(properties, required=None):
    return {"type": "object", "additionalProperties": False, "properties": properties,
            "required": list(properties) if required is None else required}


def document(name, body):
    return {"$schema": DRAFT, "$id": f"urn:teslatlas:hub-sync-v1:1.3.0:{name}", **body}


def pack_ref():
    return strict({
        "object_name": {"type": "string", "pattern": "^[a-z0-9][a-z0-9._/-]{0,1023}$"},
        "sha256": DIGEST,
        "compressed_bytes": {"type": "integer", "minimum": 1, "maximum": 16 * 1024 * 1024},
    })


def signing():
    return strict({
        "algorithm": {"const": "ed25519"},
        "key_id": {"type": "string", "minLength": 1, "maxLength": 128, "pattern": "^[A-Za-z0-9._-]+$"},
        "signed_payload_sha256": DIGEST,
        "signature": SIGNATURE,
    })


def canonical_fixture_payload(value):
    payload = dict(value)
    payload.pop("signature", None)
    # Fixture values are ASCII strings, integers, arrays, and objects, so sorted
    # compact UTF-8 JSON is RFC 8785 JCS for this deliberately narrow domain.
    return json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def sign_fixture(value):
    payload = canonical_fixture_payload(value)
    signer = Ed25519PrivateKey.from_private_bytes(FIXTURE_SIGNING_SEED)
    value["signature"] = {
        "algorithm": "ed25519",
        "key_id": FIXTURE_KEY_ID,
        "signed_payload_sha256": hashlib.sha256(payload).hexdigest(),
        "signature": base64.b64encode(signer.sign(payload)).decode(),
    }
    return value


def bundle():
    fixture_public_key = base64.b64encode(FIXTURE_PUBLIC_KEY_BYTES).decode()
    vehicle_id = "11111111-1111-4111-8111-111111111111"
    request = document("changes-since-request", {"$defs": {
        "request": strict({
            "base_receipt_id": OPAQUE,
            "base_manifest_schema": SCHEMA_VERSION,
            "from_sequence": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1},
            "schema_version_range": strict({"minimum": REQUEST_SCHEMA_VERSION, "maximum": REQUEST_SCHEMA_VERSION}),
        })
    }})
    receipt = document("changed-set-receipt", {"$defs": {
        "receipt": strict({
            "receipt_id": OPAQUE,
            "vehicle_id": UUID,
            "base_receipt_id": OPAQUE,
            "base_manifest_schema": SCHEMA_VERSION,
            "from_sequence": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1},
            "to_sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "manifest_schema": SCHEMA_VERSION,
            "changed_set_sha256": DIGEST,
            "pack": pack_ref(),
            "signature": signing(),
        }),
    }})
    rebase = document("rebase-hint", {"$defs": {
        "hint": strict({
            "kind": {"const": "rebase_required"},
            "vehicle_id": UUID,
            "requested_base_receipt_id": OPAQUE,
            "requested_base_manifest_schema": SCHEMA_VERSION,
            "requested_from_sequence": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1},
            "reason": {"const": "compacted"},
            "replacement": {"oneOf": [{"$ref": "#/$defs/replacement_2_1"}, {"$ref": "#/$defs/replacement_2_2"}]},
            "retry_request": strict({
                "base_receipt_id": OPAQUE,
                "base_manifest_schema": SCHEMA_VERSION,
                "from_sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
                "schema_version_range": strict({"minimum": REQUEST_SCHEMA_VERSION, "maximum": REQUEST_SCHEMA_VERSION}),
            }),
            "signature": signing(),
        }),
        "replacement_2_1": strict({
            "manifest_id": OPAQUE,
            "receipt_id": OPAQUE,
            "sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "manifest_schema": {"const": "2.1"},
            "pack": pack_ref(),
        }),
        "replacement_2_2": strict({
            "manifest_id": OPAQUE,
            "receipt_id": OPAQUE,
            "sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "manifest_schema": {"const": "2.2"},
            "chunks": {"type": "array", "minItems": 1, "maxItems": MAX_MANIFEST_CHUNKS, "items": {"$ref": "#/$defs/chunk"}},
        }),
        "chunk": strict({
            "chunk_index": {"type": "integer", "minimum": 0, "maximum": MAX_MANIFEST_CHUNKS - 1},
            "pack": pack_ref(),
        }),
    }})
    chunk = strict({
        "chunk_index": {"type": "integer", "minimum": 0, "maximum": MAX_MANIFEST_CHUNKS - 1},
        "pack": pack_ref(),
    })
    manifest = document("sync-manifest", {"$defs": {
        "schema_2_1": strict({
            "manifest_id": OPAQUE, "receipt_id": OPAQUE, "vehicle_id": UUID, "kind": {"const": "snapshot"},
            "schema_version": {"const": "2.1"}, "sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "pack": pack_ref(), "signature": signing(),
        }),
        "schema_2_2": strict({
            "manifest_id": OPAQUE, "receipt_id": OPAQUE, "vehicle_id": UUID, "kind": {"const": "snapshot"},
            "schema_version": {"const": "2.2"}, "sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "chunks": {"type": "array", "minItems": 1, "maxItems": MAX_MANIFEST_CHUNKS, "items": chunk}, "signature": signing(),
        }),
        "manifest": {"oneOf": [{"$ref": "#/$defs/schema_2_1"}, {"$ref": "#/$defs/schema_2_2"}]},
    }})
    noop = document("noop", {"$defs": {
        "receipt": strict({
            "kind": {"const": "no_op"}, "vehicle_id": UUID, "base_receipt_id": OPAQUE,
            "base_manifest_schema": SCHEMA_VERSION,
            "sequence": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1},
            "manifest_schema": SCHEMA_VERSION, "signature": signing(),
        }),
    }})
    prepared = document("prepared-artefact", {"$defs": {
        "map_month": strict({
            "month": {"type": "string", "pattern": "^[0-9]{4}-(0[1-9]|1[0-2])$"},
            "from_ms": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1},
            "to_ms": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "reason": {"const": "changed"},
        }),
        "route": strict({
            "route_id": OPAQUE,
            "from_ms": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1},
            "to_ms": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "reason": {"const": "changed"},
        }),
        "receipt": strict({
            "artifact_id": OPAQUE,
            "artifact_type": {"const": "map_months_and_routes"},
            "vehicle_id": UUID,
            "source": strict({"kind": {"const": "hub_compute"}, "input_manifest_id": OPAQUE, "input_sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1}}),
            "window": strict({"from_ms": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1}, "to_ms": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1}}),
            "generation": strict({"generation_id": OPAQUE, "generated_at_ms": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1}}),
            "units": strict({"distance": {"const": "km"}, "time": {"const": "ms"}, "coordinates": {"const": "wgs84_degrees"}}),
            "algorithm_version": {"type": "string", "pattern": "^[0-9]+\\.[0-9]+\\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$", "maxLength": 128},
            "dirty_spans": strict({"map_months": {"type": "array", "maxItems": 120, "items": {"$ref": "#/$defs/map_month"}}, "routes": {"type": "array", "maxItems": MAX_PREPARED_ROUTES, "items": {"$ref": "#/$defs/route"}}}),
            "pack": pack_ref(),
            "signature": signing(),
        }),
    }})
    prepared["$defs"]["receipt"]["properties"]["dirty_spans"]["anyOf"] = [
        {"properties": {"map_months": {"minItems": 1}}},
        {"properties": {"routes": {"minItems": 1}}},
    ]
    sync_error = document("sync-error", {"$defs": {
        "error": strict({
            "code": {"enum": ["invalid_json", "invalid_request", "invalid_schema_range", "unknown_base_receipt", "vehicle_not_found", "schema_range_unsupported", "request_too_large"]},
            "message": {"type": "string", "minLength": 1, "maxLength": 256},
        }),
    }})
    status_tables_schema = document("status-tables", {"$defs": {
        "response": strict({
            "status": {"type": "integer", "minimum": 100, "maximum": 599},
            "body": {"type": "string", "minLength": 1, "maxLength": 256},
            "headers": {"type": "object", "additionalProperties": False, "properties": {"cache_control": {"type": "string"}, "etag": {"type": "string"}, "content_range": {"type": "string"}}},
            "signature": {"enum": ["present", "absent"]},
        }, ["status", "body"]),
        "table": strict({"route": {"type": "string", "minLength": 1, "maxLength": 512}, "responses": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"$ref": "#/$defs/response"}}}),
        "document": strict({"profile_id": {"const": PROFILE_ID}, "tables": {"type": "array", "minItems": 6, "maxItems": 16, "items": {"$ref": "#/$defs/table"}}}),
    }})
    status_tables = {
        "profile_id": PROFILE_ID,
        "tables": [
            {"route": "POST /v1/vehicles/{vehicle_id}/sync/changes-since", "responses": [
                {"status": 200, "body": "signed changed-set or no-op receipt"},
                {"status": 400, "body": "invalid_json error", "signature": "absent"},
                {"status": 401, "body": "empty", "signature": "absent"},
                {"status": 404, "body": "vehicle_not_found error", "signature": "absent"},
                {"status": 406, "body": "schema_range_unsupported error", "headers": {"cache_control": "no-store"}, "signature": "absent"},
                {"status": 409, "body": "signed rebase hint"},
                {"status": 413, "body": "request_too_large error", "signature": "absent"},
                {"status": 422, "body": "invalid_request, invalid_schema_range, or unknown_base_receipt error", "signature": "absent"}]},
            {"route": "GET /v1/vehicles/{vehicle_id}/sync/manifest", "responses": [
                {"status": 200, "body": "one signed schema 2.1 or 2.2 manifest"},
                {"status": 401, "body": "empty", "signature": "absent"},
                {"status": 406, "body": "empty invalid or unavailable explicit bootstrap negotiation", "headers": {"cache_control": "no-store"}, "signature": "absent"}]},
            {"route": "GET /v1/vehicles/{vehicle_id}/sync/noop", "responses": [
                {"status": 200, "body": "signed no-op receipt"},
                {"status": 401, "body": "empty", "signature": "absent"},
                {"status": 406, "body": "empty", "headers": {"cache_control": "no-store"}, "signature": "absent"}]},
            {"route": "GET /v1/packs/sha256/{object_name}", "responses": [
                {"status": 200, "body": "pack bytes", "headers": {"etag": "strong"}},
                {"status": 206, "body": "single contiguous byte range", "headers": {"content_range": "required", "etag": "strong"}},
                {"status": 401, "body": "empty", "signature": "absent"},
                {"status": 404, "body": "empty", "signature": "absent"}]},
            {"route": "GET /v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}", "responses": [
                {"status": 200, "body": "signed prepared-artefact receipt"},
                {"status": 401, "body": "empty", "signature": "absent"},
                {"status": 404, "body": "empty", "signature": "absent"}]},
            {"route": "GET /v1/vehicles/{vehicle_id}/sync/signing-keys", "responses": [
                {"status": 200, "body": "vehicle-bound signing key set", "headers": {"cache_control": "no-store"}, "signature": "absent"},
                {"status": 401, "body": "empty", "signature": "absent"},
                {"status": 404, "body": "empty", "signature": "absent"}]},
        ],
    }
    request_example = {
        "fixture_id": "changes-since-request-v1",
        "vehicle_id": vehicle_id,
        "request": {"base_receipt_id": "receipt_demo_000042", "base_manifest_schema": "2.1", "from_sequence": 42,
                    "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}},
    }
    receipt_example = {
        "fixture_id": "changes-since-changed-set-v1",
        "receipt": {"receipt_id": "receipt_demo_000045", "vehicle_id": vehicle_id,
                    "base_receipt_id": "receipt_demo_000042", "base_manifest_schema": "2.1", "from_sequence": 42, "to_sequence": 45,
                    "manifest_schema": "2.1", "changed_set_sha256": "2" * 64,
                    "pack": {"object_name": "packs/demo-changed-set-000045.sqlite.zst", "sha256": "3" * 64,
                             "compressed_bytes": 4096}},
    }
    rebase_example = {
        "fixture_id": "changes-since-rebase-after-compaction-v1",
        "response": {"kind": "rebase_required", "vehicle_id": vehicle_id,
                     "requested_base_receipt_id": "receipt_demo_000042", "requested_base_manifest_schema": "2.1", "requested_from_sequence": 42,
                     "reason": "compacted", "replacement": {"manifest_id": "manifest_demo_000900", "receipt_id": "receipt_retained_000900", "sequence": 900,
                     "manifest_schema": "2.2", "chunks": [
                         {"chunk_index": 0, "pack": {"object_name": "packs/demo-rebase-000900-0000.sqlite.zst", "sha256": "4" * 64, "compressed_bytes": 8 * 1024 * 1024}},
                         {"chunk_index": 1, "pack": {"object_name": "packs/demo-rebase-000900-0001.sqlite.zst", "sha256": "5" * 64, "compressed_bytes": 4096}}]},
                     "retry_request": {"base_receipt_id": "receipt_retained_000900", "base_manifest_schema": "2.2", "from_sequence": 900,
                     "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}},
                     },
    }
    manifest_2_1_example = {
        "fixture_id": "schema-2-1-single-pack-manifest-v1",
        "manifest": {"manifest_id": "manifest_demo_000899", "receipt_id": "receipt_demo_000042", "vehicle_id": vehicle_id,
                     "kind": "snapshot", "schema_version": "2.1", "sequence": 899,
                     "pack": {"object_name": "packs/demo-000899.sqlite.zst", "sha256": "a" * 64, "compressed_bytes": 348160},
                     },
    }
    manifest_2_2_example = {
        "fixture_id": "schema-2-2-multi-chunk-manifest-v1",
        "manifest": {"manifest_id": "manifest_demo_000900", "receipt_id": "receipt_demo_000900", "vehicle_id": vehicle_id,
                     "kind": "snapshot", "schema_version": "2.2", "sequence": 900,
                     "chunks": [
                         {"chunk_index": 0, "pack": {"object_name": "packs/demo-000900-0000.sqlite.zst", "sha256": "6" * 64, "compressed_bytes": 8 * 1024 * 1024}},
                         {"chunk_index": 1, "pack": {"object_name": "packs/demo-000900-0001.sqlite.zst", "sha256": "7" * 64, "compressed_bytes": 4096}},
                     ]},
    }
    manifest_2_2_small_example = {
        "fixture_id": "schema-2-2-single-small-chunk-manifest-v1",
        "manifest": {"manifest_id": "manifest_demo_000001", "receipt_id": "receipt_demo_000001", "vehicle_id": vehicle_id,
                     "kind": "snapshot", "schema_version": "2.2", "sequence": 1,
                     "chunks": [{"chunk_index": 0, "pack": {"object_name": "packs/demo-000001-0000.sqlite.zst", "sha256": "8" * 64, "compressed_bytes": 4096}}]},
    }
    bootstrap_request = {
        "fixture_id": "bootstrap-hub-sync-v1-1-3-selected-v1",
        "vehicle_id": vehicle_id,
        "request": {
            "method": "GET",
            "path": f"/v1/vehicles/{vehicle_id}/sync/manifest",
            "headers": [
                {"name": BOOTSTRAP_PROFILE_HEADER, "value": PROFILE_ID},
                {"name": SUPPORTED_SCHEMAS_HEADER, "value": SUPPORTED_SCHEMAS_VALUE},
            ],
        },
        "expected_response": {"status": 200, "representation": PROFILE_ID},
    }
    legacy_bootstrap_request = copy.deepcopy(bootstrap_request)
    legacy_bootstrap_request["fixture_id"] = "bootstrap-schema-list-only-legacy-v1"
    legacy_bootstrap_request["request"]["headers"] = [
        {"name": SUPPORTED_SCHEMAS_HEADER, "value": SUPPORTED_SCHEMAS_VALUE}
    ]
    legacy_bootstrap_request["expected_response"] = {"status": 200, "representation": "legacy-sync-manifest"}

    def rejected_bootstrap_fixture(fixture_id, headers):
        return {
            "fixture_id": fixture_id,
            "vehicle_id": vehicle_id,
            "request": {
                "method": "GET",
                "path": f"/v1/vehicles/{vehicle_id}/sync/manifest",
                "headers": headers,
            },
            "expected_response": {
                "status": 406,
                "representation": "empty",
                "headers": {"cache_control": "no-store"},
                "body_bytes": 0,
                "manifest_signature": "absent",
            },
        }

    rejected_bootstrap_examples = {
        "bootstrap-profile-selector-invalid": rejected_bootstrap_fixture(
            "bootstrap-profile-selector-invalid-v1",
            [
                {"name": BOOTSTRAP_PROFILE_HEADER, "value": "hub-sync-v1@1.3"},
                {"name": SUPPORTED_SCHEMAS_HEADER, "value": SUPPORTED_SCHEMAS_VALUE},
            ],
        ),
        "bootstrap-profile-selector-missing-schema": rejected_bootstrap_fixture(
            "bootstrap-profile-selector-missing-schema-v1",
            [{"name": BOOTSTRAP_PROFILE_HEADER, "value": PROFILE_ID}],
        ),
        "bootstrap-profile-selector-invalid-schema": rejected_bootstrap_fixture(
            "bootstrap-profile-selector-invalid-schema-v1",
            [
                {"name": BOOTSTRAP_PROFILE_HEADER, "value": PROFILE_ID},
                {"name": SUPPORTED_SCHEMAS_HEADER, "value": "2.1, 2.2"},
            ],
        ),
        "bootstrap-profile-selector-duplicate": rejected_bootstrap_fixture(
            "bootstrap-profile-selector-duplicate-v1",
            [
                {"name": BOOTSTRAP_PROFILE_HEADER, "value": PROFILE_ID},
                {"name": BOOTSTRAP_PROFILE_HEADER, "value": PROFILE_ID},
                {"name": SUPPORTED_SCHEMAS_HEADER, "value": SUPPORTED_SCHEMAS_VALUE},
            ],
        ),
        "bootstrap-profile-selector-duplicate-schema": rejected_bootstrap_fixture(
            "bootstrap-profile-selector-duplicate-schema-v1",
            [
                {"name": BOOTSTRAP_PROFILE_HEADER, "value": PROFILE_ID},
                {"name": SUPPORTED_SCHEMAS_HEADER, "value": SUPPORTED_SCHEMAS_VALUE},
                {"name": SUPPORTED_SCHEMAS_HEADER, "value": SUPPORTED_SCHEMAS_VALUE},
            ],
        ),
    }
    noop_example = {
        "fixture_id": "sync-noop-signed-v1",
        "receipt": {"kind": "no_op", "vehicle_id": vehicle_id,
                    "base_receipt_id": "receipt_demo_000045", "base_manifest_schema": "2.2", "sequence": 45, "manifest_schema": "2.2",
                    },
    }
    changes_noop_example = copy.deepcopy(noop_example)
    changes_noop_example["fixture_id"] = "changes-since-no-change-v1"
    changes_noop_example["receipt"].update({
        "base_receipt_id": "receipt_demo_000042",
        "base_manifest_schema": "2.1",
        "sequence": 42,
        "manifest_schema": "2.1",
    })
    noop_unavailable = {
        "fixture_id": "sync-noop-unavailable-v1",
        "response": {"status": 406, "headers": {"cache_control": "no-store"}, "body_bytes": 0, "manifest_signature": "absent"},
    }
    prepared_example = {
        "fixture_id": "prepared-artefact-map-months-routes-v1",
        "receipt": {"artifact_id": "prepared_demo_000900", "artifact_type": "map_months_and_routes", "vehicle_id": vehicle_id,
                    "source": {"kind": "hub_compute", "input_manifest_id": "manifest_demo_000900", "input_sequence": 900},
                    "window": {"from_ms": 1735689600000, "to_ms": 1740787200000},
                    "generation": {"generation_id": "generation_demo_000001", "generated_at_ms": 1740790800000},
                    "units": {"distance": "km", "time": "ms", "coordinates": "wgs84_degrees"}, "algorithm_version": "1.0.0",
                    "dirty_spans": {"map_months": [
                        {"month": "2025-01", "from_ms": 1735689600000, "to_ms": 1738368000000, "reason": "changed"},
                        {"month": "2025-02", "from_ms": 1738368000000, "to_ms": 1740787200000, "reason": "changed"}],
                        "routes": [{"route_id": "route_demo_000045", "from_ms": 1738454400000, "to_ms": 1738458000000, "reason": "changed"}]},
                    "pack": {"object_name": "packs/prepared-demo-000900.sqlite.zst", "sha256": "c" * 64, "compressed_bytes": 4096},
                    },
    }
    for fixture in (receipt_example["receipt"], rebase_example["response"],
                    manifest_2_1_example["manifest"], manifest_2_2_example["manifest"],
                    manifest_2_2_small_example["manifest"], noop_example["receipt"],
                    changes_noop_example["receipt"], prepared_example["receipt"]):
        sign_fixture(fixture)
    tampered_changed = copy.deepcopy(receipt_example)
    tampered_changed["fixture_id"] = "changes-since-changed-set-tampered-v1"
    tampered_changed["receipt"]["to_sequence"] += 1
    unknown_key_changed = copy.deepcopy(receipt_example)
    unknown_key_changed["fixture_id"] = "changes-since-changed-set-unknown-key-v1"
    unknown_key_changed["receipt"]["signature"]["key_id"] = "ed25519-sha256-" + "f" * 64
    invalid_signature_changed = copy.deepcopy(receipt_example)
    invalid_signature_changed["fixture_id"] = "changes-since-changed-set-invalid-signature-v1"
    invalid_signature_changed["receipt"]["signature"]["signature"] = "A" * 86 + "=="
    cross_schema_changed = copy.deepcopy(receipt_example)
    cross_schema_changed["fixture_id"] = "changes-since-changed-set-cross-schema-v1"
    cross_schema_changed["receipt"]["manifest_schema"] = "2.2"
    sign_fixture(cross_schema_changed["receipt"])
    cross_vehicle_changed = copy.deepcopy(receipt_example)
    cross_vehicle_changed["fixture_id"] = "changes-since-changed-set-cross-vehicle-v1"
    cross_vehicle_changed["receipt"]["vehicle_id"] = "22222222-2222-4222-8222-222222222222"
    sign_fixture(cross_vehicle_changed["receipt"])
    noncontiguous_manifest = copy.deepcopy(manifest_2_2_example)
    noncontiguous_manifest["fixture_id"] = "schema-2-2-multi-chunk-manifest-noncontiguous-v1"
    noncontiguous_manifest["manifest"]["chunks"][1]["chunk_index"] = 2
    cacheable_noop = copy.deepcopy(noop_unavailable)
    cacheable_noop["fixture_id"] = "sync-noop-unavailable-cacheable-v1"
    cacheable_noop["response"]["headers"]["cache_control"] = "private"
    error_specs = (
        (400, "invalid_json", "Request body is not valid JSON."),
        (404, "vehicle_not_found", "Vehicle is not available to this pairing."),
        (406, "schema_range_unsupported", "Requested schema range has no supported version."),
        (413, "request_too_large", "Request body exceeds 8192 bytes."),
        (422, "invalid_request", "Request body does not match the changes-since schema."),
        (422, "invalid_schema_range", "Schema range is reversed or excludes the base schema."),
        (422, "unknown_base_receipt", "Base receipt is not known for this vehicle."),
    )
    error_examples = {}
    for status, code, message in error_specs:
        name = "changes-since-error-" + code.replace("_", "-")
        response = {"status": status, "body": {"code": code, "message": message}}
        if status == 406:
            response["headers"] = {"cache_control": "no-store"}
        error_examples[name] = {"fixture_id": name + "-v1", "response": response}

    invalid_request_examples = {}
    for suffix, mutate in (
        ("missing-field", lambda value: value.pop("base_receipt_id")),
        ("extra-field", lambda value: value.__setitem__("unexpected", True)),
        ("wrong-type", lambda value: value.__setitem__("from_sequence", "42")),
    ):
        value = copy.deepcopy(request_example)
        value["fixture_id"] = "changes-since-request-invalid-" + suffix + "-v1"
        mutate(value["request"])
        invalid_request_examples[suffix] = value
    unsupported_request = copy.deepcopy(request_example)
    unsupported_request["fixture_id"] = "changes-since-request-unsupported-range-v1"
    unsupported_request["request"]["schema_version_range"] = {"minimum": "2.3", "maximum": "2.3"}
    reversed_range_request = copy.deepcopy(request_example)
    reversed_range_request["fixture_id"] = "changes-since-request-reversed-range-v1"
    reversed_range_request["request"]["schema_version_range"] = {"minimum": "2.2", "maximum": "2.1"}
    base_excluded_request = copy.deepcopy(request_example)
    base_excluded_request["fixture_id"] = "changes-since-request-base-excluded-v1"
    base_excluded_request["request"]["schema_version_range"] = {"minimum": "2.2", "maximum": "2.2"}
    unknown_base_receipt_request = copy.deepcopy(request_example)
    unknown_base_receipt_request["fixture_id"] = "changes-since-request-unknown-base-receipt-v1"
    unknown_base_receipt_request["request"]["base_receipt_id"] = "receipt_unknown_999999"

    compact_request = json.dumps(request_example["request"], sort_keys=True, separators=(",", ":"))
    request_at_limit = {"fixture_id": "changes-since-request-8192-bytes-v1", "body": compact_request + " " * (8192 - len(compact_request.encode()))}
    request_over_limit = {"fixture_id": "changes-since-request-8193-bytes-v1", "body": compact_request + " " * (8193 - len(compact_request.encode()))}
    signing_keys_schema = document("signing-keys", {"$defs": {
        "key": strict({"algorithm": {"const": "ed25519"}, "key_id": {"type": "string", "pattern": "^ed25519-sha256-[0-9a-f]{64}$"}, "public_key": PUBLIC_KEY}),
        "document": strict({"profile_id": {"const": PROFILE_ID}, "vehicle_id": UUID, "key_set_id": OPAQUE,
            "keys": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"$ref": "#/$defs/key"}},
            "rotation": strict({"key_selection": {"const": "key_id"}, "key_id_derivation": {"const": "ed25519-sha256-hex-public-key"}, "publish_before_use": {"const": True}, "retain_retired_keys": {"const": True}})}),
    }})
    signing_keys = {"profile_id": PROFILE_ID, "vehicle_id": vehicle_id, "key_set_id": "fixture-ed25519-rfc8032", "keys": [
        {"algorithm": "ed25519", "key_id": FIXTURE_KEY_ID, "public_key": fixture_public_key}],
        "rotation": {"key_selection": "key_id", "key_id_derivation": "ed25519-sha256-hex-public-key", "publish_before_use": True, "retain_retired_keys": True}}
    signing_keys_example = {"fixture_id": "signing-keys-vehicle-bound-v1", "response": {
        "status": 200, "headers": {"cache_control": "no-store"}, "body": signing_keys}}

    def padded_body_fixture(fixture_id, response_type, status, document_value, body_bytes, headers=None):
        compact = json.dumps(document_value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
        padding_bytes = body_bytes - len(compact)
        if padding_bytes < 0:
            raise ValueError("fixture document exceeds response boundary")
        raw = compact + b" " * padding_bytes
        value = {"fixture_id": fixture_id, "response_type": response_type, "status": status,
                 "vehicle_id": vehicle_id, "body_bytes": body_bytes,
                 "padding_bytes": padding_bytes, "body_sha256": hashlib.sha256(raw).hexdigest(),
                 "document": document_value}
        if headers:
            value["headers"] = headers
        return value

    response_boundary_specs = (
        ("sync-manifest", "manifest", 200, manifest_2_2_small_example["manifest"], None),
        ("changes-since-changed-set", "changed_set", 200, receipt_example["receipt"], None),
        ("sync-noop", "noop", 200, noop_example["receipt"], None),
        ("changes-since-rebase", "rebase", 409, rebase_example["response"], None),
        ("changes-since-error", "error", 422, error_examples["changes-since-error-invalid-request"]["response"]["body"], None),
        ("signing-keys", "signing_keys", 200, signing_keys, {"cache_control": "no-store"}),
        ("prepared-artefact", "prepared_artefact", 200, prepared_example["receipt"], None),
    )
    response_boundary_examples = {}
    for prefix, response_type, status, document_value, headers in response_boundary_specs:
        for body_bytes in (MAX_RESPONSE_BYTES, MAX_RESPONSE_BYTES + 1):
            name = f"{prefix}-response-{body_bytes}-bytes"
            response_boundary_examples[name] = padded_body_fixture(
                name + "-v1", response_type, status, document_value, body_bytes, headers
            )
    profile = {
        "profile_id": PROFILE_ID, "status": "candidate", "contract_version": "1.3.0",
        "authentication": "paired bearer",
        "product_version_binding": "none; this protocol profile has no exact Hub product-version pin",
        "schema_version_range": {"minimum": "2.1", "maximum": "2.2"},
        "previous_profile": "hub-sync-v1@1.2.0",
        "limits": {"max_changed_set_packs": 1, "min_pack_compressed_bytes": 1,
                   "target_pack_compressed_bytes": 8 * 1024 * 1024,
                   "max_pack_compressed_bytes": 16 * 1024 * 1024, "max_manifest_chunks": MAX_MANIFEST_CHUNKS,
                   "max_prepared_routes": MAX_PREPARED_ROUTES,
                   "max_request_bytes": 8192, "max_response_bytes": MAX_RESPONSE_BYTES},
        "status_tables": "status-tables.json",
        "prepared_artefact": "prepared-artefact.schema.json",
        "fixture_signing_keys": "fixture-signing-keys.json",
        "bootstrap_selector": {
            "header": BOOTSTRAP_PROFILE_HEADER,
            "value": PROFILE_ID,
            "required_schema_header": SUPPORTED_SCHEMAS_HEADER,
            "required_schema_value": SUPPORTED_SCHEMAS_VALUE,
            "without_selector": "legacy-sync-manifest",
            "invalid_explicit_selector": {
                "status": 406,
                "body_bytes": 0,
                "cache_control": "no-store",
                "manifest_signature": "absent",
            },
        },
        "route": {"method": "POST", "path": "/v1/vehicles/{vehicle_id}/sync/changes-since",
                  "success_status": 200, "compacted_status": 409},
        "signature_rule": "Each receipt or rebase hint MUST carry an Ed25519 detached signature over RFC 8785 canonical JSON of the object with its signature member omitted. signed_payload_sha256 is the SHA-256 of those canonical bytes. The signed object's vehicle_id and the selected signing key set's vehicle_id MUST both equal the route vehicle_id. Production clients obtain vehicle-bound keys from the authenticated no-store signing-keys route. key_id MUST equal ed25519-sha256- plus lowercase SHA-256 hex of the raw 32-byte public key. Fixtures carry deterministic Ed25519 signatures using only the published fixture public key; they are test vectors, not production trust anchors.",
        "bootstrap_rule": "A client selects the hub-sync-v1@1.3.0 bootstrap representation only by sending exactly one x-teslatlas-sync-profile: hub-sync-v1@1.3.0 header and exactly one x-teslatlas-supported-schemas: 2.1,2.2 header. If an explicit profile selector is present but either header is missing, duplicated, or has any other value, the Hub MUST return an empty 406 with Cache-Control: no-store and MUST NOT fall back to legacy routing. The schema header without a profile selector remains legacy SyncManifest negotiation and MUST NOT select this representation. After applying a signed 1.3 manifest, the client MUST persist manifest.receipt_id, sequence, and schema_version. It sends those exact values as base_receipt_id, from_sequence, and base_manifest_schema on changes-since.",
        "schema_continuity_rule": "A changed-set or no-op receipt MUST use the base manifest schema. A schema 2.2 base cannot receive a schema 2.1 delta. Any schema transition requires a signed rebase whose replacement schema is accepted by the request range.",
        "pack_sizing_rule": "Every compressed pack is 1 to 16 MiB inclusive. Writers SHOULD target at least 8 MiB for each non-final snapshot chunk. A complete small history, the final chunk, a changed set, or a prepared artefact MAY be smaller than 8 MiB.",
        "response_size_rule": "Every JSON control response MUST be at most 2097152 encoded bytes before parsing. Pack byte responses use max_pack_compressed_bytes instead. The schema maxima also keep a compact encoding of every admitted control response within this bound.",
        "receipt_resolution_rule": "Receipt resolution is scoped to the route vehicle. A checkpoint in the current lineage is current. Any base or delta checkpoint found in an unexpired retained prior lineage was compacted and MUST return the signed 409 rebase hint. A receipt absent from both the current lineage and every unexpired retained prior lineage MUST return unsigned 422 unknown_base_receipt.",
        "compaction_rule": "A compacted base MUST return 409 and the signed rebase hint. The replacement MUST be a complete admitted snapshot for its declared schema and limits, never a delta, compacted delta, or partial lineage. The client applies the replacement pack for schema 2.1 or all contiguous replacement chunks for schema 2.2, persists replacement receipt_id, sequence, and manifest_schema, then sends retry_request. It MUST NOT infer a rebase target or substitute a full-history request. A Hub MUST NOT admit a checkpoint that it cannot replace with such a complete snapshot while the checkpoint can remain valid.",
        "scope": "One changed pack per changed-set receipt; schema 2.2 snapshot and rebase manifests may contain one or more chunks under one signature; prepared artefacts contain only changed map-month and route spans. No other prepared-compute artefact type is defined.",
    }
    def error_response(description):
        return {"description": description, "x-max-body-bytes": MAX_RESPONSE_BYTES,
                "content": {"application/json": {"schema": {"$ref": "sync-error.schema.json#/$defs/error"}}}}

    no_store_header = {"Cache-Control": {"description": "Required cache prohibition.", "schema": {"const": "no-store"}}}

    openapi = {"openapi": "3.1.0", "info": {"title": "Teslatlas changes-since", "version": "1.3.0"},
      "paths": {"/v1/vehicles/{vehicle_id}/sync/changes-since": {"post": {"operationId": "changesSince",
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}],
        "requestBody": {"required": True, "x-max-body-bytes": 8192, "content": {"application/json": {"schema": {"$ref": "changes-since-request.schema.json#/$defs/request"}}}},
        "security": [{"pairedBearer": []}],
        "responses": {"200": {"description": "signed changed-set or no-op receipt", "x-max-body-bytes": MAX_RESPONSE_BYTES, "content": {"application/json": {"schema": {"oneOf": [{"$ref": "changed-set-receipt.schema.json#/$defs/receipt"}, {"$ref": "noop.schema.json#/$defs/receipt"}]}}}},
                      "400": error_response("invalid JSON request body"),
                      "401": {"description": "missing, invalid, expired, or revoked paired bearer", "content": {}},
                      "404": error_response("vehicle is not available to this pairing"),
                      "406": {**error_response("schema range has no supported version; Cache-Control: no-store"), "headers": no_store_header},
                      "413": error_response("request body exceeds 8192 bytes"),
                      "422": error_response("request violates the schema, reverses the range, excludes the base schema, or names a base receipt unknown to this vehicle"),
                      "409": {"description": "signed rebase hint after compaction", "x-max-body-bytes": MAX_RESPONSE_BYTES, "content": {"application/json": {"schema": {"$ref": "rebase-hint.schema.json#/$defs/hint"}}}}}}}},
      "components": {"securitySchemes": {"pairedBearer": {"type": "http", "scheme": "bearer", "bearerFormat": "opaque paired bearer"}}}}
    openapi["paths"]["/v1/vehicles/{vehicle_id}/sync/manifest"] = {"get": {"operationId": "syncManifest", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID},
                       {"name": BOOTSTRAP_PROFILE_HEADER, "in": "header", "required": True, "description": "Exact, single profile selector for the 1.3 bootstrap representation. A missing, duplicated, or different explicit selector/companion returns empty no-store 406. Without any profile selector, the request remains legacy SyncManifest negotiation.", "schema": {"const": PROFILE_ID}},
                       {"name": SUPPORTED_SCHEMAS_HEADER, "in": "header", "required": True, "description": "Exact, single schema companion for the explicit 1.3 selector.", "schema": {"const": SUPPORTED_SCHEMAS_VALUE}}],
        "responses": {"200": {"description": "signed manifest", "x-max-body-bytes": MAX_RESPONSE_BYTES, "content": {"application/json": {"schema": {"$ref": "sync-manifest.schema.json#/$defs/manifest"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "406": {"description": "empty unavailable or malformed explicit bootstrap negotiation response with Cache-Control: no-store", "headers": no_store_header, "content": {}}}}}
    openapi["paths"]["/v1/vehicles/{vehicle_id}/sync/noop"] = {"get": {"operationId": "syncNoop", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}],
        "responses": {"200": {"description": "signed no-op receipt", "x-max-body-bytes": MAX_RESPONSE_BYTES, "content": {"application/json": {"schema": {"$ref": "noop.schema.json#/$defs/receipt"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "406": {"description": "empty unavailable response with Cache-Control: no-store", "headers": no_store_header, "content": {}}}}}
    openapi["paths"]["/v1/packs/sha256/{object_name}"] = {"get": {"operationId": "syncPack", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "object_name", "in": "path", "required": True, "schema": {"type": "string", "minLength": 1, "maxLength": 1024}}],
        "responses": {"200": {"description": "pack bytes", "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}}, "206": {"description": "single byte range", "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "404": {"description": "empty missing pack", "content": {}}}}}
    openapi["paths"]["/v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}"] = {"get": {"operationId": "preparedArtefact", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}, {"name": "artifact_id", "in": "path", "required": True, "schema": OPAQUE}],
        "responses": {"200": {"description": "signed prepared-artefact receipt", "x-max-body-bytes": MAX_RESPONSE_BYTES, "content": {"application/json": {"schema": {"$ref": "prepared-artefact.schema.json#/$defs/receipt"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "404": {"description": "empty missing artefact", "content": {}}}}}
    openapi["paths"]["/v1/vehicles/{vehicle_id}/sync/signing-keys"] = {"get": {"operationId": "syncSigningKeys", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}],
        "responses": {"200": {"description": "vehicle-bound signing keys over the authenticated paired channel; Cache-Control: no-store", "x-max-body-bytes": MAX_RESPONSE_BYTES, "headers": no_store_header, "content": {"application/json": {"schema": {"$ref": "signing-keys.schema.json#/$defs/document"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "404": {"description": "empty unknown vehicle", "content": {}}}}}
    cases = {"profile_id": PROFILE_ID, "cases": [
      {"id": "bootstrap-hub-sync-v1-1-3-selected", "validator": "bootstrap_selection", "request_fixture": bootstrap_request["fixture_id"], "status": 200, "expected_errors": []},
      {"id": "bootstrap-schema-list-only-keeps-legacy-shape", "validator": "bootstrap_selection", "request_fixture": legacy_bootstrap_request["fixture_id"], "status": 200, "expected_errors": []},
      *[{"id": name, "validator": "bootstrap_selection", "request_fixture": value["fixture_id"], "status": 406, "expected_errors": []} for name, value in rejected_bootstrap_examples.items()],
      {"id": "changes-since-changed-set", "validator": "changes_since", "request_fixture": request_example["fixture_id"], "status": 200, "response_fixture": receipt_example["fixture_id"], "expected_errors": []},
      {"id": "changes-since-rebase-after-compaction", "validator": "changes_since", "request_fixture": request_example["fixture_id"], "status": 409, "response_fixture": rebase_example["fixture_id"], "expected_errors": []},
      {"id": "schema-2-1-single-pack-manifest", "validator": "manifest", "vehicle_id": vehicle_id, "status": 200, "response_fixture": manifest_2_1_example["fixture_id"], "expected_errors": []},
      {"id": "schema-2-2-multi-chunk-manifest", "validator": "manifest", "vehicle_id": vehicle_id, "status": 200, "response_fixture": manifest_2_2_example["fixture_id"], "expected_errors": []},
      {"id": "schema-2-2-single-small-chunk-manifest", "validator": "manifest", "vehicle_id": vehicle_id, "status": 200, "response_fixture": manifest_2_2_small_example["fixture_id"], "expected_errors": []},
      {"id": "changes-since-no-change", "validator": "changes_since", "request_fixture": request_example["fixture_id"], "status": 200, "response_fixture": changes_noop_example["fixture_id"], "expected_errors": []},
      {"id": "sync-noop-signed", "validator": "noop", "vehicle_id": vehicle_id, "status": 200, "response_fixture": noop_example["fixture_id"], "expected_errors": []},
      {"id": "sync-noop-unavailable", "validator": "noop", "vehicle_id": vehicle_id, "status": 406, "response_fixture": noop_unavailable["fixture_id"], "expected_errors": []},
      {"id": "prepared-artefact-map-months-routes", "validator": "prepared_artefact", "vehicle_id": vehicle_id, "status": 200, "response_fixture": prepared_example["fixture_id"], "expected_errors": []},
      {"id": "changes-since-changed-set-tampered", "validator": "changes_since", "vehicle_id": vehicle_id, "status": 200, "response_fixture": tampered_changed["fixture_id"], "expected_errors": ["signature payload digest mismatch"]},
      {"id": "changes-since-changed-set-unknown-key", "validator": "changes_since", "vehicle_id": vehicle_id, "status": 200, "response_fixture": unknown_key_changed["fixture_id"], "expected_errors": ["signature key is unknown"]},
      {"id": "changes-since-changed-set-invalid-signature", "validator": "changes_since", "vehicle_id": vehicle_id, "status": 200, "response_fixture": invalid_signature_changed["fixture_id"], "expected_errors": ["signature verification failed"]},
      {"id": "changes-since-changed-set-cross-schema", "validator": "changes_since", "request_fixture": request_example["fixture_id"], "status": 200, "response_fixture": cross_schema_changed["fixture_id"], "expected_errors": ["changed-set schema differs from base"]},
      {"id": "changes-since-changed-set-cross-vehicle", "validator": "changes_since", "vehicle_id": vehicle_id, "status": 200, "response_fixture": cross_vehicle_changed["fixture_id"], "expected_errors": ["signed response vehicle does not match route"]},
      {"id": "schema-2-2-multi-chunk-manifest-noncontiguous", "validator": "manifest", "vehicle_id": vehicle_id, "status": 200, "response_fixture": noncontiguous_manifest["fixture_id"], "expected_errors": ["manifest chunks are not contiguous"]},
      {"id": "sync-noop-unavailable-cacheable", "validator": "noop", "vehicle_id": vehicle_id, "status": 406, "response_fixture": cacheable_noop["fixture_id"], "expected_errors": ["no-op unavailable must be empty no-store"]},
      {"id": "signing-keys-vehicle-bound", "validator": "signing_keys", "vehicle_id": vehicle_id, "status": 200, "response_fixture": signing_keys_example["fixture_id"], "expected_errors": []},
      *[{"id": "changes-since-request-invalid-" + suffix, "validator": "changes_since_request_error", "vehicle_id": vehicle_id, "request_fixture": value["fixture_id"], "status": 422, "response_fixture": error_examples["changes-since-error-invalid-request"]["fixture_id"], "expected_errors": []} for suffix, value in invalid_request_examples.items()],
      {"id": "changes-since-request-reversed-range", "validator": "changes_since_request_error", "vehicle_id": vehicle_id, "request_fixture": reversed_range_request["fixture_id"], "status": 422, "response_fixture": error_examples["changes-since-error-invalid-schema-range"]["fixture_id"], "expected_errors": []},
      {"id": "changes-since-request-base-excluded", "validator": "changes_since_request_error", "vehicle_id": vehicle_id, "request_fixture": base_excluded_request["fixture_id"], "status": 422, "response_fixture": error_examples["changes-since-error-invalid-schema-range"]["fixture_id"], "expected_errors": []},
      *[{"id": name, "validator": "changes_since", "vehicle_id": vehicle_id, "status": value["response"]["status"], "response_fixture": value["fixture_id"], "expected_errors": [], **({"request_fixture": unsupported_request["fixture_id"]} if name == "changes-since-error-schema-range-unsupported" else {"request_fixture": unknown_base_receipt_request["fixture_id"]} if name == "changes-since-error-unknown-base-receipt" else {})} for name, value in error_examples.items() if name != "changes-since-error-invalid-request"],
      *[
          {"id": prefix + ("-response-at-limit" if body_bytes == MAX_RESPONSE_BYTES else "-response-over-limit"),
           "validator": "control_response_body", "response_type": response_type, "vehicle_id": vehicle_id,
           "status": status, "response_fixture": response_boundary_examples[f"{prefix}-response-{body_bytes}-bytes"]["fixture_id"],
           "expected_errors": [] if body_bytes == MAX_RESPONSE_BYTES else ["response body exceeds 2097152 bytes"]}
          for prefix, response_type, status, _, _ in response_boundary_specs
          for body_bytes in (MAX_RESPONSE_BYTES, MAX_RESPONSE_BYTES + 1)
      ],
    ]}
    out = {"profile.json": profile, "changes-since-request.schema.json": request,
           "changed-set-receipt.schema.json": receipt, "rebase-hint.schema.json": rebase,
           "sync-manifest.schema.json": manifest, "noop.schema.json": noop, "prepared-artefact.schema.json": prepared, "signing-keys.schema.json": signing_keys_schema, "fixture-signing-keys.json": signing_keys, "sync-error.schema.json": sync_error, "status-tables.schema.json": status_tables_schema, "status-tables.json": status_tables,
           "openapi.json": openapi, "cases.json": cases,
           "examples/changes-since-request.json": request_example,
           "examples/changes-since-changed-set.json": receipt_example,
           "examples/changes-since-rebase-after-compaction.json": rebase_example,
           "examples/schema-2-1-single-pack-manifest.json": manifest_2_1_example,
           "examples/schema-2-2-multi-chunk-manifest.json": manifest_2_2_example,
           "examples/schema-2-2-single-small-chunk-manifest.json": manifest_2_2_small_example,
           "examples/bootstrap-hub-sync-v1-1-3-selected.json": bootstrap_request,
           "examples/bootstrap-schema-list-only-legacy.json": legacy_bootstrap_request,
           **{"examples/" + name + ".json": value for name, value in rejected_bootstrap_examples.items()},
           "examples/changes-since-no-change.json": changes_noop_example,
           "examples/sync-noop-signed.json": noop_example,
           "examples/sync-noop-unavailable.json": noop_unavailable,
           "examples/changes-since-changed-set-tampered.json": tampered_changed,
           "examples/changes-since-changed-set-unknown-key.json": unknown_key_changed,
           "examples/changes-since-changed-set-invalid-signature.json": invalid_signature_changed,
           "examples/changes-since-changed-set-cross-schema.json": cross_schema_changed,
           "examples/changes-since-changed-set-cross-vehicle.json": cross_vehicle_changed,
           "examples/schema-2-2-multi-chunk-manifest-noncontiguous.json": noncontiguous_manifest,
           "examples/sync-noop-unavailable-cacheable.json": cacheable_noop,
           "examples/prepared-artefact-map-months-routes.json": prepared_example,
           "examples/signing-keys-vehicle-bound.json": signing_keys_example,
           **{"examples/changes-since-request-invalid-" + suffix + ".json": value for suffix, value in invalid_request_examples.items()},
           "examples/changes-since-request-unsupported-range.json": unsupported_request,
           "examples/changes-since-request-reversed-range.json": reversed_range_request,
           "examples/changes-since-request-base-excluded.json": base_excluded_request,
           "examples/changes-since-request-unknown-base-receipt.json": unknown_base_receipt_request,
           "examples/changes-since-request-8192-bytes.json": request_at_limit,
           "examples/changes-since-request-8193-bytes.json": request_over_limit,
           **{"examples/" + name + ".json": value for name, value in response_boundary_examples.items()},
           **{"examples/" + name + ".json": value for name, value in error_examples.items()}}
    encoded = {name: (json.dumps(value, sort_keys=True, indent=2) + "\n").encode() for name, value in out.items()}
    sums = "".join(f"{hashlib.sha256(encoded[name]).hexdigest()}  {name}\n" for name in sorted(encoded))
    return {**encoded, "SHA256SUMS": sums.encode()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    files = bundle()
    stale = [name for name, value in files.items() if not (PROFILE / name).is_file() or (PROFILE / name).read_bytes() != value]
    if args.check:
        if stale:
            raise SystemExit("stale Hub sync artifacts: " + ", ".join(stale))
    else:
        for name, value in files.items():
            path = PROFILE / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(value)
    print(PROFILE_ID + " sha256=" + hashlib.sha256(files["SHA256SUMS"]).hexdigest())

if __name__ == "__main__":
    main()
