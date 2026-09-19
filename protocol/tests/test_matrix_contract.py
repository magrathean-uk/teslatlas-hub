"""Source-faithful tests for the Protocol installed adapter contract."""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from jsonschema import Draft202012Validator

from conformance import hub_matrix


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "tools" / "matrix-contract.json"
VALIDATOR_PATH = ROOT / "tools" / "matrix_contract.py"
RAW_SCHEMA_PATH = ROOT / "tools" / "protocol-http-v1.schema.json"


def load_validator():
    spec = importlib.util.spec_from_file_location("protocol_matrix_contract_test", VALIDATOR_PATH)
    if spec is None or spec.loader is None:
        raise AssertionError("Protocol matrix validator cannot be imported")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class MatrixContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
        cls.validator = load_validator()

    def test_manifest_is_closed_and_content_bound(self):
        validated = self.validator.validate_manifest(CONTRACT_PATH)
        self.assertEqual(validated["adapter_id"], "protocol_actual_hub")
        self.assertEqual(
            set(self.contract),
            {
                "schema_version",
                "adapter_id",
                "revision",
                "required_cases",
                "actors",
                "cases",
                "raw_schemas",
                "phases",
                "validator",
            },
        )
        self.assertEqual(self.contract["schema_version"], 1)
        self.assertEqual(self.contract["adapter_id"], "protocol_actual_hub")
        self.assertEqual(self.contract["required_cases"], hub_matrix.CASE_IDS)
        self.assertEqual(self.contract["actors"], [{"id": "protocol_http", "kind": "protocol_http", "required": True}])
        self.assertEqual(self.contract["validator"]["path"], VALIDATOR_PATH.name)
        self.assertEqual(
            self.contract["validator"]["sha256"],
            hashlib.sha256(VALIDATOR_PATH.read_bytes()).hexdigest(),
        )
        self.assertEqual(self.contract["raw_schemas"][0]["schema"]["path"], RAW_SCHEMA_PATH.name)
        self.assertEqual(
            self.contract["raw_schemas"][0]["schema"]["sha256"],
            hashlib.sha256(RAW_SCHEMA_PATH.read_bytes()).hexdigest(),
        )

    def test_manifest_bindings_resolve_from_the_contract_directory(self):
        """A copied contract bundle must not depend on the authoring checkout path."""
        with tempfile.TemporaryDirectory() as directory:
            copied = Path(directory)
            contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
            contract["raw_schemas"][0]["schema"]["path"] = RAW_SCHEMA_PATH.name
            contract["validator"]["path"] = VALIDATOR_PATH.name
            copied_contract = copied / CONTRACT_PATH.name
            copied_contract.write_text(json.dumps(contract), encoding="utf-8")
            shutil.copy2(RAW_SCHEMA_PATH, copied / RAW_SCHEMA_PATH.name)
            shutil.copy2(VALIDATOR_PATH, copied / VALIDATOR_PATH.name)
            self.assertEqual(
                self.validator.validate_manifest(copied_contract)["adapter_id"],
                "protocol_actual_hub",
            )

    def test_raw_schema_and_validator_exports_are_valid(self):
        raw_schema = json.loads(RAW_SCHEMA_PATH.read_text(encoding="utf-8"))
        Draft202012Validator.check_schema(raw_schema)
        raw_validator = Draft202012Validator(raw_schema)
        valid_facts = {
            "candidate_artifact_identity": {"hub_sha256": "1" * 64, "seed_sha256": "2" * 64, "product_version": "2026.36.2"},
            "installed_service_runtime": {"service_mode": "installed-deb-systemd"},
            "discovery_identity_profile": {"hub_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "api_versions": ["1.0"], "protocol": "teslatlas-sync", "protocol_major": 1, "pack_format": "sqlite-zstd", "version": "2026.36.2"},
            "unauthenticated_discovery": {"discovery": 200, "health": 200, "readiness": 200, "credential_absent": True},
            "bad_invitation": {"typed_error": "hub_http_error", "http_status": 401},
            "expired_invitation": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
            "replayed_invitation": {"typed_error": "hub_http_error", "http_status": 401},
            "real_auth": {"claimed": 200, "vehicles": [{"vehicle_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "display_name": "Synthetic"}]},
            "credential_lifecycle_reauth": {"new_device": True, "vehicles": 200},
            "revocation": {"typed_error": "hub_http_error", "http_status": 401},
            "unknown_vehicle": {"typed_error": "hub_http_error", "http_status": 404},
            "exact_current_values": {"battery_level": 0, "inside_temp": 21.5, "outside_temp": None, "observed_at_ms": 1788566400000, "est_battery_range_km": 160.93, "odometer": 16093.44, "speed": 16, "scheduled_charging_start_time": 1788570000, "active_route_miles_to_arrival": 12.5, "empty_vehicle_observed_at_ms": None},
            "endpoint_restart": {"same_hub": True, "new_process": True, "vehicles": 200},
            "outage_recovery": {"outage_observed": True, "vehicles": 200},
            "unsupported_operation_zero_requests": {"outgoing_requests": 0},
            "credential_rotation_api": {"rotated": True, "same_device": True, "vehicles": 200, "old_credential_error": "hub_http_error", "old_credential_status": 401},
            "drives_three_page_order": {"pages": [[105, 104], [103, 102], [101]]},
            "drives_terminal_cursor": {"next_cursor": None, "ids": [101]},
            "drives_etag_304": {"kind": "notModified", "post_304_ids": [103, 102]},
            "drives_wrong_vehicle_cursor": {"typed_error": "hub_http_error", "http_status": 400},
            "drives_wrong_filter_cursor": {"typed_error": "hub_http_error", "http_status": 400},
        }
        base = {"schema_version": 1, "session_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "cell_id": "protocol_actual_hub__macos_arm64", "session_input_sha256": "3" * 64, "actor_id": "protocol_http", "operation": "observe_identity", "actor_manifest_sha256": "4" * 64, "session_sequence_before": 1, "session_sequence_after": 1, "credential_device_id": None, "facts": valid_facts["candidate_artifact_identity"], "requests": [], "request_ids": [], "cleanup": {"status": "passed", "transport_resources_closed": True, "auxiliary_fixture_stopped": True, "process_exited": True}}
        self.assertFalse(list(raw_validator.iter_errors(base)))
        malformed = json.loads(json.dumps(base))
        malformed["facts"]["unexpected"] = True
        self.assertTrue(list(raw_validator.iter_errors(malformed)))
        self.assertEqual(self.validator.ADAPTER_ID, "protocol_actual_hub")
        self.assertEqual(self.validator.CONTRACT_REVISION, self.contract["revision"])
        self.assertIsInstance(self.validator.DECISION_CODES, frozenset)
        self.assertTrue(callable(self.validator.admit_case))
        self.assertEqual(tuple(self.validator.REQUIRED_CASES), tuple(hub_matrix.CASE_IDS))

    def test_case_manifest_covers_exact_operations_and_evidence_kinds(self):
        cases = {item["id"]: item for item in self.contract["cases"]}
        self.assertEqual(list(cases), hub_matrix.CASE_IDS)
        self.assertEqual(set(cases), set(hub_matrix.CASE_IDS))
        for case_id in hub_matrix.CASE_IDS:
            case = cases[case_id]
            self.assertEqual(set(case), {"id", "actor_ids", "evidence_kind", "operations", "raw_schema_ids"})
            self.assertEqual(case["actor_ids"], ["protocol_http"])
            self.assertEqual(case["raw_schema_ids"], ["protocol-http-v1"])
            expected_kind = (
                "identity"
                if case_id in {"candidate_artifact_identity", "installed_service_runtime"}
                else "zero_request"
                if case_id in {
                    "expired_invitation",
                    "unsupported_operation_zero_requests",
                }
                else "http"
            )
            self.assertEqual(case["evidence_kind"], expected_kind)
            self.assertEqual(case["operations"], list(self.validator.CASE_OPERATIONS[case_id]))

    def test_validator_rejects_wrong_context_and_case_shape(self):
        context = self.validator.AdmissionContext(
            adapter_id="wrong",
            cell_id="protocol_actual_hub__macos_arm64",
            session_id="11111111-1111-4111-8111-111111111111",
            header={},
            scenario={},
            actors={},
            invocations=(),
            raw={},
            controller_observations={},
        )
        decision = self.validator.admit_case({}, context)
        self.assertEqual((decision.status, decision.code), ("failed", "case_shape"))
        case = {
            "id": "unsupported_operation_zero_requests",
            "status": "passed",
            "expected": {"outgoing_requests": 0},
            "actual": {"outgoing_requests": 0},
            "evidence_kind": "zero_request",
            "request_transcript": [],
        }
        decision = self.validator.admit_case(case, context)
        self.assertEqual((decision.status, decision.code), ("failed", "wrong_context"))

    def test_validator_admits_bound_discovery_case_and_rejects_changed_raw_fact(self):
        header = {
            "source_identities": [{"role": "protocol_source"}],
            "artifacts": [
                {"role": "hub_executable", "sha256": "1" * 64},
                {"role": "protocol_fixture_seed", "sha256": "2" * 64},
            ],
            "runtime": {"hub": {"service_mode": "installed-deb-systemd"}},
        }
        actor = self.validator.AdmittedActor(
            id="protocol_http",
            kind="protocol_http",
            runtime_ref="root_python",
            entrypoint_ref="protocol_actual_hub",
            artifact_roles=(),
            source_roles=("protocol_source",),
            installed_manifest={"path": "/private/actor-manifest.json", "sha256": "3" * 64},
            runtime={},
        )
        invocation = self.validator.AdmittedInvocation(
            id="invoke-discovery",
            case_id="discovery_identity_profile",
            actor_id="protocol_http",
            operation="discovery",
            session_sequence_before=1,
            session_sequence_after=1,
            evidence_id="raw-discovery",
            request_ids=("request-1",),
        )
        context = self.validator.AdmissionContext(
            adapter_id="protocol_actual_hub",
            cell_id="protocol_actual_hub__macos_arm64",
            session_id="11111111-1111-4111-8111-111111111111",
            header=header,
            scenario={},
            actors={"protocol_http": actor},
            invocations=(invocation,),
            raw={},
            controller_observations={1: {"hub_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}},
        )
        expected = self.validator.expected_normalized_facts("discovery_identity_profile", context)
        context.raw["raw-discovery"] = {
            "schema_version": 1,
            "session_id": context.session_id,
            "cell_id": context.cell_id,
            "session_input_sha256": "4" * 64,
            "actor_id": "protocol_http",
            "operation": "discovery",
            "actor_manifest_sha256": "3" * 64,
            "session_sequence_before": 1,
            "session_sequence_after": 1,
            "credential_device_id": None,
            "facts": expected,
            "requests": [{"method": "GET", "route": "/.well-known/teslatlas-hub", "status": 200, "request_id": "request-1"}],
            "request_ids": ["request-1"],
            "cleanup": {"status": "passed", "transport_resources_closed": True, "auxiliary_fixture_stopped": True, "process_exited": True},
        }
        case = {
            "id": "discovery_identity_profile",
            "status": "passed",
            "expected": expected,
            "actual": expected,
            "evidence_kind": "http",
            "request_transcript": context.raw["raw-discovery"]["requests"],
        }
        self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("passed", "accepted"))
        context.raw["raw-discovery"]["request_ids"] = []
        self.assertEqual(self.validator.admit_case(case, context).code, "request_mismatch")
        context.raw["raw-discovery"]["request_ids"] = ["request-1"]
        context.raw["raw-discovery"]["facts"] = {**expected, "version": "wrong"}
        self.assertEqual(self.validator.admit_case(case, context).code, "raw_fact_mismatch")


if __name__ == "__main__":
    unittest.main()
