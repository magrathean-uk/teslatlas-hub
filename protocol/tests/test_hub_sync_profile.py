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
        frozen = ROOT / "profiles/hub-sync-v1/1.2.0/SHA256SUMS"
        self.assertEqual(hashlib.sha256(frozen.read_bytes()).hexdigest(), "331920326476ffe8d3e382181a73937d478c613793c3cc5c0a7e2db14e091ed5")

    def test_schemas_and_openapi_are_independently_valid(self):
        for name in ("changes-since-request.schema.json", "changed-set-receipt.schema.json", "rebase-hint.schema.json", "sync-manifest.schema.json", "noop.schema.json", "prepared-artefact.schema.json", "signing-keys.schema.json", "status-tables.schema.json"):
            Draft202012Validator.check_schema(json.loads((PROFILE / name).read_text()))
        validate_openapi(json.loads((PROFILE / "openapi.json").read_text()), base_uri=PROFILE.as_uri() + "/")

    def test_changed_set_and_compaction_fixtures_are_deterministic(self):
        request = self.fixture("changes-since-request")["request"]
        changed = self.fixture("changes-since-changed-set")["receipt"]
        rebase = self.fixture("changes-since-rebase-after-compaction")["response"]
        manifest_2_1 = self.fixture("schema-2-1-single-pack-manifest")["manifest"]
        manifest = self.fixture("schema-2-2-multi-chunk-manifest")["manifest"]
        noop = self.fixture("sync-noop-signed")["receipt"]
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
        self.assertEqual(self.sync.validate_noop(200, noop), [])
        self.assertEqual(self.fixture("prepared-artefact-map-months-routes")["fixture_id"], "prepared-artefact-map-months-routes-v1")
        self.assertEqual(self.sync.validate_prepared_artefact(prepared), [])
        self.assertEqual(self.sync.validate_noop(unavailable["status"], {}, {"Cache-Control": unavailable["headers"]["cache_control"]}, b""), [])
        self.assertEqual(self.sync.validate_status_tables(status_tables), [])
        self.assertEqual(rebase["reason"], "compacted")
        self.assertEqual(rebase["retry_request"]["base_receipt_id"], rebase["replacement"]["receipt_id"])
        self.assertEqual(rebase["retry_request"]["from_sequence"], rebase["replacement"]["sequence"])

    def test_conformance_rejects_reversed_range_nonadvancing_receipt_and_unbound_rebase(self):
        request = self.fixture("changes-since-request")["request"]
        request["schema_version_range"] = {"minimum": "2.2", "maximum": "2.1"}
        self.assertEqual(self.sync.validate_request(request), ["schema version range is reversed"])
        changed = self.fixture("changes-since-changed-set")["receipt"]
        changed["to_sequence"] = changed["from_sequence"]
        self.assertEqual(self.sync.validate_response(200, changed), ["changed-set receipt does not advance"])
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

    def test_registered_fixture_cases_cover_positive_and_negative_vectors(self):
        results = self.sync.run_fixture_cases()
        self.assertEqual(len(results), 11)
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


if __name__ == "__main__":
    unittest.main()
