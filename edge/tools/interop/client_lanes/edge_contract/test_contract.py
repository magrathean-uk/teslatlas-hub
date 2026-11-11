#!/usr/bin/env python3
"""Focused contract tests for the Edge matrix publication."""

from __future__ import annotations

import dataclasses
import importlib.util
import json
from pathlib import Path
import sys
import unittest


HERE = Path(__file__).resolve().parent


def _load_contract():
    specification = importlib.util.spec_from_file_location(
        "edge_matrix_contract", HERE / "matrix_contract.py"
    )
    if specification is None or specification.loader is None:
        raise RuntimeError("cannot load Edge contract")
    module = importlib.util.module_from_spec(specification)
    sys.modules[specification.name] = module
    specification.loader.exec_module(module)
    return module


contract = _load_contract()


class ContractTests(unittest.TestCase):
    def test_publication_has_exact_cases_actors_and_raw_schema_ids(self):
        manifest = json.loads((HERE / "matrix-contract.json").read_text())
        self.assertEqual(contract.ADAPTER_ID, "edge_v2")
        self.assertEqual(manifest["adapter_id"], contract.ADAPTER_ID)
        self.assertEqual(manifest["revision"], contract.CONTRACT_REVISION)
        self.assertEqual(tuple(manifest["required_cases"]), contract.REQUIRED_CASES)
        self.assertEqual(
            tuple(actor["id"] for actor in manifest["actors"]), contract.ACTOR_IDS
        )
        self.assertEqual(
            tuple(item["id"] for item in manifest["raw_schemas"]),
            contract.RAW_SCHEMA_IDS,
        )
        self.assertEqual(len(manifest["raw_schemas"]), 6)

    def test_context_is_immutable_and_wrong_identity_fails_closed(self):
        context = contract.AdmissionContext(
            adapter_id="edge_v2",
            cell_id="edge_v2__debian13_amd64",
            session_id="11111111-1111-4111-8111-111111111111",
            header={"product_version": "2026.36.2"},
            scenario={"schema_version": 1},
            actors={},
            invocations=(),
            raw={},
            controller_observations={},
        )
        with self.assertRaises(dataclasses.FrozenInstanceError):
            context.adapter_id = "other"
        decision = contract.admit_case(
            {"id": "candidate_artifact_identity"}, context
        )
        self.assertEqual((decision.status, decision.code), ("failed", "case_shape"))
        self.assertEqual(
            contract.admit_case(
                {"id": "candidate_artifact_identity", "status": "passed", "expected": {}, "actual": {}, "evidence_kind": "identity", "request_transcript": []},
                dataclasses.replace(context, adapter_id="other"),
            ).code,
            "wrong_context",
        )

    def test_service_runtime_can_only_be_runner_pending(self):
        case = {
            "id": "installed_service_runtime",
            "status": "pending",
            "expected": {"service_mode": "installed-deb-systemd"},
            "actual": {"service_mode": "installed-deb-systemd"},
            "evidence_kind": "identity",
            "request_transcript": [],
        }
        context = contract.synthetic_context_for_test(
            "installed_service_runtime", service_mode="installed-deb-systemd"
        )
        decision = contract.admit_case(case, context)
        self.assertEqual((decision.status, decision.code), ("pending", "runner_owned_service_runtime"))

    def test_raw_envelope_rejects_foreign_sequence_and_cleanup(self):
        context = contract.synthetic_context_for_test("edge_clean_shutdown_resume")
        case = contract.synthetic_case("edge_clean_shutdown_resume", context)
        self.assertEqual(contract.admit_case(case, context).code, "accepted")
        broken = dataclasses.replace(
            context,
            raw={
                key: {**value, "observation": {**value["observation"], "session_sequence": 999}}
                for key, value in context.raw.items()
            },
        )
        self.assertEqual(contract.admit_case(case, broken).code, "sequence_mismatch")
        dirty = dataclasses.replace(
            context,
            raw={
                key: {**value, "payload": {**value["payload"], "cleanup": {"status": "failed", "errors": ["stop"]}}}
                for key, value in context.raw.items()
            },
        )
        self.assertEqual(contract.admit_case(case, dirty).code, "cleanup_failure")

    def test_specialized_http_raw_envelope_is_admitted(self):
        context = contract.synthetic_context_for_test("edge_mtls_bearer_identity")
        payload = {
            "role": "edge_delivery",
            "transcript": [],
            "tls": {
                "server_der_sha256": "a" * 64,
                "client_der_sha256": None,
                "chain_verified": True,
                "hostname_verified": True,
            },
            "transport_failures": [],
            "census": {"v2_gets": 0, "v2_acks": 0, "v1_requests": 0, "redirect_requests": 0},
        }
        raw = {
            key: {**value, "payload": payload}
            for key, value in context.raw.items()
        }
        specialized = dataclasses.replace(context, raw=raw)
        self.assertEqual(contract.admit_case(contract.synthetic_case("edge_mtls_bearer_identity", specialized), specialized).code, "accepted")

    def test_vectors_are_closed_and_have_no_placeholder_hashes(self):
        vectors = json.loads((HERE / "edge-installed-vectors.json").read_text())
        self.assertEqual(set(vectors), {"schema_version", "stream_id", "binding", "records", "gap", "expected_checkpoints"})
        self.assertEqual(len(vectors["records"]), 10)
        self.assertEqual([row["sequence"] for row in vectors["records"]], list(range(1, 11)))
        self.assertEqual(len(vectors["expected_checkpoints"]), 6)
        for row in vectors["records"]:
            for key in ("stable_id", "legacy_id", "payload_sha256"):
                self.assertRegex(row[key], r"^[0-9a-f]{64}$")
                self.assertNotEqual(row[key], "0" * 64)

    def test_raw_schema_documents_are_parseable_and_manifest_bound(self):
        manifest = json.loads((HERE / "matrix-contract.json").read_text())
        for item in manifest["raw_schemas"]:
            schema_path = Path(item["schema"]["path"])
            schema = json.loads(schema_path.read_text())
            self.assertEqual(schema["type"], "object")
            self.assertEqual(schema["$id"].rsplit("/", 1)[-1], schema_path.name.replace("_", "-").replace(".schema", ""))


if __name__ == "__main__":
    unittest.main()
