import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import sqlite3
import struct
import subprocess
import sys
import tempfile
import unittest

from jsonschema import Draft202012Validator, FormatChecker
from openapi_spec_validator import validate as validate_openapi
import zstandard

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.3.0"
VEHICLE_ID = "11111111-1111-4111-8111-111111111111"
OTHER_VEHICLE_ID = "22222222-2222-4222-8222-222222222222"
I_JSON_MAX_INTEGER = 2**53 - 1


class HubSyncProfileTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("hub_sync", ROOT / "conformance/hub_sync.py")
        cls.sync = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.sync)

    def fixture(self, name):
        return json.loads((PROFILE / "examples" / (name + ".json")).read_text())

    def test_bundle_is_deterministic_and_has_no_product_version_pin(self):
        result = subprocess.run([sys.executable, str(ROOT / "tools/build_hub_sync_profile.py"), "--check"], capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        profile = self.sync.load_profile()
        self.assertEqual(profile["profile_id"], "hub-sync-v1@1.3.0")
        self.assertEqual(profile["product_version_binding"], "none; this protocol profile has no exact Hub product-version pin")
        self.assertEqual(profile["authentication"], "paired bearer")
        self.assertEqual(profile["previous_profile"], "hub-sync-v1@1.2.0")
        self.assertEqual(profile["limits"]["max_changed_set_packs"], 1)
        self.assertEqual(profile["limits"]["max_manifest_chunks"], 1771)
        self.assertEqual(profile["limits"]["max_prepared_uncompressed_bytes"], 64 * 1024 * 1024)
        self.assertEqual(profile["limits"]["max_prepared_tiles_per_month"], 4096)
        self.assertEqual(profile["limits"]["max_prepared_segments_per_tile"], 200_000)
        self.assertEqual(profile["limits"]["max_prepared_tile_bytes"], 200_000 * 8)
        self.assertEqual(
            profile["limits"]["max_prepared_publication_bytes_per_month"],
            8 * 1024 * 1024,
        )
        self.assertEqual(profile["limits"]["max_request_bytes"], 8192)
        self.assertEqual(profile["limits"]["min_pack_compressed_bytes"], 1)
        self.assertEqual(profile["limits"]["target_pack_compressed_bytes"], 8 * 1024 * 1024)
        self.assertEqual(profile["limits"]["max_i_json_integer"], I_JSON_MAX_INTEGER)
        contract = json.loads((PROFILE / profile["prepared_pack_contract"]).read_text())
        self.assertEqual(contract["map_tiles"]["max_segments_per_tile"], 200_000)
        self.assertEqual(
            contract["map_tiles"]["app_repository_bytes_per_month"],
            "8 + sum(40 + length(segments))",
        )
        self.assertEqual(
            profile["bootstrap_selector"],
            {
                "header": "x-teslatlas-sync-profile",
                "value": "hub-sync-v1@1.3.0",
                "required_schema_header": "x-teslatlas-supported-schemas",
                "required_schema_value": "2.1,2.2",
                "without_selector": "legacy-sync-manifest",
                "invalid_explicit_selector": {
                    "status": 406,
                    "body_bytes": 0,
                    "cache_control": "no-store",
                    "manifest_signature": "absent",
                },
            },
        )
        frozen_profiles = {
            "1.0.0": "875c42ffceab748e4d81b6356fdbe698e8b107a8c47c9abf33ad131f2fd34c7b",
            "1.1.0": "0a6db376b69071f79bed3f437a7ae564ffa6378fec2b944340eb0b5306d63e3a",
            "1.2.0": "331920326476ffe8d3e382181a73937d478c613793c3cc5c0a7e2db14e091ed5",
        }
        for version, digest in frozen_profiles.items():
            with self.subTest(version=version):
                frozen = ROOT / f"profiles/hub-sync-v1/{version}/SHA256SUMS"
                self.assertEqual(hashlib.sha256(frozen.read_bytes()).hexdigest(), digest)

    def test_schemas_and_openapi_are_independently_valid(self):
        for name in ("changes-since-request.schema.json", "changed-set-receipt.schema.json", "rebase-hint.schema.json", "sync-manifest.schema.json", "noop.schema.json", "prepared-artefact.schema.json", "signing-keys.schema.json", "sync-error.schema.json", "status-tables.schema.json"):
            Draft202012Validator.check_schema(json.loads((PROFILE / name).read_text()))
        validate_openapi(json.loads((PROFILE / "openapi.json").read_text()), base_uri=PROFILE.as_uri() + "/")

    def test_every_integer_wire_field_is_i_json_safe(self):
        schema_names = (
            "changes-since-request.schema.json",
            "changed-set-receipt.schema.json",
            "rebase-hint.schema.json",
            "sync-manifest.schema.json",
            "noop.schema.json",
            "prepared-artefact.schema.json",
            "signing-keys.schema.json",
        )
        integer_fields = []

        def collect(value, path):
            if isinstance(value, dict):
                if value.get("type") == "integer":
                    integer_fields.append((path, value))
                for key, item in value.items():
                    collect(item, path + "/" + key)
            elif isinstance(value, list):
                for index, item in enumerate(value):
                    collect(item, path + "/" + str(index))

        for name in schema_names:
            collect(json.loads((PROFILE / name).read_text()), name)
        self.assertTrue(integer_fields)
        for path, field in integer_fields:
            with self.subTest(path=path):
                self.assertGreaterEqual(field["minimum"], -I_JSON_MAX_INTEGER)
                self.assertLessEqual(field["maximum"], I_JSON_MAX_INTEGER)

    def test_changed_set_and_compaction_fixtures_are_deterministic(self):
        request_fixture = self.fixture("changes-since-request")
        request = request_fixture["request"]
        changed = self.fixture("changes-since-changed-set")["receipt"]
        rebase = self.fixture("changes-since-rebase-after-compaction")["response"]
        manifest_2_1 = self.fixture("schema-2-1-single-pack-manifest")["manifest"]
        manifest = self.fixture("schema-2-2-multi-chunk-manifest")["manifest"]
        small_manifest = self.fixture("schema-2-2-single-small-chunk-manifest")["manifest"]
        noop = self.fixture("sync-noop-signed")["receipt"]
        no_change = self.fixture("changes-since-no-change")["receipt"]
        unavailable = self.fixture("sync-noop-unavailable")["response"]
        prepared = self.fixture("prepared-artefact-map-months")["receipt"]
        status_tables = json.loads((PROFILE / "status-tables.json").read_text())
        vehicle_id = request_fixture["vehicle_id"]
        self.assertEqual(self.fixture("changes-since-request")["fixture_id"], "changes-since-request-v1")
        self.assertEqual(self.fixture("changes-since-changed-set")["fixture_id"], "changes-since-changed-set-v1")
        self.assertEqual(self.fixture("changes-since-rebase-after-compaction")["fixture_id"], "changes-since-rebase-after-compaction-v1")
        self.assertEqual(self.sync.validate_request(request), [])
        self.assertEqual(self.sync.validate_response(200, changed, vehicle_id), [])
        self.assertEqual(self.sync.validate_response(409, rebase, vehicle_id), [])
        self.assertEqual(self.fixture("schema-2-1-single-pack-manifest")["fixture_id"], "schema-2-1-single-pack-manifest-v1")
        self.assertEqual(self.fixture("schema-2-2-multi-chunk-manifest")["fixture_id"], "schema-2-2-multi-chunk-manifest-v1")
        self.assertEqual(self.sync.validate_manifest(manifest_2_1, vehicle_id), [])
        self.assertEqual(self.sync.validate_manifest(manifest, vehicle_id), [])
        self.assertEqual(self.sync.validate_manifest(small_manifest, vehicle_id), [])
        self.assertEqual(manifest["chunks"][-1]["pack"]["compressed_bytes"], 4096)
        self.assertEqual(small_manifest["chunks"][0]["pack"]["compressed_bytes"], 4096)
        self.assertEqual(manifest_2_1["receipt_id"], request["base_receipt_id"])
        self.assertEqual(manifest_2_1["schema_version"], request["base_manifest_schema"])
        self.assertEqual(self.sync.validate_response(200, no_change, vehicle_id), [])
        self.assertEqual(self.sync.validate_noop(200, noop, vehicle_id), [])
        self.assertEqual(self.fixture("prepared-artefact-map-months")["fixture_id"], "prepared-artefact-map-months-v1")
        self.assertEqual(self.sync.validate_prepared_artefact(prepared, vehicle_id), [])
        self.assertEqual(self.sync.validate_noop(unavailable["status"], {}, vehicle_id, {"Cache-Control": unavailable["headers"]["cache_control"]}, b""), [])
        self.assertEqual(self.sync.validate_status_tables(status_tables), [])
        self.assertEqual(rebase["reason"], "compacted")
        self.assertEqual(rebase["retry_request"]["base_receipt_id"], rebase["replacement"]["receipt_id"])
        self.assertEqual(rebase["retry_request"]["from_sequence"], rebase["replacement"]["sequence"])
        self.assertEqual(rebase["retry_request"]["base_manifest_schema"], rebase["replacement"]["manifest_schema"])
        self.assertEqual([item["chunk_index"] for item in rebase["replacement"]["chunks"]], [0, 1])

        keys = self.sync.load_signing_keys()
        self.assertEqual(keys["vehicle_id"], request_fixture["vehicle_id"])
        self.assertEqual(
            keys["keys"][0]["key_id"],
            "ed25519-sha256-21fe31dfa154a261626bf854046fd2271b7bed4b6abe45aa58877ef47f9721b9",
        )
        self.assertEqual(self.sync.validate_signing_keys(keys, request_fixture["vehicle_id"]), [])

    def test_identified_month_has_exact_signed_bytes_and_full_utc_window(self):
        fixture = self.fixture("prepared-artefact-identified-month")
        receipt = fixture["receipt"]
        compressed = (PROFILE / fixture["pack_file"]).read_bytes()
        self.assertEqual(self.sync.validate_prepared_pack(receipt, compressed, VEHICLE_ID), [])
        self.assertEqual(
            receipt["artifact_id"],
            "map-month-v1.98e39194b223fa4fd7515a0f7fa46d373d7413e76b7a2cc8c6178398f2bd3b7f",
        )
        self.assertEqual(self.sync.prepared_month_artifact_id(receipt, "2025-01"), receipt["artifact_id"])
        tampered = copy.deepcopy(receipt)
        tampered["artifact_id"] = "map-month-v1." + "0" * 64
        self.assertEqual(
            self.sync.validate_prepared_artefact(tampered, VEHICLE_ID),
            ["prepared artefact identity mismatch"],
        )

    def test_prepared_pack_is_bound_map_month_only_sqlite(self):
        fixture = self.fixture("prepared-artefact-map-months")
        receipt = fixture["receipt"]
        compressed = (PROFILE / fixture["pack_file"]).read_bytes()
        self.assertEqual(self.sync.validate_prepared_pack(receipt, compressed, VEHICLE_ID), [])
        self.assertEqual(receipt["map_style"], "route-stroke-v8-opaque")
        self.assertEqual(receipt["tile_geometry_version"], "raster-v9-rounded-tile-px")
        self.assertEqual(receipt["artifact_schema_version"], 1)
        self.assertEqual(self.sync._sqlite_record(b"\x02\x09", 1), (1,))
        for noncanonical_record in (
            b"\x02\x01\x01",
            b"\x80\x02\x09",
            b"\x02\x09x",
        ):
            with self.subTest(record=noncanonical_record):
                with self.assertRaises(ValueError):
                    self.sync._sqlite_record(noncanonical_record, 1)
        self.assertEqual(
            receipt["source"],
            {
                "kind": "hub_compute",
                "input_manifest_id": "manifest_demo_000900",
                "input_receipt_id": "receipt_demo_000900",
                "input_manifest_schema": "2.2",
                "input_sequence": 900,
            },
        )
        self.assertEqual(len(compressed), receipt["pack"]["compressed_bytes"])
        self.assertEqual(hashlib.sha256(compressed).hexdigest(), receipt["pack"]["sha256"])
        raw = zstandard.ZstdDecompressor().decompress(compressed, allow_extra_data=False)
        self.assertEqual(len(raw), receipt["pack"]["uncompressed_bytes"])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "pack.sqlite"
            path.write_bytes(raw)
            connection = sqlite3.connect(f"file:{path}?mode=ro&immutable=1", uri=True)
            try:
                self.assertEqual(
                    {row[0] for row in connection.execute("SELECT name FROM sqlite_schema WHERE type = 'table'")},
                    {"prepared_metadata", "map_months", "map_tiles"},
                )
                self.assertEqual(
                    [row[0] for row in connection.execute("SELECT segments FROM map_tiles ORDER BY zoom")],
                    [
                        struct.pack("<hhhh", -2, 0, 1, 1) + struct.pack("<hhhh", 0, 0, 2, 2),
                        struct.pack("<hhhh", -32768, -1, 0, 1) + struct.pack("<hhhh", 32767, 0, 1, 2),
                    ],
                )
                self.assertEqual(
                    connection.execute(
                        "SELECT month, resolution, drive_count, tile_count FROM map_months ORDER BY month"
                    ).fetchall(),
                    [("2025-01", "readyData", 1, 2), ("2025-02", "readyEmpty", 0, 0)],
                )
                self.assertEqual(
                    connection.execute(
                        "SELECT input_manifest_id, input_receipt_id, input_manifest_schema, input_sequence FROM prepared_metadata"
                    ).fetchone(),
                    ("manifest_demo_000900", "receipt_demo_000900", "2.2", 900),
                )
            finally:
                connection.close()

            other_writer_raw = bytearray(raw)
            other_writer_raw[96:100] = (3_054_000).to_bytes(4, "big")
            other_writer_path = Path(directory) / "other-writer.sqlite"
            other_writer_path.write_bytes(other_writer_raw)
            other_writer_connection = sqlite3.connect(
                f"file:{other_writer_path}?mode=ro&immutable=1", uri=True
            )
            try:
                other_writer_connection.execute("PRAGMA trusted_schema = OFF")
                other_writer_connection.execute("PRAGMA query_only = ON")
                contract = json.loads((PROFILE / "prepared-pack-v1-contract.json").read_text())
                self.assertTrue(
                    self.sync._prepared_sqlite_byte_domain_is_canonical(
                        other_writer_raw, other_writer_connection, contract
                    )
                )
                other_writer_raw[96:100] = (3_099_999).to_bytes(4, "big")
                self.assertFalse(
                    self.sync._prepared_sqlite_byte_domain_is_canonical(
                        other_writer_raw, other_writer_connection, contract
                    )
                )
            finally:
                other_writer_connection.close()

        ready_empty = self.fixture("prepared-artefact-ready-empty")
        ready_empty_pack = (PROFILE / ready_empty["pack_file"]).read_bytes()
        self.assertEqual(
            self.sync.validate_prepared_pack(ready_empty["receipt"], ready_empty_pack, VEHICLE_ID),
            [],
        )

        for fixture_name, marker, expect_freelist in (
            ("prepared-pack-deleted-page-content", b"DELETED_ROUTE_CONTENT!", True),
            ("prepared-pack-live-page-freeblock", b"DELETED!", False),
            ("prepared-pack-surplus-record-field", b"private-location-row!", False),
        ):
            with self.subTest(fixture=fixture_name):
                negative = self.fixture(fixture_name)
                negative_compressed = (PROFILE / negative["pack_file"]).read_bytes()
                negative_raw = zstandard.ZstdDecompressor().decompress(
                    negative_compressed, allow_extra_data=False
                )
                self.assertIn(marker, negative_raw)
                with tempfile.TemporaryDirectory() as directory:
                    negative_path = Path(directory) / "pack.sqlite"
                    negative_path.write_bytes(negative_raw)
                    negative_connection = sqlite3.connect(
                        f"file:{negative_path}?mode=ro&immutable=1", uri=True
                    )
                    try:
                        freelist = negative_connection.execute("PRAGMA freelist_count").fetchone()[0]
                        self.assertEqual(freelist > 0, expect_freelist)
                        self.assertEqual(
                            negative_connection.execute("PRAGMA integrity_check").fetchall(),
                            [("ok",)],
                        )
                    finally:
                        negative_connection.close()
                self.assertEqual(
                    self.sync.validate_prepared_pack(
                        negative["receipt"], negative_compressed, VEHICLE_ID
                    ),
                    ["prepared pack SQLite image is not canonical"],
                )

        self.assertEqual(
            self.sync.validate_prepared_pack(receipt, compressed + b"x", VEHICLE_ID),
            ["prepared pack compressed size does not match receipt"],
        )
        wrong_digest = bytearray(compressed)
        wrong_digest[-1] ^= 1
        self.assertEqual(
            self.sync.validate_prepared_pack(receipt, bytes(wrong_digest), VEHICLE_ID),
            ["prepared pack digest does not match receipt"],
        )

        manifest_schema = json.loads((PROFILE / "sync-manifest.schema.json").read_text())
        self.assertNotIn("prepared", json.dumps(manifest_schema))

    def test_conformance_rejects_reversed_range_nonadvancing_receipt_and_unbound_rebase(self):
        request = self.fixture("changes-since-request")["request"]
        request["schema_version_range"] = {"minimum": "2.2", "maximum": "2.1"}
        self.assertEqual(self.sync.validate_request(request), ["schema version range is reversed"])
        request = self.fixture("changes-since-request")["request"]
        request["base_manifest_schema"] = "2.2"
        request["schema_version_range"] = {"minimum": "2.1", "maximum": "2.1"}
        self.assertEqual(self.sync.validate_request(request), ["base manifest schema is outside accepted range"])
        changed = self.fixture("changes-since-changed-set")["receipt"]
        changed["to_sequence"] = changed["from_sequence"]
        self.assertEqual(self.sync.validate_response(200, changed, VEHICLE_ID), ["changed-set receipt does not advance"])
        changed = self.fixture("changes-since-changed-set")["receipt"]
        changed["manifest_schema"] = "2.2"
        self.assertEqual(self.sync.validate_response(200, changed, VEHICLE_ID), ["changed-set schema differs from base"])
        rebase = self.fixture("changes-since-rebase-after-compaction")["response"]
        rebase["retry_request"]["from_sequence"] += 1
        self.assertEqual(self.sync.validate_response(409, rebase, VEHICLE_ID), ["rebase retry does not bind replacement"])
        self.assertEqual(self.sync.validate_response(503, {}, VEHICLE_ID), ["status is not specified by profile"])
        manifest = self.fixture("schema-2-2-multi-chunk-manifest")["manifest"]
        manifest["chunks"][1]["chunk_index"] = 2
        self.assertEqual(self.sync.validate_manifest(manifest, VEHICLE_ID), ["manifest chunks are not contiguous"])
        noop = self.fixture("sync-noop-signed")["receipt"]
        noop["signature"]["signed_payload_sha256"] = "0" * 64
        self.assertEqual(self.sync.validate_noop(200, noop, VEHICLE_ID), ["signature digest is reserved"])
        self.assertEqual(self.sync.validate_noop(406, {}, VEHICLE_ID, {"Cache-Control": "private"}, b""), ["no-op unavailable must be empty no-store"])
        prepared = self.fixture("prepared-artefact-map-months")["receipt"]
        prepared["dirty_spans"]["map_months"][0]["to_ms"] = prepared["window"]["to_ms"] + 1
        self.assertEqual(self.sync.validate_prepared_artefact(prepared, VEHICLE_ID), ["prepared-artefact dirty span is outside window"])
        prepared = self.fixture("prepared-artefact-map-months")["receipt"]
        prepared["dirty_spans"] = {"map_months": []}
        self.assertEqual(self.sync.validate_prepared_artefact(prepared, VEHICLE_ID), ["prepared-artefact receipt violates schema"])
        status_tables = json.loads((PROFILE / "status-tables.json").read_text())
        status_tables["tables"][3]["responses"].pop()
        self.assertEqual(self.sync.validate_status_tables(status_tables), ["status tables are incomplete"])

    def test_changes_since_status_fixtures_and_key_discovery_route_are_explicit(self):
        expected = (
            (400, "invalid_json"),
            (404, "vehicle_not_found"),
            (406, "schema_range_unsupported"),
            (413, "request_too_large"),
            (422, "invalid_schema_range"),
            (422, "invalid_request"),
            (422, "unknown_base_receipt"),
        )
        for status, code in expected:
            with self.subTest(status=status, code=code):
                fixture = self.fixture("changes-since-error-" + code.replace("_", "-"))["response"]
                self.assertEqual(fixture["status"], status)
                self.assertEqual(fixture["body"]["code"], code)
                headers = {"Cache-Control": value for key, value in fixture.get("headers", {}).items() if key == "cache_control"}
                self.assertEqual(self.sync.validate_http_error(status, fixture["body"], headers), [])
                if status == 406:
                    self.assertEqual(self.sync.validate_http_error(status, fixture["body"], {}), ["schema range unavailable must be no-store"])

        spec = json.loads((PROFILE / "openapi.json").read_text())
        changes = spec["paths"]["/v1/vehicles/{vehicle_id}/sync/changes-since"]["post"]
        self.assertEqual(set(changes["responses"]), {"200", "400", "401", "404", "406", "409", "413", "422"})
        keys = spec["paths"]["/v1/vehicles/{vehicle_id}/sync/signing-keys"]["get"]
        self.assertEqual(keys["responses"]["200"]["content"]["application/json"]["schema"]["$ref"], "signing-keys.schema.json#/$defs/document")

        manifest = spec["paths"]["/v1/vehicles/{vehicle_id}/sync/manifest"]["get"]
        headers = {
            item["name"]: item
            for item in manifest["parameters"]
            if item["in"] == "header"
        }
        self.assertEqual(
            headers["x-teslatlas-sync-profile"]["schema"],
            {"const": "hub-sync-v1@1.3.0"},
        )
        self.assertTrue(headers["x-teslatlas-sync-profile"]["required"])
        self.assertEqual(
            headers["x-teslatlas-supported-schemas"]["schema"],
            {"const": "2.1,2.2"},
        )
        self.assertTrue(headers["x-teslatlas-supported-schemas"]["required"])
        self.assertEqual(manifest["responses"]["406"]["content"], {})
        self.assertEqual(
            manifest["responses"]["406"]["headers"]["Cache-Control"]["schema"],
            {"const": "no-store"},
        )

        at_limit = self.fixture("changes-since-request-8192-bytes")
        over_limit = self.fixture("changes-since-request-8193-bytes")
        self.assertEqual(len(at_limit["body"].encode()), 8192)
        self.assertEqual(self.sync.validate_request_body(at_limit["body"].encode()), [])
        self.assertEqual(len(over_limit["body"].encode()), 8193)
        self.assertEqual(self.sync.validate_request_body(over_limit["body"].encode()), ["request body exceeds 8192 bytes"])

        for name in ("missing-field", "extra-field", "wrong-type"):
            invalid = self.fixture("changes-since-request-invalid-" + name)["request"]
            self.assertEqual(self.sync.validate_request(invalid), ["request violates changes-since schema"])
        unsupported = self.fixture("changes-since-request-unsupported-range")["request"]
        self.assertEqual(self.sync.validate_request(unsupported), ["schema version range is unsupported"])
        reversed_range = self.fixture("changes-since-request-reversed-range")["request"]
        self.assertEqual(self.sync.validate_request(reversed_range), ["schema version range is reversed"])
        base_excluded = self.fixture("changes-since-request-base-excluded")["request"]
        self.assertEqual(self.sync.validate_request(base_excluded), ["base manifest schema is outside accepted range"])
        unknown_base = self.fixture("changes-since-request-unknown-base-receipt")["request"]
        self.assertEqual(self.sync.validate_request(unknown_base), [])

    def test_signed_responses_and_key_selection_match_route_vehicle(self):
        changed = self.fixture("changes-since-changed-set")["receipt"]
        rebase = self.fixture("changes-since-rebase-after-compaction")["response"]
        manifest = self.fixture("schema-2-2-multi-chunk-manifest")["manifest"]
        noop = self.fixture("sync-noop-signed")["receipt"]
        prepared = self.fixture("prepared-artefact-map-months")["receipt"]
        for validate, value in (
            (lambda item: self.sync.validate_response(200, item, OTHER_VEHICLE_ID), changed),
            (lambda item: self.sync.validate_response(409, item, OTHER_VEHICLE_ID), rebase),
            (lambda item: self.sync.validate_manifest(item, OTHER_VEHICLE_ID), manifest),
            (lambda item: self.sync.validate_noop(200, item, OTHER_VEHICLE_ID), noop),
            (lambda item: self.sync.validate_prepared_artefact(item, OTHER_VEHICLE_ID), prepared),
        ):
            with self.subTest(kind=value.get("kind", value.get("artifact_type", value.get("schema_version")))):
                self.assertEqual(validate(value), ["signed response vehicle does not match route"])

        cross_vehicle = self.fixture("changes-since-changed-set-cross-vehicle")["receipt"]
        self.assertEqual(self.sync.verify_signature(cross_vehicle, self.sync.load_signing_keys()), [])
        self.assertEqual(self.sync.validate_response(200, cross_vehicle, VEHICLE_ID), ["signed response vehicle does not match route"])
        self.assertEqual(self.sync.validate_response(200, cross_vehicle, OTHER_VEHICLE_ID), ["signing keys are bound to another vehicle"])

        key_fixture = self.fixture("signing-keys-vehicle-bound")["response"]
        headers = {"Cache-Control": key_fixture["headers"]["cache_control"]}
        self.assertEqual(self.sync.validate_signing_keys_response(key_fixture["status"], key_fixture["body"], VEHICLE_ID, headers), [])
        self.assertEqual(self.sync.validate_signing_keys_response(key_fixture["status"], key_fixture["body"], VEHICLE_ID, {}), ["signing keys response must be no-store"])

    def test_every_json_control_response_enforces_exact_raw_body_boundary(self):
        response_types = (
            ("sync-manifest", "manifest"),
            ("changes-since-changed-set", "changed_set"),
            ("sync-noop", "noop"),
            ("changes-since-rebase", "rebase"),
            ("changes-since-error", "error"),
            ("signing-keys", "signing_keys"),
            ("prepared-artefact", "prepared_artefact"),
        )
        for prefix, response_type in response_types:
            with self.subTest(response_type=response_type):
                at_limit = self.fixture(prefix + "-response-2097152-bytes")
                over_limit = self.fixture(prefix + "-response-2097153-bytes")
                headers = {
                    "Cache-Control": value
                    for key, value in at_limit.get("headers", {}).items()
                    if key == "cache_control"
                }
                raw = self.sync.materialize_body_fixture(at_limit)
                self.assertEqual(len(raw), 2 * 1024 * 1024)
                self.assertEqual(hashlib.sha256(raw).hexdigest(), at_limit["body_sha256"])
                self.assertEqual(
                    self.sync.validate_control_response_body(
                        raw,
                        response_type,
                        at_limit["status"],
                        at_limit["vehicle_id"],
                        headers,
                    ),
                    [],
                )
                raw = self.sync.materialize_body_fixture(over_limit)
                self.assertEqual(len(raw), 2 * 1024 * 1024 + 1)
                self.assertEqual(hashlib.sha256(raw).hexdigest(), over_limit["body_sha256"])
                self.assertEqual(
                    self.sync.validate_control_response_body(
                        raw,
                        response_type,
                        over_limit["status"],
                        over_limit["vehicle_id"],
                        headers,
                    ),
                    ["response body exceeds 2097152 bytes"],
                )

    def test_schema_maxima_keep_compact_control_responses_within_two_mib(self):
        opaque = "a" * 4096
        pack = {"object_name": "a" * 1024, "sha256": "a" * 64, "compressed_bytes": 16 * 1024 * 1024}
        signature = {
            "algorithm": "ed25519",
            "key_id": "a" * 128,
            "signed_payload_sha256": "a" * 64,
            "signature": "A" * 86 + "==",
        }

        def compact_size(value):
            return len(json.dumps(value, sort_keys=True, separators=(",", ":")).encode())

        def validator(name, definition):
            schema = json.loads((PROFILE / name).read_text())
            return Draft202012Validator({**schema, "$ref": "#/$defs/" + definition}, format_checker=FormatChecker())

        chunks = [{"chunk_index": index, "pack": pack} for index in range(1771)]
        manifest = {
            "manifest_id": opaque,
            "receipt_id": opaque,
            "vehicle_id": VEHICLE_ID,
            "kind": "snapshot",
            "schema_version": "2.2",
            "sequence": I_JSON_MAX_INTEGER,
            "chunks": chunks,
            "signature": signature,
        }
        manifest_validator = validator("sync-manifest.schema.json", "manifest")
        self.assertTrue(manifest_validator.is_valid(manifest))
        self.assertLessEqual(compact_size(manifest), 2 * 1024 * 1024)

        rebase = {
            "kind": "rebase_required",
            "vehicle_id": VEHICLE_ID,
            "requested_base_receipt_id": opaque,
            "requested_base_manifest_schema": "2.2",
            "requested_from_sequence": I_JSON_MAX_INTEGER,
            "reason": "compacted",
            "replacement": {
                "manifest_id": opaque,
                "receipt_id": opaque,
                "sequence": I_JSON_MAX_INTEGER,
                "manifest_schema": "2.2",
                "chunks": chunks,
            },
            "retry_request": {
                "base_receipt_id": opaque,
                "base_manifest_schema": "2.2",
                "from_sequence": I_JSON_MAX_INTEGER,
                "schema_version_range": {"minimum": "0.0", "maximum": "999.999"},
            },
            "signature": signature,
        }
        rebase_validator = validator("rebase-hint.schema.json", "hint")
        self.assertTrue(rebase_validator.is_valid(rebase))
        self.assertLessEqual(compact_size(rebase), 2 * 1024 * 1024)
        rebase["replacement"]["chunks"] = chunks + [{"chunk_index": 1771, "pack": pack}]
        self.assertFalse(rebase_validator.is_valid(rebase))
        self.assertGreater(compact_size(rebase), 2 * 1024 * 1024)

        months = [
            {
                "month": f"{2000 + index // 12:04d}-{index % 12 + 1:02d}",
                "from_ms": 0,
                "to_ms": I_JSON_MAX_INTEGER,
                "resolution": "readyEmpty",
                "drive_count": 0,
                "tile_count": 0,
                "reason": "changed",
            }
            for index in range(121)
        ]
        prepared = {
            "artifact_id": opaque,
            "artifact_type": "map_months",
            "payload": "teslatlas-prepared-v1",
            "scope": "map_months",
            "map_style": "route-stroke-v8-opaque",
            "tile_geometry_version": "raster-v9-rounded-tile-px",
            "artifact_schema_version": 1,
            "vehicle_id": VEHICLE_ID,
            "source": {
                "kind": "hub_compute",
                "input_manifest_id": opaque,
                "input_receipt_id": opaque,
                "input_manifest_schema": "2.2",
                "input_sequence": I_JSON_MAX_INTEGER,
            },
            "window": {"from_ms": 0, "to_ms": I_JSON_MAX_INTEGER},
            "generation": {"generation_id": opaque, "generated_at_ms": I_JSON_MAX_INTEGER},
            "units": {"distance": "km", "time": "ms", "coordinates": "wgs84_degrees"},
            "algorithm_version": "1.1.1+" + "a" * 122,
            "dirty_spans": {"map_months": months[:120]},
            "pack": {
                **pack,
                "payload": "teslatlas-prepared-v1",
                "media_type": "application/vnd.teslatlas.prepared+sqlite+zstd;version=1",
                "uncompressed_bytes": 64 * 1024 * 1024,
            },
            "signature": signature,
        }
        prepared_validator = validator("prepared-artefact.schema.json", "receipt")
        self.assertTrue(prepared_validator.is_valid(prepared))
        self.assertLessEqual(compact_size(prepared), 2 * 1024 * 1024)
        prepared["dirty_spans"]["map_months"] = months
        self.assertFalse(prepared_validator.is_valid(prepared))

    def test_fixture_signatures_reject_tampering_and_wrong_keys(self):
        changed = self.fixture("changes-since-changed-set")["receipt"]
        keys = self.sync.load_signing_keys()
        self.assertEqual(self.sync.verify_signature(changed, keys), [])

        tampered = copy.deepcopy(changed)
        tampered["to_sequence"] += 1
        self.assertEqual(self.sync.verify_signature(tampered, keys), ["signature payload digest mismatch"])

        unknown = copy.deepcopy(changed)
        unknown["signature"]["key_id"] = "fixture-ed25519-unknown"
        self.assertEqual(self.sync.verify_signature(unknown, keys), ["signature key is unknown"])

        wrong_key = copy.deepcopy(keys)
        wrong_key["keys"][0]["public_key"] = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        self.assertEqual(self.sync.verify_signature(changed, wrong_key), ["signature verification failed"])

        wrong_vehicle = copy.deepcopy(keys)
        wrong_vehicle["vehicle_id"] = "22222222-2222-4222-8222-222222222222"
        self.assertEqual(
            self.sync.validate_signing_keys(wrong_vehicle, "11111111-1111-4111-8111-111111111111"),
            ["signing keys are bound to another vehicle"],
        )
        unstable_id = copy.deepcopy(keys)
        unstable_id["keys"][0]["key_id"] = "ed25519-sha256-" + "f" * 64
        self.assertEqual(
            self.sync.validate_signing_keys(unstable_id, unstable_id["vehicle_id"]),
            ["signing key identifier is not stable"],
        )

    def test_signed_response_shapes_reject_a_known_key_corrupted_signature(self):
        changed = self.fixture("changes-since-changed-set")["receipt"]
        corrupted_fixture = self.fixture("changes-since-changed-set-invalid-signature")
        corrupted = corrupted_fixture["receipt"]
        self.assertEqual(
            self.sync.canonical_fixture_payload(changed),
            self.sync.canonical_fixture_payload(corrupted),
        )
        self.assertEqual(
            changed["signature"]["signed_payload_sha256"],
            corrupted["signature"]["signed_payload_sha256"],
        )
        self.assertEqual(changed["signature"]["key_id"], corrupted["signature"]["key_id"])
        self.assertEqual(self.sync.validate_response(200, corrupted, VEHICLE_ID), ["signature verification failed"])

        fixtures = (
            (self.fixture("changes-since-rebase-after-compaction")["response"], lambda value: self.sync.validate_response(409, value, VEHICLE_ID)),
            (self.fixture("schema-2-1-single-pack-manifest")["manifest"], lambda value: self.sync.validate_manifest(value, VEHICLE_ID)),
            (self.fixture("schema-2-2-multi-chunk-manifest")["manifest"], lambda value: self.sync.validate_manifest(value, VEHICLE_ID)),
            (self.fixture("sync-noop-signed")["receipt"], lambda value: self.sync.validate_noop(200, value, VEHICLE_ID)),
            (self.fixture("prepared-artefact-map-months")["receipt"], lambda value: self.sync.validate_prepared_artefact(value, VEHICLE_ID)),
        )
        for value, validate in fixtures:
            with self.subTest(fixture_id=value.get("fixture_id", "embedded")):
                value["signature"]["signature"] = "A" * 86 + "=="
                self.assertEqual(validate(value), ["signature verification failed"])

    def test_registered_fixture_cases_cover_positive_and_negative_vectors(self):
        results = self.sync.run_fixture_cases()
        self.assertEqual(len(results), 65)
        self.assertTrue(all(result["passed"] for result in results))
        by_case = {result["case_id"]: result for result in results}
        self.assertEqual(
            by_case["changes-since-changed-set-tampered"]["fixture_ids"],
            ["changes-since-changed-set-tampered-v1"],
        )
        self.assertEqual(
            by_case["changes-since-changed-set-unknown-key"]["expected_errors"],
            ["signature key is unknown"],
        )
        self.assertEqual(
            by_case["changes-since-changed-set-invalid-signature"]["fixture_ids"],
            ["changes-since-changed-set-invalid-signature-v1"],
        )
        self.assertEqual(
            by_case["changes-since-error-unknown-base-receipt"]["fixture_ids"],
            ["changes-since-request-unknown-base-receipt-v1", "changes-since-error-unknown-base-receipt-v1"],
        )
        self.assertEqual(
            by_case["changes-since-changed-set-cross-schema"]["expected_errors"],
            ["changed-set schema differs from base"],
        )
        self.assertEqual(
            by_case["changes-since-changed-set-cross-vehicle"]["expected_errors"],
            ["signed response vehicle does not match route"],
        )
        self.assertEqual(
            by_case["bootstrap-schema-list-only-keeps-legacy-shape"]["fixture_ids"],
            ["bootstrap-schema-list-only-legacy-v1"],
        )
        self.assertEqual(
            by_case["schema-2-1-manifest-unsafe-integer"]["fixture_ids"],
            ["schema-2-1-manifest-unsafe-integer-v1"],
        )
        self.assertEqual(
            self.fixture("schema-2-1-manifest-unsafe-integer")["manifest"]["sequence"],
            2**53 + 1,
        )
        self.assertEqual(
            by_case["prepared-pack-route-content-unsupported"]["expected_errors"],
            ["prepared pack contains unsupported route content"],
        )
        self.assertEqual(
            by_case["prepared-pack-unsupported-payload-version"]["expected_errors"],
            ["prepared pack payload version is unsupported"],
        )
        self.assertEqual(by_case["prepared-pack-ready-empty"]["expected_errors"], [])
        self.assertEqual(
            by_case["prepared-pack-weakened-schema"]["expected_errors"],
            ["prepared pack SQLite schema is unsupported"],
        )
        self.assertEqual(
            by_case["prepared-pack-hidden-column"]["expected_errors"],
            ["prepared pack SQLite schema is unsupported"],
        )
        self.assertEqual(
            by_case["prepared-pack-deleted-page-content"]["expected_errors"],
            ["prepared pack SQLite image is not canonical"],
        )
        self.assertEqual(
            by_case["prepared-pack-live-page-freeblock"]["expected_errors"],
            ["prepared pack SQLite image is not canonical"],
        )
        self.assertEqual(
            by_case["prepared-pack-surplus-record-field"]["expected_errors"],
            ["prepared pack SQLite image is not canonical"],
        )
        self.assertEqual(
            by_case["prepared-pack-source-lineage-mismatch"]["expected_errors"],
            ["prepared pack metadata does not match receipt"],
        )

    def test_bootstrap_profile_selector_preserves_legacy_schema_negotiation(self):
        selected = self.fixture("bootstrap-hub-sync-v1-1-3-selected")
        legacy = self.fixture("bootstrap-schema-list-only-legacy")
        self.assertEqual(self.sync.validate_bootstrap_selection(selected, 200), [])
        self.assertEqual(self.sync.validate_bootstrap_selection(legacy, 200), [])
        self.assertEqual(
            legacy["request"]["headers"],
            [{"name": "x-teslatlas-supported-schemas", "value": "2.1,2.2"}],
        )
        self.assertEqual(legacy["expected_response"], {"status": 200, "representation": "legacy-sync-manifest"})

        rejected = (
            "bootstrap-profile-selector-invalid",
            "bootstrap-profile-selector-missing-schema",
            "bootstrap-profile-selector-invalid-schema",
            "bootstrap-profile-selector-duplicate",
            "bootstrap-profile-selector-duplicate-schema",
        )
        for name in rejected:
            with self.subTest(name=name):
                fixture = self.fixture(name)
                self.assertEqual(self.sync.validate_bootstrap_selection(fixture, 406), [])
                self.assertEqual(
                    fixture["expected_response"],
                    {
                        "status": 406,
                        "representation": "empty",
                        "headers": {"cache_control": "no-store"},
                        "body_bytes": 0,
                        "manifest_signature": "absent",
                    },
                )
        duplicate = self.fixture("bootstrap-profile-selector-duplicate")
        self.assertEqual(
            [item["name"] for item in duplicate["request"]["headers"]].count("x-teslatlas-sync-profile"),
            2,
        )
        duplicate_schema = self.fixture("bootstrap-profile-selector-duplicate-schema")
        self.assertEqual(
            [item["name"] for item in duplicate_schema["request"]["headers"]].count("x-teslatlas-supported-schemas"),
            2,
        )

        malformed = copy.deepcopy(selected)
        malformed["request"]["headers"] = {"x-teslatlas-sync-profile": "hub-sync-v1@1.3.0"}
        self.assertEqual(
            self.sync.validate_bootstrap_selection(malformed, 200),
            ["bootstrap fixture headers are invalid"],
        )


if __name__ == "__main__":
    unittest.main()
