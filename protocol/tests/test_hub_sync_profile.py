import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

from jsonschema import Draft202012Validator, FormatChecker
from openapi_spec_validator import validate as validate_openapi

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.3.0"
VEHICLE_ID = "11111111-1111-4111-8111-111111111111"
OTHER_VEHICLE_ID = "22222222-2222-4222-8222-222222222222"


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
        self.assertEqual(profile["limits"]["max_prepared_routes"], 497)
        self.assertEqual(profile["limits"]["max_request_bytes"], 8192)
        self.assertEqual(profile["limits"]["min_pack_compressed_bytes"], 1)
        self.assertEqual(profile["limits"]["target_pack_compressed_bytes"], 8 * 1024 * 1024)
        frozen = ROOT / "profiles/hub-sync-v1/1.2.0/SHA256SUMS"
        self.assertEqual(hashlib.sha256(frozen.read_bytes()).hexdigest(), "331920326476ffe8d3e382181a73937d478c613793c3cc5c0a7e2db14e091ed5")

    def test_schemas_and_openapi_are_independently_valid(self):
        for name in ("changes-since-request.schema.json", "changed-set-receipt.schema.json", "rebase-hint.schema.json", "sync-manifest.schema.json", "noop.schema.json", "prepared-artefact.schema.json", "signing-keys.schema.json", "sync-error.schema.json", "status-tables.schema.json"):
            Draft202012Validator.check_schema(json.loads((PROFILE / name).read_text()))
        validate_openapi(json.loads((PROFILE / "openapi.json").read_text()), base_uri=PROFILE.as_uri() + "/")

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
        prepared = self.fixture("prepared-artefact-map-months-routes")["receipt"]
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
        self.assertEqual(self.fixture("prepared-artefact-map-months-routes")["fixture_id"], "prepared-artefact-map-months-routes-v1")
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
        prepared = self.fixture("prepared-artefact-map-months-routes")["receipt"]
        prepared["dirty_spans"]["routes"][0]["to_ms"] = prepared["window"]["to_ms"] + 1
        self.assertEqual(self.sync.validate_prepared_artefact(prepared, VEHICLE_ID), ["prepared-artefact dirty span is outside window"])
        prepared = self.fixture("prepared-artefact-map-months-routes")["receipt"]
        prepared["dirty_spans"] = {"map_months": [], "routes": []}
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

    def test_signed_responses_and_key_selection_match_route_vehicle(self):
        changed = self.fixture("changes-since-changed-set")["receipt"]
        rebase = self.fixture("changes-since-rebase-after-compaction")["response"]
        manifest = self.fixture("schema-2-2-multi-chunk-manifest")["manifest"]
        noop = self.fixture("sync-noop-signed")["receipt"]
        prepared = self.fixture("prepared-artefact-map-months-routes")["receipt"]
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
            "sequence": 2**63 - 1,
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
            "requested_from_sequence": 2**63 - 1,
            "reason": "compacted",
            "replacement": {
                "manifest_id": opaque,
                "receipt_id": opaque,
                "sequence": 2**63 - 1,
                "manifest_schema": "2.2",
                "chunks": chunks,
            },
            "retry_request": {
                "base_receipt_id": opaque,
                "base_manifest_schema": "2.2",
                "from_sequence": 2**63 - 1,
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

        routes = [
            {"route_id": "a" * (4096 - len(str(index))) + str(index), "from_ms": 0, "to_ms": 2**63 - 1, "reason": "changed"}
            for index in range(498)
        ]
        months = [
            {"month": f"{2000 + index // 12:04d}-{index % 12 + 1:02d}", "from_ms": 0, "to_ms": 2**63 - 1, "reason": "changed"}
            for index in range(120)
        ]
        prepared = {
            "artifact_id": opaque,
            "artifact_type": "map_months_and_routes",
            "vehicle_id": VEHICLE_ID,
            "source": {"kind": "hub_compute", "input_manifest_id": opaque, "input_sequence": 2**63 - 1},
            "window": {"from_ms": 0, "to_ms": 2**63 - 1},
            "generation": {"generation_id": opaque, "generated_at_ms": 2**63 - 1},
            "units": {"distance": "km", "time": "ms", "coordinates": "wgs84_degrees"},
            "algorithm_version": "1.1.1+" + "a" * 122,
            "dirty_spans": {"map_months": months, "routes": routes[:497]},
            "pack": pack,
            "signature": signature,
        }
        prepared_validator = validator("prepared-artefact.schema.json", "receipt")
        self.assertTrue(prepared_validator.is_valid(prepared))
        self.assertLessEqual(compact_size(prepared), 2 * 1024 * 1024)
        prepared["dirty_spans"]["routes"] = routes
        self.assertFalse(prepared_validator.is_valid(prepared))
        self.assertGreater(compact_size(prepared), 2 * 1024 * 1024)

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
            (self.fixture("prepared-artefact-map-months-routes")["receipt"], lambda value: self.sync.validate_prepared_artefact(value, VEHICLE_ID)),
        )
        for value, validate in fixtures:
            with self.subTest(fixture_id=value.get("fixture_id", "embedded")):
                value["signature"]["signature"] = "A" * 86 + "=="
                self.assertEqual(validate(value), ["signature verification failed"])

    def test_registered_fixture_cases_cover_positive_and_negative_vectors(self):
        results = self.sync.run_fixture_cases()
        self.assertEqual(len(results), 41)
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
            by_case["changes-since-changed-set-cross-schema"]["expected_errors"],
            ["changed-set schema differs from base"],
        )
        self.assertEqual(
            by_case["changes-since-changed-set-cross-vehicle"]["expected_errors"],
            ["signed response vehicle does not match route"],
        )


if __name__ == "__main__":
    unittest.main()
