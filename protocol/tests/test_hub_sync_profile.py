import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

from jsonschema import Draft202012Validator
from openapi_spec_validator import validate as validate_openapi

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.0.0"


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
        self.assertEqual(profile["profile_id"], "hub-sync-v1@1.0.0")
        self.assertEqual(profile["product_version_binding"], "none; this protocol profile has no exact Hub product-version pin")
        self.assertEqual(profile["authentication"], "paired bearer")
        self.assertEqual(profile["limits"]["max_changed_set_packs"], 1)

    def test_schemas_and_openapi_are_independently_valid(self):
        for name in ("changes-since-request.schema.json", "changed-set-receipt.schema.json", "rebase-hint.schema.json"):
            Draft202012Validator.check_schema(json.loads((PROFILE / name).read_text()))
        validate_openapi(json.loads((PROFILE / "openapi.json").read_text()), base_uri=PROFILE.as_uri() + "/")

    def test_changed_set_and_compaction_fixtures_are_deterministic(self):
        request = self.fixture("changes-since-request")["request"]
        changed = self.fixture("changes-since-changed-set")["receipt"]
        rebase = self.fixture("changes-since-rebase-after-compaction")["response"]
        self.assertEqual(self.fixture("changes-since-request")["fixture_id"], "changes-since-request-v1")
        self.assertEqual(self.fixture("changes-since-changed-set")["fixture_id"], "changes-since-changed-set-v1")
        self.assertEqual(self.fixture("changes-since-rebase-after-compaction")["fixture_id"], "changes-since-rebase-after-compaction-v1")
        self.assertEqual(self.sync.validate_request(request), [])
        self.assertEqual(self.sync.validate_response(200, changed), [])
        self.assertEqual(self.sync.validate_response(409, rebase), [])
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


if __name__ == "__main__":
    unittest.main()
