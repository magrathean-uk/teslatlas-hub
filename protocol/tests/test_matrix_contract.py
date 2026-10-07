"""Source-faithful tests for the Protocol installed adapter contract."""

from __future__ import annotations

import copy
from dataclasses import replace
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
from types import MappingProxyType, SimpleNamespace
import unittest

from jsonschema import Draft202012Validator

from conformance import hub_matrix


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_PATH = ROOT / "tools" / "matrix-contract.json"
VALIDATOR_PATH = ROOT / "tools" / "matrix_contract.py"
RAW_SCHEMA_PATH = ROOT / "tools" / "protocol-http-v1.schema.json"


# Benign metadata exported by the actual Hub build_controller_admission_view.
# No proof, journal, actor exit or installed execution is validated by this fixture.
RESTART_OBSERVATIONS = {1: {'schema_version': 1,
     'session_id': '11111111-1111-4111-8111-111111111111',
     'sequence': 1,
     'operation': 'verify',
     'state': 'running',
     'started_monotonic_ns': 10,
     'finished_monotonic_ns': 11,
     'observed_at_ms': 1001,
     'result_sha256': 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
     'proof_sha256': '9c785dec503b835c05ebb82034dee1a2f4464eceec9661decf8826648fb8f161',
     'scenario_sha256': 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
     'seed_sha256': 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
     'store_id': 'store-1',
     'store_schema_version': 59,
     'hub_id': 'store-1',
     'service_generation': 'boot:start-1',
     'invitations': {'active': {'pairing_id': 'pair-1', 'expires_at_ms': 2000},
                     'expired': {'pairing_id': 'pair-0', 'expires_at_ms': 900}},
     'transition': None},
 3: {'schema_version': 1,
     'session_id': '11111111-1111-4111-8111-111111111111',
     'sequence': 3,
     'operation': 'start',
     'state': 'running',
     'started_monotonic_ns': 30,
     'finished_monotonic_ns': 31,
     'observed_at_ms': 1003,
     'result_sha256': 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
     'proof_sha256': '1d5a39ea3e4a8cd4c911af5ad3164677d22c31f24f7f285fb1719c619a87cd3d',
     'scenario_sha256': 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
     'seed_sha256': 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
     'store_id': 'store-1',
     'store_schema_version': 59,
     'hub_id': 'store-1',
     'service_generation': 'boot:start-3',
     'invitations': {'active': {'pairing_id': 'pair-1', 'expires_at_ms': 2000},
                     'expired': {'pairing_id': 'pair-0', 'expires_at_ms': 900}},
     'transition': {'kind': 'start', 'from_sequence': 1, 'stopped_sequence': 2}},
 4: {'schema_version': 1,
     'session_id': '11111111-1111-4111-8111-111111111111',
     'sequence': 4,
     'operation': 'verify',
     'state': 'running',
     'started_monotonic_ns': 40,
     'finished_monotonic_ns': 41,
     'observed_at_ms': 1004,
     'result_sha256': 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
     'proof_sha256': '62c53792f96c50485cd24a3e279c955bed87878f6efd9cd36681eb736b02ebc2',
     'scenario_sha256': 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
     'seed_sha256': 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
     'store_id': 'store-1',
     'store_schema_version': 59,
     'hub_id': 'store-1',
     'service_generation': 'boot:start-3',
     'invitations': {'active': {'pairing_id': 'pair-1', 'expires_at_ms': 2000},
                     'expired': {'pairing_id': 'pair-0', 'expires_at_ms': 900}},
     'transition': None}}


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
            "credential_rotation_api": {"rotated": True, "same_device": True, "vehicles": 200, "old_credential_status": 200, "lost_response_retry_status": 200},
            "drives_three_page_order": {"pages": [[105, 104], [103, 102], [101]]},
            "drives_terminal_cursor": {"next_cursor": None, "ids": [101]},
            "drives_etag_304": {"kind": "notModified", "post_304_ids": [103, 102]},
            "drives_wrong_vehicle_cursor": {"typed_error": "hub_http_error", "http_status": 400},
            "drives_wrong_filter_cursor": {"typed_error": "hub_http_error", "http_status": 400},
        }
        base = {"schema_version": 1, "session_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "cell_id": "protocol_actual_hub__macos_arm64", "session_input_sha256": "3" * 64, "actor_id": "protocol_http", "operation": "observe_identity", "actor_manifest_sha256": "4" * 64, "session_sequence_before": 1, "session_sequence_after": 1, "credential_device_id": None, "facts": valid_facts["candidate_artifact_identity"], "requests": [], "request_ids": [], "cleanup": {"status": "passed", "transport_resources_closed": True, "auxiliary_fixture_stopped": True, "process_exited": True}}
        self.assertFalse(list(raw_validator.iter_errors(base)))
        for case_id,facts in valid_facts.items():
            with self.subTest(case_id=case_id):
                envelope={**base,'operation':self.validator.CASE_OPERATIONS[case_id][0],'facts':facts}
                self.assertFalse(list(raw_validator.iter_errors(envelope)))
                invalid={**envelope,'facts':{**facts,'unexpected':True}}
                self.assertTrue(list(raw_validator.iter_errors(invalid)))
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


    def _restart_fixture(self, observations=None, *, before=1, after=4):
        validator = self.validator
        header = {
            "source_identities": [{"role": "protocol_source"}],
            "artifacts": [{"role": "hub_executable", "sha256": "1" * 64},
                          {"role": "protocol_fixture_seed", "sha256": "2" * 64}],
            "runtime": {"hub": {"service_mode": "installed-deb-systemd"}},
        }
        actor = validator.AdmittedActor(
            id="protocol_http", kind="protocol_http", runtime_ref="root_python",
            entrypoint_ref="protocol_actual_hub", artifact_roles=(), source_roles=("protocol_source",),
            installed_manifest={"path": "/private/actor-manifest.json", "sha256": "3" * 64}, runtime={},
        )
        requests = [
            {"method": "GET", "route": "/.well-known/teslatlas-hub", "status": 200, "request_id": "request-1"},
            {"method": "GET", "route": "/v1/vehicles", "status": 200, "request_id": "request-2"},
        ]
        invocation = validator.AdmittedInvocation(
            id="invoke-restart", case_id="endpoint_restart", actor_id="protocol_http", operation="endpoint_restart",
            session_sequence_before=before, session_sequence_after=after, evidence_id="raw-restart",
            request_ids=("request-1", "request-2"),
        )
        facts = {"same_hub": True, "new_process": True, "vehicles": 200}
        raw = {
            "schema_version": 1, "session_id": RESTART_OBSERVATIONS[1]["session_id"],
            "cell_id": "protocol_actual_hub__macos_arm64", "session_input_sha256": "4" * 64,
            "actor_id": "protocol_http", "operation": "endpoint_restart", "actor_manifest_sha256": "3" * 64,
            "session_sequence_before": before, "session_sequence_after": after, "credential_device_id": None,
            "facts": facts, "requests": requests, "request_ids": ["request-1", "request-2"],
            # Schema literals only: this pure predicate fixture claims no cleanup observation.
            "cleanup": {"status": "passed", "transport_resources_closed": True,
                        "auxiliary_fixture_stopped": True, "process_exited": True},
        }
        Draft202012Validator(json.loads(RAW_SCHEMA_PATH.read_text())).validate(raw)
        context = validator.AdmissionContext(
            adapter_id="protocol_actual_hub", cell_id=raw["cell_id"], session_id=raw["session_id"], header=header,
            scenario={}, actors={"protocol_http": actor}, invocations=(invocation,), raw={"raw-restart": raw},
            controller_observations=copy.deepcopy(RESTART_OBSERVATIONS if observations is None else observations),
        )
        case = {"id": "endpoint_restart", "status": "passed", "expected": facts, "actual": facts,
                "evidence_kind": "http", "request_transcript": requests}
        return case, context

    def test_restart_accepts_retained_start_followed_by_final_verify(self):
        case, context = self._restart_fixture()
        self.assertEqual(context.controller_observations[4]["operation"], "verify")
        self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("passed", "accepted"))
        frozen = {
            sequence: MappingProxyType({**row, "transition": MappingProxyType(row["transition"]) if row["transition"] else None})
            for sequence, row in context.controller_observations.items()
        }
        self.assertEqual(self.validator.admit_case(case, replace(context, controller_observations=MappingProxyType(frozen))),
                         self.validator.AdmissionDecision("passed", "accepted"))

    def test_restart_rejects_verify_only_same_generation_later_anchor(self):
        rows = {1: copy.deepcopy(RESTART_OBSERVATIONS[1]), 2: copy.deepcopy(RESTART_OBSERVATIONS[1])}
        rows[2]["sequence"] = 2
        case, context = self._restart_fixture(rows, after=2)
        self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("failed", "controller_mismatch"))

    def test_restart_normal_producer_retains_final_verify_without_transport(self):
        """Execute only the current restart action/capture against in-memory I/O.

        Other matrix actions are explicitly unexecuted. The controller/proof/TLS
        admission boundary is stubbed; this checks producer sequencing and the
        public predicate, not a real controller, transport or installed lifecycle.
        """
        rows = copy.deepcopy(RESTART_OBSERVATIONS)
        operations = []
        replies = iter(("verify", "stop", "start", "verify"))

        class Control:
            def request(inner, op):
                self.assertEqual(op, next(replies))
                operations.append(op)
                if op == "stop":
                    return {"stopped": True, "events": list(operations)}
                sequence = {1: 1, 3: 3, 4: 4}[len(operations)]
                return rows[sequence]

        class Http:
            def reset_transcript(inner):
                inner.transcript = []

            def exchange(inner, kind, path, **kwargs):
                self.assertIn(kind, ("discovery", "vehicles"))
                inner.transcript.append({"method": "GET", "route": path, "status": 200,
                                         "request_id": "request-" + str(len(inner.transcript) + 1)})
                return {"hub_id": rows[1]["hub_id"]}, {}

        matrix = hub_matrix.MatrixCases.__new__(hub_matrix.MatrixCases)
        _, context = self._restart_fixture(rows)
        matrix.config = context.header
        matrix.control, matrix.http = Control(), Http()
        matrix.descriptor = {"hub_id": rows[1]["hub_id"], "service_generation": rows[1]["service_generation"]}
        matrix.proof, matrix.events = None, []
        matrix.token, matrix.device_id = "fixture-only", None
        matrix.results, matrix.anchors, matrix.case_device_ids = [], {}, {}

        def retain(row):
            matrix.descriptor = {"hub_id": row["hub_id"], "service_generation": row["service_generation"]}
            matrix.proof = {"sequence": row["sequence"]}
            return row

        def selected_capture(case_id, *args, **kwargs):
            if case_id == "endpoint_restart":
                return hub_matrix.MatrixCases.capture(matrix, case_id, *args, **kwargs)
            matrix.results.append({"id": case_id, "status": "unexecuted"})

        matrix._admit_running = retain
        matrix.capture = selected_capture
        observed = next(case for case in matrix.run() if case["id"] == "endpoint_restart")
        self.assertEqual(operations, ["verify", "stop", "start", "verify"])
        self.assertEqual(matrix.anchors["endpoint_restart"], (1, 4))
        self.assertEqual(observed["status"], "passed")
        case = {key: observed[key] for key in ("id", "status", "expected", "actual", "evidence_kind", "request_transcript")}
        self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("passed", "accepted"))

    def test_restart_rejects_equal_anchors(self):
        case, context = self._restart_fixture({1: RESTART_OBSERVATIONS[1]}, after=1)
        self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("failed", "controller_mismatch"))

    def test_local_restart_admission_keeps_installed_publication_refusal(self):
        case, context = self._restart_fixture()
        self.assertEqual(self.validator.admit_case(case, context).status, "passed")
        # Satisfy only completion's in-memory prerequisites; no case execution
        # or actor cleanup is inferred from these literals.
        matrix = SimpleNamespace(
            anchors={case_id: (1, 4) for case_id in hub_matrix.CASE_IDS},
            case_device_ids={case_id: None for case_id in hub_matrix.CASE_IDS},
        )
        cases = [{"id": case_id, "status": "passed"} for case_id in hub_matrix.CASE_IDS]
        with self.assertRaisesRegex(hub_matrix.MatrixAcceptanceError, "lacks an observed actor-exit witness"):
            hub_matrix._complete_v2({}, {}, matrix, cases)

    def test_restart_accepts_multiple_cycles_on_final_start_generation(self):
        rows = copy.deepcopy(RESTART_OBSERVATIONS)
        rows[6] = copy.deepcopy(rows[3])
        rows[6].update(sequence=6, service_generation="boot:start-6",
                       transition={"kind": "start", "from_sequence": 4, "stopped_sequence": 5})
        rows[7] = copy.deepcopy(rows[4])
        rows[7].update(sequence=7, service_generation="boot:start-6")
        case, context = self._restart_fixture(rows, after=7)
        self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("passed", "accepted"))

    def test_restart_rejects_non_immediate_origin_and_running_stop_reference(self):
        rows = {1: copy.deepcopy(RESTART_OBSERVATIONS[1]), 3: copy.deepcopy(RESTART_OBSERVATIONS[1]),
                5: copy.deepcopy(RESTART_OBSERVATIONS[3]), 6: copy.deepcopy(RESTART_OBSERVATIONS[4])}
        rows[3]["sequence"] = 3
        rows[5].update(sequence=5, transition={"kind": "start", "from_sequence": 1, "stopped_sequence": 4})
        rows[6]["sequence"] = 6
        with self.subTest(label="existing but non-immediate origin"):
            case, context = self._restart_fixture(rows, after=6)
            self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("failed", "controller_mismatch"))
        rows = copy.deepcopy(RESTART_OBSERVATIONS)
        rows[2] = copy.deepcopy(rows[1])
        rows[2]["sequence"] = 2
        with self.subTest(label="stop reference is a running observation"):
            case, context = self._restart_fixture(rows)
            self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("failed", "controller_mismatch"))

    def test_restart_rejects_unjoined_controller_facts(self):
        mutations = [
            ("missing transition", 3, "transition", None),
            ("wrong transition kind", 3, "transition", {"kind": "verify", "from_sequence": 1, "stopped_sequence": 2}),
            ("missing stop reference", 3, "transition", {"kind": "start", "from_sequence": 1}),
            ("future stop reference", 3, "transition", {"kind": "start", "from_sequence": 1, "stopped_sequence": 5}),
            ("stop before window", 3, "transition", {"kind": "start", "from_sequence": 1, "stopped_sequence": 0}),
            ("boolean stop reference", 3, "transition", {"kind": "start", "from_sequence": 1, "stopped_sequence": True}),
            ("string stop reference", 3, "transition", {"kind": "start", "from_sequence": 1, "stopped_sequence": "2"}),
            ("from outside window", 3, "transition", {"kind": "start", "from_sequence": 0, "stopped_sequence": 2}),
            ("missing from row", 3, "transition", {"kind": "start", "from_sequence": 2, "stopped_sequence": 2}),
            ("foreign session", 3, "session_id", "22222222-2222-4222-8222-222222222222"),
            ("foreign Hub", 3, "hub_id", "other-hub"),
            ("foreign store", 3, "store_id", "other-store"),
            ("different store schema", 3, "store_schema_version", 60),
            ("row sequence mismatch", 3, "sequence", 2),
            ("boolean row sequence", 1, "sequence", True),
            ("non-running start", 3, "state", "stopped"),
            ("wrong schema", 3, "schema_version", 2),
            ("unchanged generation", 3, "service_generation", "boot:start-1"),
            ("empty generation", 3, "service_generation", ""),
            ("different final generation", 4, "service_generation", "boot:start-5"),
            ("foreign final session", 4, "session_id", "other-session"),
            ("start label removed", 3, "operation", "verify"),
        ]
        for label, sequence, field, value in mutations:
            rows = copy.deepcopy(RESTART_OBSERVATIONS)
            rows[sequence][field] = value
            with self.subTest(label=label):
                case, context = self._restart_fixture(rows)
                self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("failed", "controller_mismatch"))
        for label, before, after, extra_sequence in (("start before window", 4, 5, 5), ("start after window", 1, 2, 2)):
            with self.subTest(label=label):
                rows = copy.deepcopy(RESTART_OBSERVATIONS)
                rows[extra_sequence] = copy.deepcopy(rows[before])
                rows[extra_sequence]["sequence"] = extra_sequence
                case, context = self._restart_fixture(rows, before=before, after=after)
                self.assertEqual(self.validator.admit_case(case, context), self.validator.AdmissionDecision("failed", "controller_mismatch"))

    def test_observed_matrix_requires_each_operation_request_sequence(self):
        from test_hub_matrix import HubMatrixTests, SyntheticControl, SyntheticHub

        class RetainedViewControl(SyntheticControl):
            """Retain fake control actions in the supplied builder's row shape.

            This remains a synthetic producer test, with no common proof admission.
            Stops consume a sequence but are not running observations.
            """
            def __init__(self, hub):
                super().__init__(hub)
                self.controller_observations = {}
                self.stopped_transition = None

            def request(self, op, *, device_id=None):
                previous_sequence = self.sequence
                result = super().request(op, device_id=device_id)
                if op == "stop":
                    self.sequence += 1
                    self.stopped_transition = (previous_sequence, self.sequence)
                    return result
                proof = result["proof"]
                row = copy.deepcopy(RESTART_OBSERVATIONS[1])
                row.update(
                    sequence=self.sequence, operation=op,
                    started_monotonic_ns=self.sequence * 10,
                    finished_monotonic_ns=self.sequence * 10 + 1,
                    observed_at_ms=1000 + self.sequence,
                    session_id=proof["session_id"], hub_id=proof["discovery"]["hub_id"],
                    store_id=proof["discovery"]["hub_id"],
                    service_generation=proof["service"]["generation"], transition=None,
                )
                if op == "start" and self.stopped_transition is not None:
                    origin, stopped = self.stopped_transition
                    row["transition"] = {"kind": "start", "from_sequence": origin, "stopped_sequence": stopped}
                    self.stopped_transition = None
                self.controller_observations[self.sequence] = row
                return result

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hub = SyntheticHub(root)
            self.addCleanup(hub.close)
            config = HubMatrixTests().config_for(hub, root)
            control = RetainedViewControl(hub)
            matrix = hub_matrix.MatrixCases(config, control)
            observed_cases = matrix.run()
            self.assertEqual({case["status"] for case in observed_cases}, {"passed"})
            actor = self.validator.AdmittedActor(
                id="protocol_http", kind="protocol_http", runtime_ref="root_python",
                entrypoint_ref="protocol_actual_hub", artifact_roles=(), source_roles=("protocol_source",),
                installed_manifest={"path": "/private/actor-manifest.json", "sha256": "3" * 64}, runtime={},
            )
            context = self.validator.AdmissionContext(
                adapter_id="protocol_actual_hub", cell_id=config["cell_id"],
                session_id=config["host_session"]["session_id"], header=config,
                scenario={}, actors={"protocol_http": actor}, invocations=(), raw={},
                controller_observations=control.controller_observations,
            )

            def decide(observed, requests):
                case_id = observed["id"]
                case = {key: copy.deepcopy(observed[key]) for key in ("id", "status", "expected", "actual", "evidence_kind", "request_transcript")}
                case["request_transcript"] = copy.deepcopy(requests)
                if case_id == "installed_service_runtime":
                    case["status"] = "pending"
                request_ids = tuple(item["request_id"] for item in requests if "request_id" in item)
                before, after = matrix.anchors[case_id]
                invocation = self.validator.AdmittedInvocation(
                    id="invoke-" + case_id, case_id=case_id, actor_id="protocol_http",
                    operation=self.validator.CASE_OPERATIONS[case_id][0],
                    session_sequence_before=before, session_sequence_after=after,
                    evidence_id="raw-" + case_id, request_ids=request_ids,
                )
                raw = {
                    "schema_version": 1, "session_id": context.session_id, "cell_id": context.cell_id,
                    "session_input_sha256": "4" * 64, "actor_id": "protocol_http",
                    "operation": invocation.operation, "actor_manifest_sha256": "3" * 64,
                    "session_sequence_before": before, "session_sequence_after": after,
                    "credential_device_id": matrix.case_device_ids[case_id],
                    "facts": copy.deepcopy(observed["actual"]), "requests": copy.deepcopy(requests),
                    "request_ids": list(request_ids),
                    # Required schema literals are inputs to this local admission test;
                    # they do not establish actor/process cleanup.
                    "cleanup": {"status": "passed", "transport_resources_closed": True, "auxiliary_fixture_stopped": True, "process_exited": True},
                }
                return self.validator.admit_case(case, replace(context, invocations=(invocation,), raw={invocation.evidence_id: raw}))

            for observed in observed_cases:
                requests = observed["request_transcript"]
                case_id = observed["id"]
                expected_status = "pending" if case_id == "installed_service_runtime" else "passed"
                with self.subTest(case_id=case_id, mutation="none"):
                    self.assertEqual(decide(observed, requests).status, expected_status)
                extra = {"method": "POST", "route": "/unrelated", "status": 500, "request_id": "extra-unrelated"}
                mutations = [("extra", requests + [extra])]
                if requests:
                    mutations.append(("missing", requests[:-1]))
                    for label, field, value in (("route", "route", "/unrelated"), ("method", "method", "POST" if requests[0]["method"] == "GET" else "GET")):
                        changed = copy.deepcopy(requests)
                        changed[0][field] = value
                        mutations.append((label, changed))
                    changed = copy.deepcopy(requests)
                    if "status" in changed[0]:
                        changed[0]["status"] = 500
                    else:
                        changed[0] = {"method": "GET", "route": "/v1/vehicles", "status": 200, "request_id": "false-outage"}
                    mutations.append(("status-or-failure", changed))
                for label, changed in mutations:
                    with self.subTest(case_id=case_id, mutation=label):
                        self.assertEqual(decide(observed, changed), self.validator.AdmissionDecision("failed", "request_mismatch"))


if __name__ == "__main__":
    unittest.main()
