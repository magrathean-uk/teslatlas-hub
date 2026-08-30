from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import time
import unittest
from copy import deepcopy
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker, ValidationError

from conformance.runner import (
    AdapterSession,
    ConformanceError,
    resolve_templates,
    validate_contract_artifacts,
    validate_response,
)

from tests.support import ROOT, load_json, load_schema_registry


EXPECTED_CASES = {
    "discovery-version-negotiation",
    "cursor-pagination",
    "etag-conditional-get",
    "problem-details",
    "sse-last-event-id",
    "sse-empty-id-reset",
    "sse-terminal-204",
    "documented-limits",
    "command-idempotency",
    "metadata-if-match",
    "deprecation-sunset",
}


def semver_tuple(value: str) -> tuple[int, int, int]:
    return tuple(int(part) for part in value.split("."))  # type: ignore[return-value]


class ConformanceContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.schemas, cls.registry = load_schema_registry()

    def conformance_validator(self) -> Draft202012Validator:
        schema_id = "urn:teslatlas:protocol:schema:conformance:1.2.0"
        self.assertIn(schema_id, self.schemas)
        return Draft202012Validator(
            self.schemas[schema_id],
            registry=self.registry,
            format_checker=FormatChecker(),
        )

    def load_cases(self) -> dict[str, dict[str, Any]]:
        case_dir = ROOT / "conformance" / "cases"
        self.assertTrue(case_dir.is_dir())
        cases = {
            item["case_id"]: item
            for path in sorted(case_dir.glob("*.json"))
            for item in [json.loads(path.read_text(encoding="utf-8"))]
        }
        self.assertEqual(EXPECTED_CASES, cases.keys())
        return cases

    def test_manifest_profiles_and_cases_validate_against_neutral_schema(self) -> None:
        validator = self.conformance_validator()
        manifest = load_json("compatibility/manifest.json")
        validator.validate(manifest)
        cases = self.load_cases()
        for case in cases.values():
            validator.validate(case)
        for entry in manifest["profiles"]:
            validator.validate(load_json(entry["path"]))

    def test_current_profile_covers_exactly_previous_two_minor_versions(self) -> None:
        manifest = load_json("compatibility/manifest.json")
        self.assertEqual("1.2.0", manifest["current_version"])
        self.assertEqual(["1.0.0", "1.1.0", "1.2.0"], manifest["supported_profiles"])
        current = semver_tuple(manifest["current_version"])
        actual = [semver_tuple(item) for item in manifest["supported_profiles"]]
        self.assertEqual(
            [(current[0], current[1] - 2, 0), (current[0], current[1] - 1, 0), current],
            actual,
        )

    def test_each_profile_runs_every_case_available_in_that_minor(self) -> None:
        manifest = load_json("compatibility/manifest.json")
        cases = self.load_cases()
        for entry in manifest["profiles"]:
            profile = load_json(entry["path"])
            version = semver_tuple(profile["protocol_version"])
            expected = {
                case_id
                for case_id, case in cases.items()
                if semver_tuple(case["introduced_in"]) <= version
            }
            self.assertEqual(expected, set(profile["cases"]), profile["protocol_version"])

    def test_profiles_preserve_capabilities_and_extend_the_previous_minor(self) -> None:
        manifest = load_json("compatibility/manifest.json")
        cases = self.load_cases()
        profiles = {
            entry["version"]: load_json(entry["path"])
            for entry in manifest["profiles"]
        }

        missing_capability = deepcopy(profiles)
        missing_capability["1.0.0"]["capabilities"].remove("query.vehicles")
        with self.assertRaises(ConformanceError):
            validate_contract_artifacts(
                manifest, missing_capability, cases, self.schemas, self.registry
            )

        broken_extension = deepcopy(profiles)
        broken_extension["1.1.0"]["extends"] = None
        with self.assertRaises(ConformanceError):
            validate_contract_artifacts(
                manifest, broken_extension, cases, self.schemas, self.registry
            )

    def test_reference_vectors_cover_http_headers_confirmation_audit_and_tombstones(self) -> None:
        cases = self.load_cases()
        required_get_headers = {
            "Content-Type",
            "ETag",
            "Cache-Control",
            "Vary",
        }
        etags: list[str] = []
        for case in cases.values():
            for step in case["steps"]:
                response = step["reference_response"]
                headers = response["headers"]
                if (
                    step["request"]["method"] == "GET"
                    and response["status"] == 200
                    and "body_schema" in step["expect"]
                ):
                    self.assertFalse(required_get_headers - headers.keys(), step["step_id"])
                    if step["request"]["path"].startswith("/v1/"):
                        self.assertIn("Teslatlas-Protocol-Version", headers)
                if response["status"] == 304:
                    self.assertFalse(
                        {"ETag", "Cache-Control", "Vary"} - headers.keys(),
                        step["step_id"],
                    )
                if "ETag" in headers:
                    etags.append(headers["ETag"])

        self.assertTrue(etags)
        for etag in etags:
            self.assertIsNone(re.search(r"(?:^|[-_])r[0-9]+(?:[-_]|\"|$)", etag))

        command_steps = {
            step["step_id"]: step for step in cases["command-idempotency"]["steps"]
        }
        self.assertEqual(400, command_steps["missing-confirmation"]["expect"]["status"])

        discovery_steps = {
            step["step_id"]: step
            for step in cases["discovery-version-negotiation"]["steps"]
        }
        omitted = discovery_steps["omitted-version-selects-minimum"]
        self.assertNotIn("Teslatlas-Protocol-Version", omitted["request"]["headers"])
        self.assertEqual(
            "${discover.body.protocol.minimum_client_version}",
            omitted["expect"]["headers_equal"]["Teslatlas-Protocol-Version"],
        )
        resolved = resolve_templates(
            omitted["expect"],
            "1.2.0",
            {
                "discover": {
                    "body": {
                        "protocol": {
                            "minimum_client_version": "1.1.0",
                        }
                    }
                }
            },
        )
        self.assertEqual(
            "1.1.0", resolved["headers_equal"]["Teslatlas-Protocol-Version"]
        )

        metadata_steps = {
            step["step_id"]: step for step in cases["metadata-if-match"]["steps"]
        }
        self.assertTrue(
            {"delete", "get-tombstone", "list-excludes-tombstone", "event-tombstone"}
            <= metadata_steps.keys()
        )
        update_assertions = metadata_steps["update"]["expect"]["assertions"]
        self.assertTrue(
            any(item["path"] == "/body/audit/1/action" for item in update_assertions)
        )

        sse_case = cases["sse-last-event-id"]
        self.assertEqual("1.0.0", sse_case["introduced_in"])
        v1_event = sse_case["steps"][0]["reference_response"]["events"][0]["data"]
        self.assertEqual(
            {
                "event_id",
                "event_type",
                "occurred_at",
                "vehicle_id",
                "resource_id",
                "revision",
                "data",
            },
            set(v1_event),
        )
        for version in ("1.0.0", "1.1.0", "1.2.0"):
            profile = load_json(f"compatibility/{version}/profile.json")
            self.assertIn("sse-last-event-id", profile["cases"])

        deprecated = cases["deprecation-sunset"]
        discovery = deprecated["steps"][0]["reference_response"]["body"]
        self.assertEqual("query.vehicles", discovery["capabilities"][0]["id"])
        self.assertEqual("deprecated", discovery["capabilities"][0]["status"])
        self.assertEqual(
            discovery["capabilities"][0]["href"],
            deprecated["steps"][1]["request"]["path"],
        )

    def test_runner_passes_all_three_profiles_through_reference_adapter(self) -> None:
        runner = ROOT / "conformance" / "run"
        self.assertTrue(runner.is_file())
        self.assertTrue(runner.stat().st_mode & 0o111)
        completed = subprocess.run(
            [str(runner), "--json"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, completed.returncode, completed.stdout + completed.stderr)
        summary = json.loads(completed.stdout)
        self.assertEqual(["1.0.0", "1.1.0", "1.2.0"], summary["profiles"])
        self.assertEqual(28, summary["runs"])
        self.assertEqual(28, summary["passed"])
        self.assertEqual(0, summary["failed"])

    def test_runner_rejects_a_nonconforming_language_neutral_adapter(self) -> None:
        runner = ROOT / "conformance" / "run"
        adapter = ROOT / "tests" / "fixtures" / "nonconforming_adapter.py"
        self.assertTrue(runner.is_file())
        completed = subprocess.run(
            [str(runner), "--json", "--adapter", str(adapter)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(0, completed.returncode)
        summary = json.loads(completed.stdout)
        self.assertGreater(summary["failed"], 0)
        self.assertLess(summary["passed"], summary["runs"])

    def run_fixture_adapter(
        self,
        name: str,
        *extra: str,
        timeout: float = 8,
    ) -> subprocess.CompletedProcess[str]:
        runner = ROOT / "conformance" / "run"
        adapter = ROOT / "tests" / "fixtures" / name
        return subprocess.run(
            [
                str(runner),
                "--json",
                "--profile",
                "1.0.0",
                "--adapter",
                str(adapter),
                *extra,
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
            timeout=timeout,
        )

    def test_malformed_adapter_envelope_is_a_structured_failure(self) -> None:
        completed = self.run_fixture_adapter("malformed_adapter.py")
        self.assertNotEqual(0, completed.returncode)
        summary = json.loads(completed.stdout)
        self.assertGreater(summary["failed"], 0)
        self.assertIn("adapter response schema", json.dumps(summary["failures"]))
        self.assertNotIn("Traceback", completed.stderr)

    def test_runner_rejects_non_finite_adapter_json(self) -> None:
        completed = self.run_fixture_adapter("nan_adapter.py")
        self.assertNotEqual(0, completed.returncode)
        summary = json.loads(completed.stdout)
        self.assertIn("non-finite JSON constant", json.dumps(summary["failures"]))

    def test_adapter_line_timeout_is_bounded(self) -> None:
        completed = self.run_fixture_adapter(
            "hanging_adapter.py", "--timeout-seconds", "0.1", timeout=5
        )
        self.assertNotEqual(0, completed.returncode)
        summary = json.loads(completed.stdout)
        self.assertIn("timed out", json.dumps(summary["failures"]))

    def test_stderr_flood_does_not_deadlock(self) -> None:
        completed = self.run_fixture_adapter("stderr_flood_adapter.py", timeout=5)
        self.assertNotEqual(0, completed.returncode)
        self.assertGreater(json.loads(completed.stdout)["failed"], 0)

    def test_adapter_process_is_isolated_per_case(self) -> None:
        completed = self.run_fixture_adapter("isolation_adapter.py")
        self.assertEqual(0, completed.returncode, completed.stdout + completed.stderr)
        summary = json.loads(completed.stdout)
        self.assertEqual(8, summary["passed"])

    def test_metadata_entity_etags_must_be_strong(self) -> None:
        expectation = {"status": 200, "headers_present": ["ETag"]}
        response = {"status": 200, "headers": {"ETag": 'W/"weak"'}}
        requests = [
            {"method": "GET", "path": "/v1/metadata/metadata_demo_note_0001"},
            {"method": "PUT", "path": "/v1/metadata/metadata_demo_note_0001"},
            {"method": "DELETE", "path": "/v1/metadata/metadata_demo_note_0001"},
            {
                "method": "POST",
                "path": "/v1/vehicles/vehicle_demo_alpha/metadata",
            },
        ]
        for request in requests:
            with self.subTest(method=request["method"]):
                errors = validate_response(
                    response,
                    request,
                    expectation,
                    "1.2.0",
                    {},
                    self.schemas,
                    self.registry,
                )
                self.assertIn("strong ETag", "\n".join(errors))

    def test_event_types_are_gated_by_the_negotiated_profile(self) -> None:
        event_schema = "urn:teslatlas:protocol:schema:event:1.2.0"
        command = load_json("examples/command-job.json")
        metadata = load_json("examples/metadata-record.json")

        def errors_for(profile: str, event_type: str, payload: dict[str, Any]) -> list[str]:
            resource_id = (
                payload["command_id"]
                if event_type == "command.changed"
                else payload["metadata_id"]
            )
            envelope = {
                "event_id": f"event_demo_{event_type.replace('.', '_')}",
                "event_type": event_type,
                "occurred_at": "2026-08-30T12:50:00.000Z",
                "vehicle_id": payload["vehicle_id"],
                "resource_id": resource_id,
                "revision": payload.get("revision", 1),
                "data": payload,
            }
            return validate_response(
                {
                    "status": 200,
                    "headers": {},
                    "events": [
                        {
                            "id": envelope["event_id"],
                            "event": event_type,
                            "data": envelope,
                        }
                    ],
                },
                {"method": "GET", "path": "/v1/events"},
                {"status": 200, "event_schema": event_schema},
                profile,
                {},
                self.schemas,
                self.registry,
            )

        self.assertIn(
            "not available in profile 1.0.0",
            "\n".join(errors_for("1.0.0", "command.changed", command)),
        )
        self.assertNotIn(
            "not available",
            "\n".join(errors_for("1.1.0", "command.changed", command)),
        )
        self.assertIn(
            "not available in profile 1.1.0",
            "\n".join(errors_for("1.1.0", "metadata.changed", metadata)),
        )
        self.assertNotIn(
            "not available",
            "\n".join(errors_for("1.2.0", "metadata.changed", metadata)),
        )

    def test_adapter_session_rejects_extra_stdout_after_final_response(self) -> None:
        adapter = ROOT / "tests" / "fixtures" / "extra_final_stdout_adapter.py"
        session = AdapterSession([sys.executable, str(adapter)], 1)
        try:
            response = session.request({"probe": True})
            self.assertEqual({"probe": True}, json.loads(response))
        finally:
            finish_error = session.finish()
        self.assertIsNotNone(finish_error)
        self.assertIn("extra stdout", finish_error or "")

    @unittest.skipUnless(os.name == "posix", "process-group cleanup is POSIX-specific")
    def test_adapter_timeout_kills_descendants(self) -> None:
        adapter = ROOT / "tests" / "fixtures" / "descendant_adapter.py"
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "descendant-survived"
            session = AdapterSession(
                [sys.executable, str(adapter), str(marker)], 0.05
            )
            try:
                with self.assertRaises(ConformanceError):
                    session.request({"probe": True})
            finally:
                session.finish()
            time.sleep(0.6)
            self.assertFalse(marker.exists())

    def test_body_file_path_traversal_is_rejected_by_contract_schema(self) -> None:
        case = deepcopy(next(iter(self.load_cases().values())))
        case["steps"][0]["reference_response"] = {
            "status": 200,
            "headers": {},
            "body_file": "examples/../../AGENTS.md",
        }
        with self.assertRaises(ValidationError):
            self.conformance_validator().validate(case)

    def test_conformance_generator_is_byte_deterministic(self) -> None:
        script = ROOT / "tools" / "build_conformance.py"
        self.assertTrue(script.is_file())
        completed = subprocess.run(
            [sys.executable, str(script), "--check"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, completed.returncode, completed.stdout + completed.stderr)


if __name__ == "__main__":
    unittest.main()
