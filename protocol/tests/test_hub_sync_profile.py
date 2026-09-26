import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

from jsonschema import Draft202012Validator
from openapi_spec_validator import validate as validate_openapi

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.3.0"


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
        self.assertEqual(profile["limits"]["max_manifest_chunks"], 4096)
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
        self.assertEqual(self.fixture("changes-since-request")["fixture_id"], "changes-since-request-v1")
        self.assertEqual(self.fixture("changes-since-changed-set")["fixture_id"], "changes-since-changed-set-v1")
        self.assertEqual(self.fixture("changes-since-rebase-after-compaction")["fixture_id"], "changes-since-rebase-after-compaction-v1")
        self.assertEqual(self.sync.validate_request(request), [])
        self.assertEqual(self.sync.validate_response(200, changed), [])
        self.assertEqual(self.sync.validate_response(409, rebase), [])
        self.assertEqual(self.fixture("schema-2-1-single-pack-manifest")["fixture_id"], "schema-2-1-single-pack-manifest-v1")
        self.assertEqual(self.fixture("schema-2-2-multi-chunk-manifest")["fixture_id"], "schema-2-2-multi-chunk-manifest-v1")
        self.assertEqual(self.sync.validate_manifest(manifest_2_1), [])
        self.assertEqual(self.sync.validate_manifest(manifest), [])
        self.assertEqual(self.sync.validate_manifest(small_manifest), [])
        self.assertEqual(manifest["chunks"][-1]["pack"]["compressed_bytes"], 4096)
        self.assertEqual(small_manifest["chunks"][0]["pack"]["compressed_bytes"], 4096)
        self.assertEqual(manifest_2_1["receipt_id"], request["base_receipt_id"])
        self.assertEqual(manifest_2_1["schema_version"], request["base_manifest_schema"])
        self.assertEqual(self.sync.validate_response(200, no_change), [])
        self.assertEqual(self.sync.validate_noop(200, noop), [])
        self.assertEqual(self.fixture("prepared-artefact-map-months-routes")["fixture_id"], "prepared-artefact-map-months-routes-v1")
        self.assertEqual(self.sync.validate_prepared_artefact(prepared), [])
        self.assertEqual(self.sync.validate_noop(unavailable["status"], {}, {"Cache-Control": unavailable["headers"]["cache_control"]}, b""), [])
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
        self.assertEqual(self.sync.validate_response(200, changed), ["changed-set receipt does not advance"])
        changed = self.fixture("changes-since-changed-set")["receipt"]
        changed["manifest_schema"] = "2.2"
        self.assertEqual(self.sync.validate_response(200, changed), ["changed-set schema differs from base"])
        rebase = self.fixture("changes-since-rebase-after-compaction")["response"]
        rebase["retry_request"]["from_sequence"] += 1
        self.assertEqual(self.sync.validate_response(409, rebase), ["rebase retry does not bind replacement"])
        self.assertEqual(self.sync.validate_response(503, {}), ["status is not specified by profile"])
        manifest = self.fixture("schema-2-2-multi-chunk-manifest")["manifest"]
        manifest["chunks"][1]["chunk_index"] = 2
        self.assertEqual(self.sync.validate_manifest(manifest), ["manifest chunks are not contiguous"])
        noop = self.fixture("sync-noop-signed")["receipt"]
        noop["signature"]["signed_payload_sha256"] = "0" * 64
        self.assertEqual(self.sync.validate_noop(200, noop), ["signature digest is reserved"])
        self.assertEqual(self.sync.validate_noop(406, {}, {"Cache-Control": "private"}, b""), ["no-op unavailable must be empty no-store"])
        prepared = self.fixture("prepared-artefact-map-months-routes")["receipt"]
        prepared["dirty_spans"]["routes"][0]["to_ms"] = prepared["window"]["to_ms"] + 1
        self.assertEqual(self.sync.validate_prepared_artefact(prepared), ["prepared-artefact dirty span is outside window"])
        prepared = self.fixture("prepared-artefact-map-months-routes")["receipt"]
        prepared["dirty_spans"] = {"map_months": [], "routes": []}
        self.assertEqual(self.sync.validate_prepared_artefact(prepared), ["prepared-artefact receipt violates schema"])
        status_tables = json.loads((PROFILE / "status-tables.json").read_text())
        status_tables["tables"][3]["responses"].pop()
        self.assertEqual(self.sync.validate_status_tables(status_tables), ["status tables are incomplete"])

    def test_changes_since_status_fixtures_and_key_discovery_route_are_explicit(self):
        expected = {
            400: "invalid_json",
            404: "vehicle_not_found",
            406: "schema_range_unsupported",
            413: "request_too_large",
            422: "invalid_schema_range",
        }
        for status, code in expected.items():
            with self.subTest(status=status):
                fixture = self.fixture("changes-since-error-" + code.replace("_", "-"))["response"]
                self.assertEqual(fixture["status"], status)
                self.assertEqual(fixture["body"]["code"], code)
                self.assertEqual(self.sync.validate_http_error(status, fixture["body"]), [])

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
        self.assertEqual(self.sync.validate_response(200, corrupted), ["signature verification failed"])

        fixtures = (
            (self.fixture("changes-since-rebase-after-compaction")["response"], lambda value: self.sync.validate_response(409, value)),
            (self.fixture("schema-2-1-single-pack-manifest")["manifest"], self.sync.validate_manifest),
            (self.fixture("schema-2-2-multi-chunk-manifest")["manifest"], self.sync.validate_manifest),
            (self.fixture("sync-noop-signed")["receipt"], lambda value: self.sync.validate_noop(200, value)),
            (self.fixture("prepared-artefact-map-months-routes")["receipt"], self.sync.validate_prepared_artefact),
        )
        for value, validate in fixtures:
            with self.subTest(fixture_id=value.get("fixture_id", "embedded")):
                value["signature"]["signature"] = "A" * 86 + "=="
                self.assertEqual(validate(value), ["signature verification failed"])

    def test_registered_fixture_cases_cover_positive_and_negative_vectors(self):
        results = self.sync.run_fixture_cases()
        self.assertEqual(len(results), 21)
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


if __name__ == "__main__":
    unittest.main()
