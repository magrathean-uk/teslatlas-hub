from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker

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
