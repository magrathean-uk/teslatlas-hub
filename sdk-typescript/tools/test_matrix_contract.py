from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent


def load(mode: str):
    name = f"_ts_matrix_contract_test_{mode}"
    spec = importlib.util.spec_from_file_location(name, ROOT / f"matrix_contract_{mode}.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def context_for(module):
    actor_id = module.ACTOR_ID
    artifact_sha = "b" * 64
    manifest_sha = "a" * 64
    runtime = {
        "schema_version": 1,
        "runtime_ref": module.RUNTIME_REF,
        "runtime_kind": "node-process" if module.ADAPTER_ID.endswith("node") else "browser-process",
        "platform": {"os": "macOS", "architecture": "arm64"},
        "tool_versions": {"node": "v26.7.0"} if module.ADAPTER_ID.endswith("node") else {"node": "v26.7.0", "chromium": "Chromium 152"},
        "transport": {"kind": "node-undici-diagnostics-channel" if module.ADAPTER_ID.endswith("node") else "Chromium CDP Network"},
        "artifact": {
            "role": module.ARTIFACT_ROLE,
            "sha256": artifact_sha,
            "installed_root": "/private/sdk",
            "installed_manifest_sha256": manifest_sha,
            "installed_members": [{"path": "dist/index.js", "bytes": 1, "mode": 0o644, "sha256": "c" * 64}],
        },
        "identity_sha256": "d" * 64,
    }
    actor = module.AdmittedActor(
        id=actor_id,
        kind=module.ACTOR_KIND,
        runtime_ref=module.RUNTIME_REF,
        entrypoint_ref=module.ENTRYPOINT_REF,
        artifact_roles=(module.ARTIFACT_ROLE,),
        source_roles=(module.SOURCE_ROLE,),
        installed_manifest={"path": "/private/manifest.json", "sha256": manifest_sha},
        runtime=runtime,
    )
    header = {
        "schema_version": 1,
        "execution_kind": "actual_hub_acceptance",
        "adapter": module.ADAPTER_ID,
        "cell_id": f"{module.ADAPTER_ID}__macos_arm64",
        "product_version": module.PRODUCT_VERSION,
        "profile_id": "hub-http-v1",
        "profile_revision": "1.0.0",
        "profile_sha256": module.PROFILE_SHA256,
        "source_identities": [],
        "artifacts": [
            {"role": "hub_executable", "sha256": "e" * 64},
            {"role": module.ARTIFACT_ROLE, "sha256": artifact_sha},
        ],
        "runtime": {"hub": {"service_mode": "installed-app-launchagent"}},
    }
    observations = {
        1: {
            "hub_id": "11111111-1111-4111-8111-111111111111",
            "invitations": {
                "active": {"pairing_id": "22222222-2222-4222-8222-222222222222", "expires_at_ms": 9_999_999_999_999},
                "expired": {"pairing_id": "33333333-3333-4333-8333-333333333333", "expires_at_ms": 1},
            },
            "state": "running",
        }
    }
    return module.AdmissionContext(
        adapter_id=module.ADAPTER_ID,
        cell_id=header["cell_id"],
        session_id="44444444-4444-4444-8444-444444444444",
        header=header,
        scenario={},
        actors={actor_id: actor},
        invocations=(),
        raw={},
        controller_observations=observations,
    )


class MatrixContractTests(unittest.TestCase):
    def test_manifests_are_hash_bound_and_closed(self):
        for mode in ("node", "browser"):
            module = load(mode)
            manifest = module.validate_manifest(ROOT / f"matrix-contract-{mode}.json")
            self.assertEqual(manifest["adapter_id"], module.ADAPTER_ID)
            self.assertEqual(len(manifest["required_cases"]), 21 if mode == "node" else 23)
            self.assertEqual(manifest["phases"], [])

    def test_candidate_identity_is_admitted(self):
        module = load("node")
        context = context_for(module)
        invocation = module.AdmittedInvocation(
            id="candidate-1", case_id="candidate_artifact_identity", actor_id=module.ACTOR_ID,
            operation="observe_identity", session_sequence_before=1, session_sequence_after=1,
            evidence_id="raw-1", request_ids=(),
        )
        context = module.AdmissionContext(**{**context.__dict__, "invocations": (invocation,)})
        expected = module._semantic("candidate_artifact_identity", context)
        raw = {
            "schema_version": 1, "session_id": context.session_id, "cell_id": context.cell_id,
            "session_input_sha256": "f" * 64, "actor_id": module.ACTOR_ID, "operation": "observe_identity",
            "actor_manifest_sha256": "a" * 64, "session_sequence_before": 1,
            "session_sequence_after": 1, "credential_device_id": None, "facts": expected,
            "requests": [], "cleanup": {"status": "passed", "transport_resources_closed": True, "auxiliary_fixture_stopped": True, "process_exited": True},
        }
        context = module.AdmissionContext(**{**context.__dict__, "raw": {"raw-1": raw}})
        case = {"id": "candidate_artifact_identity", "status": "passed", "expected": expected, "actual": expected, "evidence_kind": "identity", "request_transcript": []}
        self.assertEqual(module.admit_case(case, context), module.AdmissionDecision("passed", "accepted"))

    def test_installed_service_runtime_remains_runner_owned_pending(self):
        module = load("browser")
        context = context_for(module)
        invocation = module.AdmittedInvocation(
            id="runtime-1", case_id="installed_service_runtime", actor_id=module.ACTOR_ID,
            operation="observe_identity", session_sequence_before=1, session_sequence_after=1,
            evidence_id="raw-1", request_ids=(),
        )
        expected = module._semantic("installed_service_runtime", context)
        raw = {
            "schema_version": 1, "session_id": context.session_id, "cell_id": context.cell_id,
            "session_input_sha256": "f" * 64, "actor_id": module.ACTOR_ID, "operation": "observe_identity",
            "actor_manifest_sha256": "a" * 64, "session_sequence_before": 1, "session_sequence_after": 1,
            "credential_device_id": None, "facts": expected, "requests": [],
            "cleanup": {"status": "passed", "transport_resources_closed": True, "auxiliary_fixture_stopped": True, "process_exited": True},
        }
        context = module.AdmissionContext(**{**context.__dict__, "invocations": (invocation,), "raw": {"raw-1": raw}})
        case = {"id": "installed_service_runtime", "status": "pending", "expected": expected, "actual": expected, "evidence_kind": "identity", "request_transcript": []}
        self.assertEqual(module.admit_case(case, context), module.AdmissionDecision("pending", "runner_owned_service_runtime"))

    def test_browser_cors_requires_real_preflight_records(self):
        module = load("browser")
        self.assertTrue(module._request_requirement("real_browser_cors", [{
            "method": "OPTIONS", "route": "/v1/vehicles", "status": 204,
            "request_id": "req-1", "scope": "/v1/vehicles",
        }]))
        self.assertFalse(module._request_requirement("real_browser_cors", [{
            "method": "GET", "route": "/v1/vehicles", "status": 200,
            "request_id": "req-1", "scope": "/v1/vehicles",
        }]))

    def test_wrong_actor_and_wrong_request_are_rejected(self):
        module = load("node")
        context = context_for(module)
        bad = module.AdmittedActor("other", module.ACTOR_KIND, module.RUNTIME_REF, module.ENTRYPOINT_REF, (module.ARTIFACT_ROLE,), (module.SOURCE_ROLE,), context.actors[module.ACTOR_ID].installed_manifest, context.actors[module.ACTOR_ID].runtime)
        self.assertEqual(module._valid_actor(bad), "wrong_actor")
        self.assertFalse(module._request_requirement("discovery_identity_profile", []))

    def test_manifest_hash_change_is_rejected(self):
        module = load("node")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bad.json"
            value = json.loads((ROOT / "matrix-contract-node.json").read_text())
            value["validator"]["sha256"] = "0" * 64
            path.write_text(json.dumps(value))
            with self.assertRaises(ValueError):
                module.validate_manifest(path)


if __name__ == "__main__":
    unittest.main()
