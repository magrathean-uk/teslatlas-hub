from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
import time
import unittest
from threading import Event, TIMEOUT_MAX
from unittest.mock import patch
from copy import deepcopy
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker, ValidationError

from conformance.runner import (
    AdapterSession,
    ConformanceError,
    resolve_templates,
    strict_loads,
    validate_contract_artifacts,
    validate_response,
)

from tests.support import ROOT, load_json, load_schema_registry, strict_loads as support_loads


EXPECTED_CASES = {
    "discovery-version-negotiation",
    "cursor-pagination",
    "etag-conditional-get",
    "problem-details",
    "sse-last-event-id",
    "sse-principal-visibility",
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

    def test_runner_passes_rich_and_hub_sync_profiles_through_their_local_gates(self) -> None:
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
        self.assertEqual(["1.0.0", "1.1.0", "1.2.0", "hub-sync-v1@1.3.0"], summary["profiles"])
        self.assertEqual(96, summary["runs"])
        self.assertEqual(96, summary["passed"])
        self.assertEqual(0, summary["failed"])

    def test_runner_runs_the_registered_hub_sync_fixture_gate_by_profile(self) -> None:
        runner = ROOT / "conformance" / "run"
        completed = subprocess.run(
            [str(runner), "--profile", "hub-sync-v1@1.3.0", "--json"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(0, completed.returncode, completed.stdout + completed.stderr)
        summary = json.loads(completed.stdout)
        self.assertEqual(["hub-sync-v1@1.3.0"], summary["profiles"])
        self.assertEqual((65, 65, 0), (summary["runs"], summary["passed"], summary["failed"]))

    def test_runner_rejects_default_jsonl_adapter_invocation(self) -> None:
        runner = ROOT / "conformance" / "run"
        adapter = ROOT / "tests" / "fixtures" / "nonconforming_adapter.py"
        completed = subprocess.run(
            [str(runner), "--adapter", str(adapter), "--json"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(2, completed.returncode)
        self.assertEqual(
            "JSONL adapter conformance requires explicit rich profile selections; hub-sync fixture conformance runs separately",
            json.loads(completed.stdout)["error"],
        )

    def test_runner_rejects_jsonl_adapters_for_hub_sync_fixture_conformance(self) -> None:
        runner = ROOT / "conformance" / "run"
        adapter = ROOT / "tests" / "fixtures" / "nonconforming_adapter.py"
        completed = subprocess.run(
            [
                str(runner),
                "--profile",
                "hub-sync-v1@1.3.0",
                "--adapter",
                str(adapter),
                "--json",
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(2, completed.returncode)
        self.assertEqual(
            "hub-sync fixture conformance does not use a JSONL adapter",
            json.loads(completed.stdout)["error"],
        )

    def test_runner_rejects_a_nonconforming_language_neutral_adapter(self) -> None:
        runner = ROOT / "conformance" / "run"
        adapter = ROOT / "tests" / "fixtures" / "nonconforming_adapter.py"
        self.assertTrue(runner.is_file())
        completed = subprocess.run(
            [str(runner), "--json", "--profile", "1.0.0", "--adapter", str(adapter)],
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
        self.assertEqual(9, summary["passed"])

    def test_event_visibility_is_bound_to_principal(self) -> None:
        steps = {
            step["step_id"]: step
            for step in self.load_cases()["sse-principal-visibility"]["steps"]
        }
        self.assertEqual(
            {
                "principal-a-resource",
                "principal-a-live",
                "principal-b-resource",
                "principal-b-live",
                "cross-principal-replay",
            },
            steps.keys(),
        )

        visible_resource = steps["principal-a-resource"]
        visible_live = steps["principal-a-live"]
        self.assertEqual(200, visible_resource["expect"]["status"])
        self.assertIn(
            {
                "path": "/events/0/data/data",
                "op": "same_as",
                "ref": "principal-a-resource.body",
            },
            visible_live["expect"]["assertions"],
        )

        hidden_resource = steps["principal-b-resource"]
        hidden_live = steps["principal-b-live"]
        self.assertEqual(404, hidden_resource["expect"]["status"])
        self.assertIn(
            {"path": "/events", "op": "is_empty"},
            hidden_live["expect"]["assertions"],
        )

        replay = steps["cross-principal-replay"]
        self.assertEqual(
            "${principal-a-live.events.0.id}",
            replay["request"]["headers"]["Last-Event-ID"],
        )
        self.assertEqual(400, replay["expect"]["status"])
        self.assertIn(
            {"path": "/body/code", "op": "equals", "value": "event_id_invalid"},
            replay["expect"]["assertions"],
        )
        self.assertEqual(
            "principal-b",
            replay["request"]["headers"]["Teslatlas-Conformance-Principal"],
        )

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

    def test_cursor_traversal_rejects_changed_snapshot_through_real_adapter(self) -> None:
        reference = (ROOT / "conformance/adapters/reference_adapter.py").read_text()
        # Retain the real adapter's loading, correlation and responses, changing
        # only the snapshot on the followed cursor page.
        mutation = (
            '    if envelope["case_id"] == "cursor-pagination" and envelope["step_id"] == "next":\n'
            '        response["body"]["snapshot_revision"] = "snapshot_demo_different"\n'
        )
        reference = reference.replace(
            'ROOT = Path(__file__).resolve().parents[2]', f'ROOT = Path({str(ROOT)!r})'
        )
        with tempfile.TemporaryDirectory() as directory:
            adapter = Path(directory) / "changed_snapshot.py"
            adapter.write_text(reference.replace("    print(\n", mutation + "    print(\n"))
            for profile in ("1.0.0", "1.1.0", "1.2.0"):
                with self.subTest(profile=profile):
                    command = [str(ROOT / "conformance/run"), "--profile", profile,
                               "--adapter", str(adapter), "--json"]
                    result = subprocess.run(command, cwd=ROOT, capture_output=True,
                                            text=True, timeout=60)
                    self.assertEqual(1, result.returncode, result.stdout + result.stderr)
                    self.assertIn("expected same value as initial.body.snapshot_revision", result.stdout)

    def test_cursor_traversal_accepts_reference_adapter_in_all_rich_profiles(self) -> None:
        result = subprocess.run(
            [str(ROOT / "conformance/run"), "--profile", "1.0.0", "--profile", "1.1.0",
             "--profile", "1.2.0", "--adapter", str(ROOT / "conformance/adapters/reference_adapter.py"),
             "--json"], cwd=ROOT, capture_output=True, text=True, timeout=60,
        )
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertEqual("adapter-contract", json.loads(result.stdout)["provenance"])

    @unittest.skipUnless(os.name == "posix", "owned process-group deadline requires POSIX")
    def test_actual_hub_branch_enforces_total_process_deadline(self) -> None:
        from conformance import runner
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            adapter = root / "conformance/adapters/actual-hub"
            adapter.parent.mkdir(parents=True)
            # A real child that waits for a signal, requiring no network/config
            # and no timed race to demonstrate the outer-process deadline.
            adapter.write_text(f"#!{sys.executable}\nimport signal\nsignal.pause()\n")
            adapter.chmod(0o700)
            argv = ["runner", "--profile", "hub-http-v1@1.1.0", "--adapter", str(adapter),
                    "--timeout-seconds", "0.1"]
            started = time.monotonic()
            with patch.object(runner, "ROOT", root), patch.object(sys, "argv", argv):
                with self.assertRaisesRegex(ConformanceError, "total process deadline"):
                    runner.run()
            self.assertLess(time.monotonic() - started, 2.0)
            adapter.write_text(f"#!{sys.executable}\nraise SystemExit(0)\n")
            with patch.object(runner, "ROOT", root), patch.object(sys, "argv", argv):
                self.assertEqual(0, runner.run())

    @unittest.skipUnless(os.name == "posix", "owned process-group cleanup requires POSIX")
    def test_actual_hub_cleans_ready_descendant_after_leader_exits(self) -> None:
        from conformance.runner import run_actual_hub
        import signal
        child_source = (
            "import signal,sys; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            "sys.stdout.write('ready'); sys.stdout.flush(); signal.pause()"
        )
        with tempfile.TemporaryDirectory() as directory:
            pid_file = Path(directory) / "owned-child.pid"
            for exit_status in (0, 3):
                with self.subTest(exit_status=exit_status):
                    leader_source = (
                        "import subprocess,sys; from pathlib import Path; "
                        f"child=subprocess.Popen([sys.executable,'-c',{child_source!r}],stdout=subprocess.PIPE); "
                        "assert child.stdout.read(5)==b'ready'; "
                        f"Path({str(pid_file)!r}).write_text(str(child.pid)); "
                        f"raise SystemExit({exit_status})"
                    )
                    try:
                        self.assertEqual(exit_status, run_actual_hub([sys.executable, "-c", leader_source], 2))
                        child_pid = int(pid_file.read_text())
                        deadline = time.monotonic() + 2
                        while True:
                            try:
                                os.kill(child_pid, 0)
                            except ProcessLookupError:
                                break
                            if time.monotonic() >= deadline:
                                self.fail("owned descendant remained after leader settlement")
                            Event().wait(0.01)
                    finally:
                        if pid_file.exists():
                            try:
                                os.kill(int(pid_file.read_text()), signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                            pid_file.unlink()

    def test_metadata_etags_preserve_transport_safe_exported_values(self) -> None:
        request = {"method": "GET", "path": "/v1/metadata/metadata_demo_note_0001"}
        expectation = {"status": 200, "headers_present": ["ETag"]}
        for tag in ('"with space"', '"with\\backslash"', '"ordinary"'):
            with self.subTest(tag=tag):
                self.assertEqual([], validate_response(
                    {"status": 200, "headers": {"ETag": tag}}, request,
                    expectation, "1.2.0", {}, self.schemas, self.registry,
                ))
        for tag in ('W/"weak"', '"in\tjection"', '"in\r\njection"', '"nul\x00"', '"del\x7f"', '"inner"quote"'):
            with self.subTest(tag=tag):
                self.assertIn("strong ETag", "\n".join(validate_response(
                    {"status": 200, "headers": {"ETag": tag}}, request,
                    expectation, "1.2.0", {}, self.schemas, self.registry,
                )))

    def test_rich_json_rejects_float_overflow_without_rounding_integers(self) -> None:
        integer = 10 ** 100 + 123
        for parser in (strict_loads, support_loads):
            with self.subTest(parser=parser.__module__):
                self.assertEqual(integer, parser(str(integer)))
                self.assertEqual(-integer, parser(str(-integer)))
                self.assertEqual(12.5, parser("1.25e1"))
                self.assertEqual({"value": 2}, parser('{"value":1,"value":2}'))
                for value in ("1e309", "-1e309"):
                    with self.assertRaisesRegex(ValueError, "unsupported non-finite numeric representation"):
                        parser(value)
                for value in ("NaN", "Infinity", "-Infinity"):
                    with self.assertRaisesRegex(ValueError, "non-finite JSON constant"):
                        parser(value)
        completed = subprocess.run(
            [sys.executable, str(ROOT / "conformance/adapters/reference_adapter.py")],
            input='{"probe":1e309}\n', capture_output=True, text=True,
            check=False, timeout=3,
        )
        self.assertNotEqual(0, completed.returncode)
        self.assertIn("unsupported non-finite numeric representation", completed.stderr)

    def test_metadata_page_applies_each_record_audit_semantics(self) -> None:
        page = {
            "resource_type": "metadata_page", "items": [load_json("examples/metadata-record.json")],
            "next_cursor": None, "generated_at": "2026-08-31T12:31:00.000Z",
            "snapshot_revision": "snapshot_demo_0042",
        }
        request = {"method": "GET", "path": "/v1/vehicles/vehicle_demo_alpha/metadata",
                   "headers": {"Teslatlas-Protocol-Version": "1.2.0"}}
        expectation = {"status": 200, "body_schema": "urn:teslatlas:protocol:schema:resources:1.2.0#/$defs/metadata_page"}
        headers = {"Content-Type": "application/json", "ETag": '"page"',
                   "Cache-Control": "private", "Vary": "Authorization",
                   "Teslatlas-Protocol-Version": "1.2.0"}
        def errors(body: dict[str, Any]) -> list[str]:
            return validate_response({"status": 200, "headers": headers, "body": body},
                                     request, expectation, "1.2.0", {}, self.schemas, self.registry)
        self.assertEqual([], errors(page))
        for field, value in (("revision", 99), ("updated_by", "user_other"),
                             ("updated_at", "2026-08-31T12:40:00.000Z")):
            invalid = deepcopy(page)
            invalid["items"][0][field] = value
            with self.subTest(field=field):
                self.assertIn(f"metadata {field} must match", "\n".join(errors(invalid)))
                self.assertIn("metadata/items/0", "\n".join(errors(invalid)))

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

    def test_adapter_stdin_write_uses_the_request_deadline(self) -> None:
        session = AdapterSession([sys.executable, "-c", "import time; time.sleep(5)"], 0.05)
        started = time.monotonic()
        try:
            with self.assertRaisesRegex(ConformanceError, "stdin write timed out"):
                session.request({"payload": "x" * 262144})
            self.assertLess(time.monotonic() - started, 1.5)
            self.assertIsNotNone(session.process.poll())
        finally:
            session.finish()

    def test_adapter_stdout_flood_has_bounded_pending_storage(self) -> None:
        command = "import sys,time; sys.stdin.buffer.readline(); sys.stdout.write('1\\n'*256); sys.stdout.flush(); time.sleep(5)"
        session = AdapterSession([sys.executable, "-c", command], 0.5)
        try:
            try:
                session.request({"probe": True})
            except ConformanceError as error:
                self.assertIn("extra stdout", str(error))
            session.stdout_thread.join(timeout=0.2)
            self.assertLessEqual(session.stdout_lines.qsize(), 2)
        finally:
            finish_error = session.finish()
        self.assertIn("extra stdout", finish_error or "")

    def test_adapter_delayed_partial_writer_cannot_use_recycled_stdin_fd(self) -> None:
        session = AdapterSession([sys.executable, "-c", "import time; time.sleep(5)"], 0.03)
        original_fd = session.process.stdin.fileno()
        paused, released = Event(), Event()
        real_write = os.write
        first = True
        opened: set[int] = set()

        def partial_write(descriptor: int, payload: Any) -> int:
            nonlocal first
            if first:
                first = False
                count = real_write(descriptor, payload[:1])
                paused.set()
                if not released.wait(2):
                    raise OSError("bounded regression release did not arrive")
                return count
            return real_write(descriptor, payload)

        try:
            with tempfile.TemporaryDirectory() as directory:
                target = Path(directory) / "owned-reused-fd"
                with patch("conformance.runner.os.write", side_effect=partial_write):
                    with self.assertRaisesRegex(ConformanceError, "stdin write timed out"):
                        session.request({"probe": "delayed-partial-write"})
                    self.assertTrue(paused.is_set())
                    self.assertTrue(session.process.stdin.closed)
                    self.assertTrue(session.write_thread.is_alive())
                    descriptor = os.open(target, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, 0o600)
                    opened.add(descriptor)
                    if descriptor != original_fd:
                        os.dup2(descriptor, original_fd)
                        opened.add(original_fd)
                    released.set()
                    session.write_thread.join(timeout=1)
                    self.assertFalse(session.write_thread.is_alive())
                self.assertEqual(b"", target.read_bytes(), "delayed write must retain its own pipe identity")
        finally:
            released.set()
            if session.write_thread is not None:
                session.write_thread.join(timeout=1)
            session.finish()
            for descriptor in opened:
                os.close(descriptor)

    def test_adapter_stdout_frame_budget_counts_utf8_bytes(self) -> None:
        command = "import sys; sys.stdin.buffer.readline(); sys.stdout.buffer.write(('é'*32+'\\n').encode('utf-8')); sys.stdout.buffer.flush()"
        session = AdapterSession([sys.executable, "-c", command], 0.5, max_frame_bytes=64)
        try:
            with self.assertRaisesRegex(ConformanceError, "stdout frame exceeds"):
                session.request({"probe": True})
        finally:
            self.assertIn("stdout frame exceeds", session.finish() or "")

    def test_adapter_writer_launch_failure_closes_owned_descriptor(self) -> None:
        session = AdapterSession([sys.executable, "-c", "import time; time.sleep(5)"], 0.03)
        duplicated: list[int] = []
        real_dup = os.dup
        def duplicate(descriptor: int) -> int:
            result = real_dup(descriptor)
            duplicated.append(result)
            return result
        try:
            with patch("conformance.runner.os.dup", side_effect=duplicate), patch(
                "conformance.runner.Thread.start", side_effect=RuntimeError("synthetic launch refusal"),
            ):
                with self.assertRaisesRegex(ConformanceError, "writer could not start"):
                    session.request({"probe": True})
            self.assertEqual(1, len(duplicated))
            with self.assertRaises(OSError):
                os.fstat(duplicated[0])
        finally:
            self.assertIn("did not exit", session.finish() or "")
            self.assertIsNotNone(session.process.poll())

    def test_adapter_multiple_steps_and_invalid_local_budgets(self) -> None:
        command = "import sys\nfor line in sys.stdin.buffer:\n sys.stdout.buffer.write(line); sys.stdout.buffer.flush()\n"
        session = AdapterSession([sys.executable, "-c", command], 1, max_frame_bytes=128)
        try:
            for value in range(3):
                self.assertEqual({"step": value}, json.loads(session.request({"step": value})))
            with self.assertRaisesRegex(ConformanceError, "stdin frame exceeds"):
                session.request({"payload": "x" * 128})
        finally:
            self.assertIsNone(session.finish())
        for timeout in (float("nan"), float("inf"), 0):
            with self.assertRaisesRegex(ConformanceError, "finite and positive"):
                AdapterSession([sys.executable, "-c", "pass"], timeout)
        with self.assertRaisesRegex(ConformanceError, "platform wait limit"):
            AdapterSession([sys.executable, "-c", "pass"], TIMEOUT_MAX * 2)
        for budget in (True, 0, sys.maxsize):
            with self.assertRaisesRegex(ConformanceError, "frame budget"):
                AdapterSession([sys.executable, "-c", "pass"], 1, max_frame_bytes=budget)

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
