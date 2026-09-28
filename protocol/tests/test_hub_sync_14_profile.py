import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

from jsonschema import Draft202012Validator
from openapi_spec_validator import validate as validate_openapi

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.4.0"


class PhysicalChangedSetProfileTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("hub_sync_14", ROOT / "conformance/hub_sync_14.py")
        cls.contract = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.contract)
        cls.request = json.loads((PROFILE / "examples/changes-since-request-physical.json").read_text())["request"]
        cls.receipt = json.loads((PROFILE / "examples/changes-since-physical-changed-set.json").read_text())["receipt"]
        cls.pack = (PROFILE / "packs/physical-changed-set-v1.sqlite.zst").read_bytes()

    def test_generated_bundle_and_machine_schemas(self):
        check = subprocess.run([sys.executable, str(ROOT / "tools/build_hub_sync_14_profile.py"), "--check"], capture_output=True)
        self.assertEqual(check.returncode, 0, check.stderr.decode())
        self.assertEqual(self.contract.verify_bundle()["profile_id"], "hub-sync-v1@1.4.0")
        for name in ("changes-since-request.schema.json", "physical-changed-set-receipt.schema.json", "physical-rebase-hint.schema.json"):
            Draft202012Validator.check_schema(json.loads((PROFILE / name).read_text()))
        validate_openapi(json.loads((PROFILE / "openapi.json").read_text()), base_uri=PROFILE.as_uri() + "/")

    def test_positive_signed_changed_set_and_complete_pack(self):
        self.contract.verify_receipt(self.request, self.receipt, self.receipt["vehicle_id"])
        self.assertTrue(self.contract.verify_signed_manifest_bindings(
            self.receipt, (PROFILE / "examples/signed-full-manifest-42.json").read_bytes(),
            (PROFILE / "examples/signed-full-manifest-43.json").read_bytes()))
        self.assertTrue(self.contract.verify_pack(self.receipt, self.pack))
        catalog = json.loads((PROFILE / "physical-field-catalog.json").read_text())
        self.assertEqual(catalog["mapped_source_fields"], 168)
        self.assertEqual(catalog["wire_columns"], 205)
        self.assertEqual(len(catalog["tables"]), 11)
        self.assertEqual(catalog["excluded_source_fields"], ["addresses.raw"])

    def test_unadvertised_capability_and_wrong_base_fail_closed(self):
        unadvertised = dict(self.request)
        unadvertised.pop("accepted_changed_set_formats")
        with self.assertRaisesRegex(self.contract.ContractError, "request violates"):
            self.contract.verify_receipt(unadvertised, self.receipt, self.receipt["vehicle_id"])
        other_base = dict(self.request, base_receipt_id="receipt_other")
        with self.assertRaisesRegex(self.contract.ContractError, "base checkpoint mismatch"):
            self.contract.verify_receipt(other_base, self.receipt, self.receipt["vehicle_id"])

    def test_tampering_and_pack_hash_fail_closed(self):
        tampered = json.loads(json.dumps(self.receipt))
        tampered["target"]["sequence"] += 1
        with self.assertRaises(self.contract.ContractError):
            self.contract.verify_receipt(self.request, tampered, self.receipt["vehicle_id"])
        with self.assertRaisesRegex(self.contract.ContractError, "pack digest mismatch"):
            self.contract.verify_pack(self.receipt, self.pack + b"x")

    def test_negative_signed_fixtures_bindings_closure_and_bounds(self):
        for label, expected in (("missing-closure", "incomplete impacted root"),
                                ("wrong-binding", "physical pack binding mismatch"),
                                ("wrong-open-state", "impacted root open/closed state mismatch")):
            with self.subTest(label=label):
                receipt = json.loads((PROFILE / f"examples/changes-since-physical-{label}.json").read_text())["receipt"]
                pack_name = receipt["chunks"][0]["pack"]["object_name"]
                self.contract.verify_receipt(self.request, receipt, receipt["vehicle_id"])
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_pack(receipt, (PROFILE / pack_name).read_bytes())
        wrong_source = json.loads((PROFILE / "examples/changes-since-physical-wrong-source.json").read_text())["receipt"]
        with self.assertRaisesRegex(self.contract.ContractError, "source vehicle mismatch"):
            self.contract.verify_receipt(self.request, wrong_source, self.receipt["vehicle_id"])
        wrong_generation = json.loads((PROFILE / "examples/changes-since-physical-wrong-generation.json").read_text())["receipt"]
        with self.assertRaisesRegex(self.contract.ContractError, "source binding mismatch"):
            self.contract.verify_receipt(self.request, wrong_generation, self.receipt["vehicle_id"])
        over_limit = json.loads((PROFILE / "examples/changes-since-physical-over-limit.json").read_text())["receipt"]
        with self.assertRaisesRegex(self.contract.ContractError, "receipt violates 1.4 schema"):
            self.contract.verify_receipt(self.request, over_limit, self.receipt["vehicle_id"])

    def test_previous_profile_noop_is_still_admitted(self):
        prior = ROOT / "profiles/hub-sync-v1/1.3.0"
        noop = json.loads((prior / "examples/sync-noop-signed.json").read_text())
        self.assertEqual(noop["receipt"]["kind"], "no_op")
        self.assertEqual(noop["receipt"]["manifest_schema"], "2.2")
        self.assertEqual(self.contract.verify_bundle()["previous_profile"], "hub-sync-v1@1.3.0")

    def test_signed_rebase_carries_exact_14_retry(self):
        hint = json.loads((PROFILE / "examples/changes-since-physical-rebase.json").read_text())["hint"]
        target = (PROFILE / "examples/signed-full-manifest-43.json").read_bytes()
        self.assertTrue(self.contract.verify_rebase(self.request, hint, self.receipt["vehicle_id"], target))
        tampered = json.loads(json.dumps(hint))
        tampered["retry_request"].pop("accepted_changed_set_formats")
        with self.assertRaisesRegex(self.contract.ContractError, "rebase violates"):
            self.contract.verify_rebase(self.request, tampered, self.receipt["vehicle_id"], target)

    def test_full_rebase_over_delta_cap_and_full_boundary(self):
        fixture = json.loads((PROFILE / "examples/changes-since-physical-rebase-large.json").read_text())
        hint = fixture["hint"]
        manifest = (PROFILE / "examples/signed-full-manifest-43-large.json").read_bytes()
        self.assertEqual(len(hint["replacement"]["chunks"]), 65)
        self.assertTrue(self.contract.verify_rebase(self.request, hint, self.receipt["vehicle_id"], manifest))
        schema = json.loads((PROFILE / "physical-rebase-hint.schema.json").read_text())
        validation = {**schema, "$ref": "#/$defs/hint"}
        at_full_limit = json.loads(json.dumps(hint))
        one_pack = at_full_limit["replacement"]["chunks"][0]["pack"]
        at_full_limit["replacement"]["chunks"] = [
            {"chunk_index": index, "pack": one_pack} for index in range(1771)
        ]
        self.assertFalse(list(Draft202012Validator(validation).iter_errors(at_full_limit)))
        beyond_full_limit = json.loads(json.dumps(at_full_limit))
        beyond_full_limit["replacement"]["chunks"].append({"chunk_index": 1771, "pack": one_pack})
        self.assertTrue(list(Draft202012Validator(validation).iter_errors(beyond_full_limit)))
        delta_schema = json.loads((PROFILE / "physical-changed-set-receipt.schema.json").read_text())
        delta_validation = {**delta_schema, "$ref": "#/$defs/receipt"}
        over_delta_limit = json.loads(json.dumps(self.receipt))
        over_delta_limit["chunks"] = hint["replacement"]["chunks"]
        self.assertTrue(list(Draft202012Validator(delta_validation).iter_errors(over_delta_limit)))

    def test_absent_drive_retains_lossless_soft_fk_and_requires_detach_scope(self):
        positive = json.loads((PROFILE / "examples/changes-since-physical-absent-drive-surviving-position.json").read_text())["receipt"]
        self.contract.verify_receipt(self.request, positive, positive["vehicle_id"])
        pack = (PROFILE / positive["chunks"][0]["pack"]["object_name"]).read_bytes()
        self.assertTrue(self.contract.verify_pack(positive, pack))
        for name, expected in (
            ("absent-drive-missing-surviving-position", "incomplete impacted root"),
            ("absent-drive-missing-detach-scope", "missing surviving-child recomputation scope"),
        ):
            with self.subTest(name=name):
                receipt = json.loads((PROFILE / f"examples/changes-since-physical-{name}.json").read_text())["receipt"]
                self.contract.verify_receipt(self.request, receipt, receipt["vehicle_id"])
                bad_pack = (PROFILE / receipt["chunks"][0]["pack"]["object_name"]).read_bytes()
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_pack(receipt, bad_pack)

    def test_delta_row_bound_admits_normal_30_day_cut(self):
        definition = json.loads((PROFILE / "physical-changed-set-receipt.schema.json").read_text())
        receipt_schema = {**definition, "$ref": "#/$defs/receipt"}
        accepted = json.loads(json.dumps(self.receipt))
        accepted["total_rows"] = 311_288
        self.assertFalse(list(Draft202012Validator(receipt_schema).iter_errors(accepted)))
        rejected = json.loads(json.dumps(self.receipt))
        rejected["total_rows"] = 2_000_001
        self.assertTrue(list(Draft202012Validator(receipt_schema).iter_errors(rejected)))


if __name__ == "__main__":
    unittest.main()
