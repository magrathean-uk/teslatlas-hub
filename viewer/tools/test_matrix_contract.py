from __future__ import annotations

import importlib.util
from dataclasses import replace
import hashlib
import json
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).parent
SPEC = importlib.util.spec_from_file_location("viewer_matrix_contract", ROOT / "matrix_contract.py")
assert SPEC is not None and SPEC.loader is not None
contract = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


SESSION_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
DIGEST = "a" * 64


def scenario() -> dict:
    return {
        "schema_version": 1,
        "name": "two-vehicles-five-drives",
        "provenance": "synthetic-only",
        "observed_at_ms": 1788566400000,
        "vehicle_ids": [item["vehicle_id"] for item in contract.SCENARIO_VEHICLES],
        "drive_pages_at_limit_2": contract.SCENARIO_DRIVE_PAGES,
        "equal_start_time_ids": [105, 104],
        "current": contract.SCENARIO_CURRENT,
        "empty_vehicle_observed_at_ms": None,
        "null_distance_drive_id": 101,
        "vehicles": contract.SCENARIO_VEHICLES,
        "later_current": {
            "observed_at_ms": 1788566460000,
            "battery_level": 1,
            "inside_temp": 22.5,
            "outside_temp": None,
        },
    }


def context(*, actors=None, invocations=(), raw=None, observations=None, header=None):
    actors = actors or {
        "viewer_ui": contract.AdmittedActor(
            "viewer_ui", "built_viewer_ui", "viewer_browser", "viewer-installed",
            ("viewer_package_tarball",), ("viewer_source",),
            {"path": "/tmp/viewer-manifest.json", "sha256": DIGEST},
            {"bundle": {"sha256": DIGEST}, "sdk": {"sha256": DIGEST}},
        ),
        "viewer_sdk_contract": contract.AdmittedActor(
            "viewer_sdk_contract", "packed_sdk_dependency", "viewer_browser", "sdk-installed",
            ("typescript_sdk_tarball",), ("typescript_sdk_source",),
            {"path": "/tmp/sdk-manifest.json", "sha256": DIGEST}, {},
        ),
    }
    header = header or {
        "artifacts": [
            {"role": "hub_executable", "sha256": DIGEST},
            {"role": "typescript_sdk_tarball", "sha256": DIGEST},
            {"role": "viewer_package_tarball", "sha256": DIGEST, "embedded_version": contract.PRODUCT_VERSION},
        ],
    }
    return contract.AdmissionContext(
        adapter_id="viewer",
        cell_id="viewer__macos_arm64",
        session_id=SESSION_ID,
        header=header,
        scenario=scenario(),
        actors=actors,
        invocations=tuple(invocations),
        raw=raw or {},
        controller_observations=observations or {1: {}, 2: {}},
    )


def invocation(case_id, actor_id, operation, evidence_id="raw", request_ids=()):
    return contract.AdmittedInvocation(
        f"invoke-{case_id}", case_id, actor_id, operation, 1, 2, evidence_id, tuple(request_ids)
    )


class MatrixContractTests(unittest.TestCase):
    def test_runtime_schema_digest_is_bound_in_manifest(self):
        manifest = json.loads((ROOT / "matrix-contract.json").read_text())
        schema_entry = next(
            item
            for item in manifest["raw_schemas"]
            if item["id"] == "viewer-runtime-v1"
        )
        digest = hashlib.sha256(
            (ROOT / "viewer-runtime-v1.schema.json").read_bytes()
        ).hexdigest()
        self.assertEqual(schema_entry["schema"]["sha256"], digest)
        validator_digest = hashlib.sha256(
            (ROOT / "matrix_contract.py").read_bytes()
        ).hexdigest()
        self.assertEqual(manifest["validator"]["sha256"], validator_digest)

    def test_manifest_declares_fixed_cases_and_roles(self):
        manifest = json.loads((ROOT / "matrix-contract.json").read_text())
        self.assertEqual(manifest["adapter_id"], "viewer")
        self.assertEqual(manifest["revision"], contract.CONTRACT_REVISION)
        self.assertEqual(manifest["required_cases"], list(contract.REQUIRED_CASES))
        self.assertEqual([actor["id"] for actor in manifest["actors"]], list(contract.ACTOR_IDS))
        self.assertEqual(len(manifest["cases"]), 24)
        self.assertEqual({case["id"] for case in manifest["cases"]}, set(contract.REQUIRED_CASES))

    def test_valid_zero_request_case_is_admitted(self):
        case_id = "unsupported_operation_zero_requests"
        facts = {"outgoing_requests": 0}
        raw = {
            "schema_version": 1,
            "session_id": SESSION_ID,
            "cell_id": "viewer__macos_arm64",
            "session_input_sha256": DIGEST,
            "actor_id": "viewer_ui",
            "operation": "unsupported_and_logout",
            "session_sequence_before": 1,
            "session_sequence_after": 2,
            "facts": facts,
            "requests": [],
        }
        result = contract.admit_case(
            {"id": case_id, "status": "passed", "expected": facts, "actual": facts,
             "evidence_kind": "zero_request", "request_transcript": []},
            context(
                invocations=[invocation(case_id, "viewer_ui", "unsupported_and_logout")],
                raw={"raw": raw},
            ),
        )
        self.assertEqual(result, contract.AdmissionDecision("passed", "accepted"))

    def test_candidate_identity_uses_fixed_hub_and_sdk_artifacts(self):
        case_id = "candidate_artifact_identity"
        facts = {
            "hub_sha256": DIGEST,
            "tarball_sha256": DIGEST,
            "package_version": contract.PRODUCT_VERSION,
            "installed_members": 81,
        }
        raw = {
            "schema_version": 1, "session_id": SESSION_ID, "cell_id": "viewer__macos_arm64",
            "session_input_sha256": DIGEST, "actor_id": "viewer_ui",
            "operation": "observe_installed_runtime", "session_sequence_before": 1,
            "session_sequence_after": 2, "facts": facts, "requests": [],
        }
        result = contract.admit_case(
            {"id": case_id, "status": "passed", "expected": facts, "actual": facts,
             "evidence_kind": "identity", "request_transcript": [],
             "process_evidence": {"bound": True}},
            context(invocations=[invocation(case_id, "viewer_ui", "observe_installed_runtime")], raw={"raw": raw}),
        )
        self.assertEqual(result, contract.AdmissionDecision("passed", "accepted"))

    def test_discovery_expected_identity_comes_from_controller_observation(self):
        case_id = "discovery_identity_profile"
        facts = {
            "hub_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "api_versions": ["1.0"], "protocol": "teslatlas-sync", "protocol_major": 1,
            "pack_format": "sqlite-zstd", "version": contract.PRODUCT_VERSION,
        }
        request = {"method": "GET", "route": "/.well-known/teslatlas-hub", "status": 200, "request_id": "discovery-1"}
        raw = {
            "schema_version": 1, "session_id": SESSION_ID, "cell_id": "viewer__macos_arm64",
            "session_input_sha256": DIGEST, "actor_id": "viewer_ui", "operation": "discover",
            "session_sequence_before": 1, "session_sequence_after": 2, "facts": facts, "requests": [request],
        }
        result = contract.admit_case(
            {"id": case_id, "status": "passed", "expected": facts, "actual": facts,
             "evidence_kind": "http", "request_transcript": [request]},
            context(invocations=[invocation(case_id, "viewer_ui", "discover", request_ids=("discovery-1",))], raw={"raw": raw}, observations={1: {}, 2: {"hub_id": facts["hub_id"]}}),
        )
        self.assertEqual(result, contract.AdmissionDecision("passed", "accepted"))

    def test_pending_installed_service_requires_fixed_mode(self):
        expected = {"service_mode": "installed-app-launchagent"}
        case = {"id": "installed_service_runtime", "status": "pending", "expected": expected,
                "actual": expected, "evidence_kind": "identity", "request_transcript": [],
                "process_evidence": {"status": "pending"}}
        self.assertEqual(contract.admit_case(case, context()), contract.AdmissionDecision("pending", "evidence_pending"))
        case["actual"] = {"service_mode": "owned-user-process"}
        self.assertEqual(contract.admit_case(case, context()).code, "literal_mismatch")

    def test_wrong_actor_and_changed_raw_are_rejected(self):
        case_id = "unsupported_operation_zero_requests"
        facts = {"outgoing_requests": 0}
        raw = {
            "schema_version": 1, "session_id": SESSION_ID, "cell_id": "viewer__macos_arm64",
            "session_input_sha256": DIGEST, "actor_id": "viewer_sdk_contract",
            "operation": "unsupported_and_logout", "session_sequence_before": 1,
            "session_sequence_after": 2, "facts": facts, "requests": [],
        }
        case = {"id": case_id, "status": "passed", "expected": facts, "actual": facts,
                "evidence_kind": "zero_request", "request_transcript": []}
        wrong_actor = contract.admit_case(
            case, context(invocations=[invocation(case_id, "viewer_sdk_contract", "unsupported_and_logout")], raw={"raw": raw})
        )
        self.assertEqual(wrong_actor.code, "wrong_actor")
        raw["actor_id"] = "viewer_ui"
        raw["facts"] = {"outgoing_requests": 1}
        changed = contract.admit_case(
            case, context(invocations=[invocation(case_id, "viewer_ui", "unsupported_and_logout")], raw={"raw": raw})
        )
        self.assertEqual(changed.code, "raw_fact_mismatch")

    def test_role_manifest_invocation_and_request_bindings_fail_closed(self):
        case_id = "unsupported_operation_zero_requests"
        facts = {"outgoing_requests": 0}
        case = {"id": case_id, "status": "passed", "expected": facts, "actual": facts,
                "evidence_kind": "zero_request", "request_transcript": []}
        actor_map = context().actors
        wrong_source = dict(actor_map)
        wrong_source["viewer_ui"] = replace(actor_map["viewer_ui"], source_roles=("foreign_source",))
        decision = contract.admit_case(
            case, context(actors=wrong_source, invocations=[invocation(case_id, "viewer_ui", "unsupported_and_logout")], raw={})
        )
        self.assertEqual(decision.code, "wrong_source_role")
        wrong_artifact = dict(actor_map)
        wrong_artifact["viewer_ui"] = replace(actor_map["viewer_ui"], artifact_roles=("typescript_sdk_tarball",))
        decision = contract.admit_case(
            case, context(actors=wrong_artifact, invocations=[invocation(case_id, "viewer_ui", "unsupported_and_logout")], raw={})
        )
        self.assertEqual(decision.code, "wrong_artifact_role")
        wrong_manifest = dict(actor_map)
        wrong_manifest["viewer_ui"] = replace(actor_map["viewer_ui"], installed_manifest={"path": "/tmp/viewer-manifest.json", "sha256": "z" * 64})
        decision = contract.admit_case(
            case, context(actors=wrong_manifest, invocations=[invocation(case_id, "viewer_ui", "unsupported_and_logout")], raw={})
        )
        self.assertEqual(decision.code, "installed_manifest_mismatch")
        self.assertEqual(contract.admit_case(case, context()).code, "invocation_missing")

    def test_unrelated_request_and_wrong_zero_fact_are_rejected(self):
        case_id = "discovery_identity_profile"
        facts = {
            "hub_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "api_versions": ["1.0"], "protocol": "teslatlas-sync", "protocol_major": 1,
            "pack_format": "sqlite-zstd", "version": contract.PRODUCT_VERSION,
        }
        request = {"method": "GET", "route": "/healthz", "status": 200, "request_id": "health-1"}
        raw = {
            "schema_version": 1, "session_id": SESSION_ID, "cell_id": "viewer__macos_arm64",
            "session_input_sha256": DIGEST, "actor_id": "viewer_ui", "operation": "discover",
            "session_sequence_before": 1, "session_sequence_after": 2, "facts": facts, "requests": [request],
        }
        decision = contract.admit_case(
            {"id": case_id, "status": "passed", "expected": facts, "actual": facts,
             "evidence_kind": "http", "request_transcript": [request]},
            context(invocations=[invocation(case_id, "viewer_ui", "discover", request_ids=("health-1",))], raw={"raw": raw}, observations={1: {}, 2: {"hub_id": facts["hub_id"]}}),
        )
        self.assertEqual(decision.code, "request_mismatch")
        zero_case = {"id": "unsupported_operation_zero_requests", "status": "passed",
                     "expected": {"outgoing_requests": 0}, "actual": {"outgoing_requests": 1},
                     "evidence_kind": "zero_request", "request_transcript": []}
        self.assertEqual(contract.admit_case(zero_case, context()).code, "literal_mismatch")

    def test_invalid_scenario_and_stale_sequence_fail_closed(self):
        case_id = "unsupported_operation_zero_requests"
        facts = {"outgoing_requests": 0}
        case = {"id": case_id, "status": "passed", "expected": facts, "actual": facts,
                "evidence_kind": "zero_request", "request_transcript": []}
        bad_scenario = context(invocations=[invocation(case_id, "viewer_ui", "unsupported_and_logout")], raw={})
        bad_scenario = bad_scenario.__class__(
            bad_scenario.adapter_id, bad_scenario.cell_id, bad_scenario.session_id,
            bad_scenario.header, {**bad_scenario.scenario, "vehicle_ids": []}, bad_scenario.actors,
            bad_scenario.invocations, bad_scenario.raw, bad_scenario.controller_observations,
        )
        self.assertEqual(contract.admit_case(case, bad_scenario).code, "wrong_context")
        raw = {
            "schema_version": 1, "session_id": SESSION_ID, "cell_id": "viewer__macos_arm64",
            "session_input_sha256": DIGEST, "actor_id": "viewer_ui",
            "operation": "unsupported_and_logout", "session_sequence_before": 3,
            "session_sequence_after": 4, "facts": facts, "requests": [],
        }
        self.assertEqual(
            contract.admit_case(
                case,
                context(invocations=[contract.AdmittedInvocation("i", case_id, "viewer_ui", "unsupported_and_logout", 3, 4, "raw", ())], raw={"raw": raw}),
            ).code,
            "sequence_mismatch",
        )


if __name__ == "__main__":
    unittest.main()
