#!/usr/bin/env python3
"""Export the source-neutral Hub changes-since contract."""
import argparse
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.0.0"
PROFILE_ID = "hub-sync-v1@1.0.0"
DRAFT = "https://json-schema.org/draft/2020-12/schema"
DIGEST = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
OPAQUE = {"type": "string", "minLength": 1, "maxLength": 4096, "pattern": "^[A-Za-z0-9._~-]+$"}
UUID = {"type": "string", "format": "uuid", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"}
SCHEMA_VERSION = {"type": "string", "enum": ["2.1", "2.2"]}
SIGNATURE = {"type": "string", "pattern": "^[A-Za-z0-9+/]{86}==$"}


def strict(properties, required=None):
    return {"type": "object", "additionalProperties": False, "properties": properties,
            "required": list(properties) if required is None else required}


def document(name, body):
    return {"$schema": DRAFT, "$id": f"urn:teslatlas:hub-sync-v1:1.0.0:{name}", **body}


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


def bundle():
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
    request_example = {
        "fixture_id": "changes-since-request-v1",
        "request": {"base_receipt_id": "receipt_demo_000042", "from_sequence": 42,
                    "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}},
    }
    signature = {"algorithm": "ed25519", "key_id": "fixture-ed25519-2026", "signed_payload_sha256": "1" * 64,
                 "signature": "A" * 86 + "=="}
    receipt_example = {
        "fixture_id": "changes-since-changed-set-v1",
        "receipt": {"receipt_id": "receipt_demo_000045", "vehicle_id": "11111111-1111-4111-8111-111111111111",
                    "base_receipt_id": "receipt_demo_000042", "from_sequence": 42, "to_sequence": 45,
                    "manifest_schema": "2.2", "changed_set_sha256": "2" * 64,
                    "pack": {"object_name": "packs/demo-changed-set-000045.sqlite.zst", "sha256": "3" * 64,
                             "compressed_bytes": 1048576}, "signature": signature},
    }
    rebase_example = {
        "fixture_id": "changes-since-rebase-after-compaction-v1",
        "response": {"kind": "rebase_required", "vehicle_id": "11111111-1111-4111-8111-111111111111",
                     "requested_base_receipt_id": "receipt_compacted_000042", "requested_from_sequence": 42,
                     "reason": "compacted", "replacement": {"receipt_id": "receipt_retained_000900", "sequence": 900,
                     "manifest_schema": "2.2", "pack": {"object_name": "packs/demo-rebase-000900.sqlite.zst",
                     "sha256": "4" * 64, "compressed_bytes": 2097152}},
                     "retry_request": {"base_receipt_id": "receipt_retained_000900", "from_sequence": 900,
                     "schema_version_range": {"minimum": "2.1", "maximum": "2.2"}},
                     "signature": {**signature, "signed_payload_sha256": "5" * 64}},
    }
    profile = {
        "profile_id": PROFILE_ID, "status": "candidate", "contract_version": "1.0.0",
        "authentication": "paired bearer",
        "product_version_binding": "none; this protocol profile has no exact Hub product-version pin",
        "schema_version_range": {"minimum": "2.1", "maximum": "2.2"},
        "limits": {"max_changed_set_packs": 1, "max_pack_compressed_bytes": 16 * 1024 * 1024,
                   "max_request_bytes": 8192, "max_response_bytes": 2 * 1024 * 1024},
        "route": {"method": "POST", "path": "/v1/vehicles/{vehicle_id}/sync/changes-since",
                  "success_status": 200, "compacted_status": 409},
        "signature_rule": "Each receipt or rebase hint MUST carry an Ed25519 detached signature over RFC 8785 canonical JSON of the object with its signature member omitted. signed_payload_sha256 is the SHA-256 of those canonical bytes. Fixtures use a deterministic shape-only signature and MUST NOT be accepted as cryptographic proof.",
        "compaction_rule": "A compacted base MUST return 409 and the signed rebase hint. The client applies replacement.pack, persists replacement receipt_id and sequence, then sends retry_request. It MUST NOT infer a rebase target or substitute a full-history request.",
        "scope": "One changed pack per changed-set receipt. Multi-chunk transfer, prepared-compute artifacts, and no-op delivery are separate contracts.",
    }
    openapi = {"openapi": "3.1.0", "info": {"title": "Teslatlas changes-since", "version": "1.0.0"},
      "paths": {"/v1/vehicles/{vehicle_id}/sync/changes-since": {"post": {"operationId": "changesSince",
        "parameters": [{"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}],
        "requestBody": {"required": True, "content": {"application/json": {"schema": {"$ref": "changes-since-request.schema.json#/$defs/request"}}}},
        "security": [{"pairedBearer": []}],
        "responses": {"200": {"description": "signed changed-set receipt", "content": {"application/json": {"schema": {"$ref": "changed-set-receipt.schema.json#/$defs/receipt"}}}},
                      "401": {"description": "missing, invalid, expired, or revoked paired bearer", "content": {}},
                      "409": {"description": "signed rebase hint after compaction", "content": {"application/json": {"schema": {"$ref": "rebase-hint.schema.json#/$defs/hint"}}}}}}}},
      "components": {"securitySchemes": {"pairedBearer": {"type": "http", "scheme": "bearer", "bearerFormat": "opaque paired bearer"}}}}
    cases = {"profile_id": PROFILE_ID, "cases": [
      {"id": "changes-since-changed-set", "request_fixture": request_example["fixture_id"], "status": 200, "response_fixture": receipt_example["fixture_id"]},
      {"id": "changes-since-rebase-after-compaction", "request_fixture": request_example["fixture_id"], "status": 409, "response_fixture": rebase_example["fixture_id"]},
    ]}
    out = {"profile.json": profile, "changes-since-request.schema.json": request,
           "changed-set-receipt.schema.json": receipt, "rebase-hint.schema.json": rebase,
           "openapi.json": openapi, "cases.json": cases,
           "examples/changes-since-request.json": request_example,
           "examples/changes-since-changed-set.json": receipt_example,
           "examples/changes-since-rebase-after-compaction.json": rebase_example}
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
