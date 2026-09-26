#!/usr/bin/env python3
"""Export the source-neutral Hub changes-since contract."""
import argparse
import base64
import hashlib
import json
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.3.0"
PROFILE_ID = "hub-sync-v1@1.3.0"
DRAFT = "https://json-schema.org/draft/2020-12/schema"
DIGEST = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
OPAQUE = {"type": "string", "minLength": 1, "maxLength": 4096, "pattern": "^[A-Za-z0-9._~-]+$"}
UUID = {"type": "string", "format": "uuid", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"}
SCHEMA_VERSION = {"type": "string", "enum": ["2.1", "2.2"]}
SIGNATURE = {"type": "string", "pattern": "^[A-Za-z0-9+/]{86}==$"}
PUBLIC_KEY = {"type": "string", "pattern": "^[A-Za-z0-9+/]{43}=$"}
# RFC 8032 test-vector seed. It is a public, deterministic fixture value only.
FIXTURE_SIGNING_SEED = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
FIXTURE_KEY_ID = "fixture-ed25519-rfc8032-1"


def strict(properties, required=None):
    return {"type": "object", "additionalProperties": False, "properties": properties,
            "required": list(properties) if required is None else required}


def document(name, body):
    return {"$schema": DRAFT, "$id": f"urn:teslatlas:hub-sync-v1:1.3.0:{name}", **body}


def pack_ref():
    return strict({
        "object_name": {"type": "string", "pattern": "^[a-z0-9][a-z0-9._/-]{0,1023}$"},
        "sha256": DIGEST,
        "compressed_bytes": {"type": "integer", "minimum": 8 * 1024 * 1024, "maximum": 16 * 1024 * 1024},
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
    fixture_public_key = base64.b64encode(
        Ed25519PrivateKey.from_private_bytes(FIXTURE_SIGNING_SEED).public_key().public_bytes(
            serialization.Encoding.Raw, serialization.PublicFormat.Raw
        )
    ).decode()
    request = document("changes-since-request", {"$defs": {
        "request": strict({
            "base_receipt_id": OPAQUE,
            "from_sequence": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1},
            "schema_version_range": strict({"minimum": SCHEMA_VERSION, "maximum": SCHEMA_VERSION}),
        })
    }})
    receipt = document("changed-set-receipt", {"$defs": {
        "receipt": strict({
            "receipt_id": OPAQUE,
            "vehicle_id": UUID,
            "base_receipt_id": OPAQUE,
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
            "requested_from_sequence": {"type": "integer", "minimum": 0, "maximum": 2**63 - 1},
            "reason": {"const": "compacted"},
            "replacement": strict({
                "receipt_id": OPAQUE,
                "sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
                "manifest_schema": SCHEMA_VERSION,
                "pack": pack_ref(),
            }),
            "retry_request": strict({
                "base_receipt_id": OPAQUE,
                "from_sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
                "schema_version_range": strict({"minimum": SCHEMA_VERSION, "maximum": SCHEMA_VERSION}),
            }),
            "signature": signing(),
        }),
    }})
    chunk = strict({
        "chunk_index": {"type": "integer", "minimum": 0, "maximum": 4095},
        "pack": pack_ref(),
    })
    manifest = document("sync-manifest", {"$defs": {
        "schema_2_1": strict({
            "manifest_id": OPAQUE, "vehicle_id": UUID, "kind": {"const": "snapshot"},
            "schema_version": {"const": "2.1"}, "sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "pack": pack_ref(), "signature": signing(),
        }),
        "schema_2_2": strict({
            "manifest_id": OPAQUE, "vehicle_id": UUID, "kind": {"const": "snapshot"},
            "schema_version": {"const": "2.2"}, "sequence": {"type": "integer", "minimum": 1, "maximum": 2**63 - 1},
            "chunks": {"type": "array", "minItems": 2, "maxItems": 4096, "items": chunk}, "signature": signing(),
        }),
        "manifest": {"oneOf": [{"$ref": "#/$defs/schema_2_1"}, {"$ref": "#/$defs/schema_2_2"}]},
    }})
    noop = document("noop", {"$defs": {
        "receipt": strict({
            "kind": {"const": "no_op"}, "vehicle_id": UUID, "base_receipt_id": OPAQUE,
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
            "dirty_spans": strict({"map_months": {"type": "array", "maxItems": 120, "items": {"$ref": "#/$defs/map_month"}}, "routes": {"type": "array", "maxItems": 10000, "items": {"$ref": "#/$defs/route"}}}),
            "pack": pack_ref(),
            "signature": signing(),
        }),
    }})
    prepared["$defs"]["receipt"]["properties"]["dirty_spans"]["anyOf"] = [
        {"properties": {"map_months": {"minItems": 1}}},
        {"properties": {"routes": {"minItems": 1}}},
    ]
    status_tables_schema = document("status-tables", {"$defs": {
        "response": strict({
            "status": {"type": "integer", "minimum": 100, "maximum": 599},
            "body": {"type": "string", "minLength": 1, "maxLength": 256},
            "headers": {"type": "object", "additionalProperties": False, "properties": {"cache_control": {"type": "string"}, "etag": {"type": "string"}, "content_range": {"type": "string"}}},
            "signature": {"enum": ["present", "absent"]},
        }, ["status", "body"]),
        "table": strict({"route": {"type": "string", "minLength": 1, "maxLength": 512}, "responses": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"$ref": "#/$defs/response"}}}),
        "document": strict({"profile_id": {"const": PROFILE_ID}, "tables": {"type": "array", "minItems": 4, "maxItems": 16, "items": {"$ref": "#/$defs/table"}}}),
    }})
    status_tables = {
        "profile_id": PROFILE_ID,
        "tables": [
            {"route": "POST /v1/vehicles/{vehicle_id}/sync/changes-since", "responses": [
                {"status": 200, "body": "signed changed-set receipt"},
                {"status": 401, "body": "empty", "signature": "absent"},
                {"status": 409, "body": "signed rebase hint"}]},
            {"route": "GET /v1/vehicles/{vehicle_id}/sync/manifest", "responses": [
                {"status": 200, "body": "one signed schema 2.1 or 2.2 manifest"},
                {"status": 401, "body": "empty", "signature": "absent"},
                {"status": 406, "body": "empty", "headers": {"cache_control": "no-store"}, "signature": "absent"}]},
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
        ],
    }
    request_example = {
        "fixture_id": "changes-since-request-v1",
        "request": {"base_receipt_id": "receipt_demo_000042", "from_sequence": 42,
                    "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}},
    }
    receipt_example = {
        "fixture_id": "changes-since-changed-set-v1",
        "receipt": {"receipt_id": "receipt_demo_000045", "vehicle_id": "11111111-1111-4111-8111-111111111111",
                    "base_receipt_id": "receipt_demo_000042", "from_sequence": 42, "to_sequence": 45,
                    "manifest_schema": "2.2", "changed_set_sha256": "2" * 64,
                    "pack": {"object_name": "packs/demo-changed-set-000045.sqlite.zst", "sha256": "3" * 64,
                             "compressed_bytes": 8 * 1024 * 1024}},
    }
    rebase_example = {
        "fixture_id": "changes-since-rebase-after-compaction-v1",
        "response": {"kind": "rebase_required", "vehicle_id": "11111111-1111-4111-8111-111111111111",
                     "requested_base_receipt_id": "receipt_compacted_000042", "requested_from_sequence": 42,
                     "reason": "compacted", "replacement": {"receipt_id": "receipt_retained_000900", "sequence": 900,
                     "manifest_schema": "2.2", "pack": {"object_name": "packs/demo-rebase-000900.sqlite.zst",
                     "sha256": "4" * 64, "compressed_bytes": 8 * 1024 * 1024}},
                     "retry_request": {"base_receipt_id": "receipt_retained_000900", "from_sequence": 900,
                     "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}},
                     },
    }
    manifest_2_1_example = {
        "fixture_id": "schema-2-1-single-pack-manifest-v1",
        "manifest": {"manifest_id": "manifest_demo_000899", "vehicle_id": "11111111-1111-4111-8111-111111111111",
                     "kind": "snapshot", "schema_version": "2.1", "sequence": 899,
                     "pack": {"object_name": "packs/demo-000899.sqlite.zst", "sha256": "a" * 64, "compressed_bytes": 8 * 1024 * 1024},
                     },
    }
    manifest_2_2_example = {
        "fixture_id": "schema-2-2-multi-chunk-manifest-v1",
        "manifest": {"manifest_id": "manifest_demo_000900", "vehicle_id": "11111111-1111-4111-8111-111111111111",
                     "kind": "snapshot", "schema_version": "2.2", "sequence": 900,
                     "chunks": [
                         {"chunk_index": 0, "pack": {"object_name": "packs/demo-000900-0000.sqlite.zst", "sha256": "6" * 64, "compressed_bytes": 8 * 1024 * 1024}},
                         {"chunk_index": 1, "pack": {"object_name": "packs/demo-000900-0001.sqlite.zst", "sha256": "7" * 64, "compressed_bytes": 8 * 1024 * 1024}},
                     ]},
    }
    noop_example = {
        "fixture_id": "sync-noop-signed-v1",
        "receipt": {"kind": "no_op", "vehicle_id": "11111111-1111-4111-8111-111111111111",
                    "base_receipt_id": "receipt_demo_000045", "sequence": 45, "manifest_schema": "2.2",
                    },
    }
    noop_unavailable = {
        "fixture_id": "sync-noop-unavailable-v1",
        "response": {"status": 406, "headers": {"cache_control": "no-store"}, "body_bytes": 0, "manifest_signature": "absent"},
    }
    prepared_example = {
        "fixture_id": "prepared-artefact-map-months-routes-v1",
        "receipt": {"artifact_id": "prepared_demo_000900", "artifact_type": "map_months_and_routes", "vehicle_id": "11111111-1111-4111-8111-111111111111",
                    "source": {"kind": "hub_compute", "input_manifest_id": "manifest_demo_000900", "input_sequence": 900},
                    "window": {"from_ms": 1735689600000, "to_ms": 1740787200000},
                    "generation": {"generation_id": "generation_demo_000001", "generated_at_ms": 1740790800000},
                    "units": {"distance": "km", "time": "ms", "coordinates": "wgs84_degrees"}, "algorithm_version": "1.0.0",
                    "dirty_spans": {"map_months": [
                        {"month": "2025-01", "from_ms": 1735689600000, "to_ms": 1738368000000, "reason": "changed"},
                        {"month": "2025-02", "from_ms": 1738368000000, "to_ms": 1740787200000, "reason": "changed"}],
                        "routes": [{"route_id": "route_demo_000045", "from_ms": 1738454400000, "to_ms": 1738458000000, "reason": "changed"}]},
                    "pack": {"object_name": "packs/prepared-demo-000900.sqlite.zst", "sha256": "c" * 64, "compressed_bytes": 8 * 1024 * 1024},
                    },
    }
    for fixture in (receipt_example["receipt"], rebase_example["response"],
                    manifest_2_1_example["manifest"], manifest_2_2_example["manifest"],
                    noop_example["receipt"], prepared_example["receipt"]):
        sign_fixture(fixture)
    signing_keys_schema = document("signing-keys", {"$defs": {
        "key": strict({"algorithm": {"const": "ed25519"}, "key_id": {"type": "string", "minLength": 1, "maxLength": 128, "pattern": "^[A-Za-z0-9._-]+$"}, "public_key": PUBLIC_KEY}),
        "document": strict({"profile_id": {"const": PROFILE_ID}, "key_set_id": OPAQUE,
            "keys": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"$ref": "#/$defs/key"}},
            "rotation": strict({"key_selection": {"const": "key_id"}, "publish_before_use": {"const": True}, "retain_retired_keys": {"const": True}})}),
    }})
    signing_keys = {"profile_id": PROFILE_ID, "key_set_id": "fixture-ed25519-rfc8032", "keys": [
        {"algorithm": "ed25519", "key_id": FIXTURE_KEY_ID, "public_key": fixture_public_key}],
        "rotation": {"key_selection": "key_id", "publish_before_use": True, "retain_retired_keys": True}}
    profile = {
        "profile_id": PROFILE_ID, "status": "candidate", "contract_version": "1.3.0",
        "authentication": "paired bearer",
        "product_version_binding": "none; this protocol profile has no exact Hub product-version pin",
        "schema_version_range": {"minimum": "2.1", "maximum": "2.2"},
        "previous_profile": "hub-sync-v1@1.2.0",
        "limits": {"max_changed_set_packs": 1, "min_pack_compressed_bytes": 8 * 1024 * 1024,
                   "max_pack_compressed_bytes": 16 * 1024 * 1024, "max_manifest_chunks": 4096,
                   "max_request_bytes": 8192, "max_response_bytes": 2 * 1024 * 1024},
        "status_tables": "status-tables.json",
        "prepared_artefact": "prepared-artefact.schema.json",
        "fixture_signing_keys": "fixture-signing-keys.json",
        "route": {"method": "POST", "path": "/v1/vehicles/{vehicle_id}/sync/changes-since",
                  "success_status": 200, "compacted_status": 409},
        "signature_rule": "Each receipt or rebase hint MUST carry an Ed25519 detached signature over RFC 8785 canonical JSON of the object with its signature member omitted. signed_payload_sha256 is the SHA-256 of those canonical bytes. Fixtures carry deterministic Ed25519 signatures using only the published fixture public key; they are test vectors, not production trust anchors.",
        "compaction_rule": "A compacted base MUST return 409 and the signed rebase hint. The client applies replacement.pack, persists replacement receipt_id and sequence, then sends retry_request. It MUST NOT infer a rebase target or substitute a full-history request.",
        "scope": "One changed pack per changed-set receipt; schema 2.2 snapshot manifests may contain multiple chunks under one signature; prepared artefacts contain only changed map-month and route spans. No other prepared-compute artefact type is defined.",
    }
    openapi = {"openapi": "3.1.0", "info": {"title": "Teslatlas changes-since", "version": "1.3.0"},
      "paths": {"/v1/vehicles/{vehicle_id}/sync/changes-since": {"post": {"operationId": "changesSince",
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}],
        "requestBody": {"required": True, "content": {"application/json": {"schema": {"$ref": "changes-since-request.schema.json#/$defs/request"}}}},
        "security": [{"pairedBearer": []}],
        "responses": {"200": {"description": "signed changed-set receipt", "content": {"application/json": {"schema": {"$ref": "changed-set-receipt.schema.json#/$defs/receipt"}}}},
                      "401": {"description": "missing, invalid, expired, or revoked paired bearer", "content": {}},
                      "409": {"description": "signed rebase hint after compaction", "content": {"application/json": {"schema": {"$ref": "rebase-hint.schema.json#/$defs/hint"}}}}}}}},
      "components": {"securitySchemes": {"pairedBearer": {"type": "http", "scheme": "bearer", "bearerFormat": "opaque paired bearer"}}}}
    openapi["paths"]["/v1/vehicles/{vehicle_id}/sync/manifest"] = {"get": {"operationId": "syncManifest", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}, {"name": "x-teslatlas-supported-schemas", "in": "header", "required": True, "schema": {"type": "string", "pattern": "^2\\.1,2\\.2$"}}],
        "responses": {"200": {"description": "signed manifest", "content": {"application/json": {"schema": {"$ref": "sync-manifest.schema.json#/$defs/manifest"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "406": {"description": "empty unavailable response with Cache-Control: no-store", "content": {}}}}}
    openapi["paths"]["/v1/vehicles/{vehicle_id}/sync/noop"] = {"get": {"operationId": "syncNoop", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}],
        "responses": {"200": {"description": "signed no-op receipt", "content": {"application/json": {"schema": {"$ref": "noop.schema.json#/$defs/receipt"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "406": {"description": "empty unavailable response with Cache-Control: no-store", "content": {}}}}}
    openapi["paths"]["/v1/packs/sha256/{object_name}"] = {"get": {"operationId": "syncPack", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "object_name", "in": "path", "required": True, "schema": {"type": "string", "minLength": 1, "maxLength": 1024}}],
        "responses": {"200": {"description": "pack bytes", "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}}, "206": {"description": "single byte range", "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "404": {"description": "empty missing pack", "content": {}}}}}
    openapi["paths"]["/v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}"] = {"get": {"operationId": "preparedArtefact", "security": [{"pairedBearer": []}],
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}, {"name": "artifact_id", "in": "path", "required": True, "schema": OPAQUE}],
        "responses": {"200": {"description": "signed prepared-artefact receipt", "content": {"application/json": {"schema": {"$ref": "prepared-artefact.schema.json#/$defs/receipt"}}}}, "401": {"description": "empty authentication failure", "content": {}}, "404": {"description": "empty missing artefact", "content": {}}}}}
    cases = {"profile_id": PROFILE_ID, "cases": [
      {"id": "changes-since-changed-set", "request_fixture": request_example["fixture_id"], "status": 200, "response_fixture": receipt_example["fixture_id"]},
      {"id": "changes-since-rebase-after-compaction", "request_fixture": request_example["fixture_id"], "status": 409, "response_fixture": rebase_example["fixture_id"]},
      {"id": "schema-2-1-single-pack-manifest", "status": 200, "response_fixture": manifest_2_1_example["fixture_id"]},
      {"id": "schema-2-2-multi-chunk-manifest", "status": 200, "response_fixture": manifest_2_2_example["fixture_id"]},
      {"id": "sync-noop-signed", "status": 200, "response_fixture": noop_example["fixture_id"]},
      {"id": "sync-noop-unavailable", "status": 406, "response_fixture": noop_unavailable["fixture_id"]},
      {"id": "prepared-artefact-map-months-routes", "status": 200, "response_fixture": prepared_example["fixture_id"]},
    ]}
    out = {"profile.json": profile, "changes-since-request.schema.json": request,
           "changed-set-receipt.schema.json": receipt, "rebase-hint.schema.json": rebase,
           "sync-manifest.schema.json": manifest, "noop.schema.json": noop, "prepared-artefact.schema.json": prepared, "signing-keys.schema.json": signing_keys_schema, "fixture-signing-keys.json": signing_keys, "status-tables.schema.json": status_tables_schema, "status-tables.json": status_tables,
           "openapi.json": openapi, "cases.json": cases,
           "examples/changes-since-request.json": request_example,
           "examples/changes-since-changed-set.json": receipt_example,
           "examples/changes-since-rebase-after-compaction.json": rebase_example,
           "examples/schema-2-1-single-pack-manifest.json": manifest_2_1_example,
           "examples/schema-2-2-multi-chunk-manifest.json": manifest_2_2_example,
           "examples/sync-noop-signed.json": noop_example,
           "examples/sync-noop-unavailable.json": noop_unavailable,
           "examples/prepared-artefact-map-months-routes.json": prepared_example}
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
