from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
import tempfile
import unittest
from decimal import Decimal
from pathlib import Path
from typing import Any, Iterator
from unittest.mock import patch

from jsonschema import Draft202012Validator, FormatChecker

from tests.support import ROOT, load_json, load_schema_registry
from tools import build_fixtures


SCENARIOS = {"normal", "duplicate", "delayed", "missing", "reordered"}
FORBIDDEN_TEXT = (
    "access_token",
    "refresh_token",
    "api_key",
    "private_key",
    "bearer ",
)
EMAIL_PATTERN = re.compile(r"\b[^\s@]+@[^\s@]+\.[^\s@]+\b")
VIN_PATTERN = re.compile(r"\b[A-HJ-NPR-Z0-9]{17}\b")


def sequence_key(value: Any) -> tuple[int, Any]:
    if type(value) is int:
        return (0, value)
    if isinstance(value, str):
        return (1, value.encode("utf-8"))
    if value is None:
        return (2, b"")
    raise AssertionError(f"unexpected source sequence: {value!r}")


def canonical_key(observation: dict[str, Any]) -> tuple[Any, ...]:
    return (
        observation["provider_timestamp"],
        observation["source"].lower().encode("ascii"),
        sequence_key(observation["source_sequence"]),
        observation["observation_id"].lower().encode("ascii"),
    )


def walk(value: Any, path: tuple[str, ...] = ()) -> Iterator[tuple[tuple[str, ...], Any]]:
    yield path, value
    if isinstance(value, dict):
        for key, item in value.items():
            yield from walk(item, (*path, key))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from walk(item, (*path, str(index)))


class FixtureContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.schemas, cls.registry = load_schema_registry()
        cls.validator = Draft202012Validator(
            cls.schemas["urn:teslatlas:protocol:schema:fixture:1.2.0"],
            registry=cls.registry,
            format_checker=FormatChecker(),
        )

    def fixtures(self) -> list[dict[str, Any]]:
        manifest = load_json("fixtures/manifest.json")
        self.assertEqual(SCENARIOS, {entry["scenario_id"] for entry in manifest["fixtures"]})
        loaded = []
        for entry in manifest["fixtures"]:
            path = ROOT / entry["path"]
            self.assertTrue(path.is_file(), entry["path"])
            payload = path.read_bytes()
            self.assertEqual(entry["sha256"], hashlib.sha256(payload).hexdigest())
            fixture = json.loads(payload)
            self.validator.validate(fixture)
            loaded.append(fixture)
        return loaded

    def test_normal_duplicate_delayed_missing_and_reordered_fixtures_validate(self) -> None:
        fixtures = self.fixtures()
        by_id = {fixture["scenario_id"]: fixture for fixture in fixtures}

        self.assertEqual("complete", by_id["normal"]["expected"]["quality"]["quality"])
        self.assertTrue(by_id["duplicate"]["expected"]["duplicate_observation_ids"])
        self.assertTrue(
            any(
                "delayed" in observation["quality_flags"]
                for observation in by_id["delayed"]["input_observations"]
            )
        )
        self.assertGreater(by_id["missing"]["expected"]["quality"]["gap_count"], 0)
        reordered = by_id["reordered"]
        self.assertNotEqual(
            [item["observation_id"] for item in reordered["input_observations"]],
            reordered["expected"]["ordered_observation_ids"],
        )

    def test_expected_projection_is_ordered_and_carries_exact_quality(self) -> None:
        for fixture in self.fixtures():
            with self.subTest(scenario=fixture["scenario_id"]):
                expected = fixture["expected"]
                by_id = {
                    observation["observation_id"]: observation
                    for observation in fixture["input_observations"]
                }
                canonical = sorted(
                    expected["accepted_observation_ids"],
                    key=lambda item: canonical_key(by_id[item]),
                )
                self.assertEqual(canonical, expected["ordered_observation_ids"])
                self.assertEqual(
                    canonical,
                    expected["projection"]["input_observation_ids"],
                )
                self.assertEqual(expected["quality"], expected["projection"]["quality"])

    def test_reordered_fixture_exercises_every_canonical_tie_break(self) -> None:
        reordered = next(
            fixture for fixture in self.fixtures() if fixture["scenario_id"] == "reordered"
        )
        observations = reordered["input_observations"]
        self.assertEqual(1, len({item["provider_timestamp"] for item in observations}))
        self.assertGreaterEqual(len({item["source"] for item in observations}), 2)
        self.assertEqual(
            {int, str, type(None)},
            {type(item["source_sequence"]) for item in observations},
        )
        expected = sorted(observations, key=canonical_key)
        self.assertEqual(
            [item["observation_id"] for item in expected],
            reordered["expected"]["ordered_observation_ids"],
        )

    def test_fixtures_contain_no_secrets_vins_real_ids_or_precise_locations(self) -> None:
        for fixture in self.fixtures():
            with self.subTest(scenario=fixture["scenario_id"]):
                rendered = json.dumps(fixture, sort_keys=True).lower()
                for forbidden in FORBIDDEN_TEXT:
                    self.assertNotIn(forbidden, rendered)
                self.assertIsNone(EMAIL_PATTERN.search(rendered))
                self.assertIsNone(VIN_PATTERN.search(rendered.upper()))
                self.assertEqual(
                    {
                        "contains_real_identifiers": False,
                        "contains_secrets": False,
                        "contains_precise_location": False,
                        "maximum_coordinate_decimals": 2,
                    },
                    fixture["redaction"],
                )
                for path, value in walk(fixture):
                    if path and path[-1] in {"latitude", "longitude"} and isinstance(value, (int, float)):
                        decimals = max(0, -Decimal(str(value)).as_tuple().exponent)
                        self.assertLessEqual(decimals, 2, ".".join(path))

    def test_fixture_output_check_rejects_prohibited_private_data(self) -> None:
        prohibited_values = {
            "access token": {"access_token": "access-secret-123"},
            "account identifier": {"account_id": "account-real-123"},
            "account identifier alias": {"account_identifier": "account-real-123"},
            "API key": {"api_key": "api-secret-123"},
            "auth token": {"auth_token": "auth-secret-123"},
            "client secret": {"client_secret": "client-secret-123"},
            "credential": {"credential": "credential-secret-123"},
            "credentials": {"credentials": {"opaque": "secret-123"}},
            "password": {"password": "password-secret-123"},
            "private key": {"private_key": "private-secret-123"},
            "provider payload": {"provider_payload": {"state": "online"}},
            "provider token": {"provider_token": "provider-secret-123"},
            "raw payload": {"raw_payload": {"state": "online"}},
            "session token": {"session_token": "session-secret-123"},
            "secret": {"secret": "secret-123"},
            "token": {"token": "token-secret-123"},
            "raw provider payload": {"raw_provider_payload": {"state": "online"}},
            "refresh token": {"refresh_token": "refresh-secret-123"},
            "VIN key": {"vin": "synthetic-redacted"},
            "email address": {"contact": "driver@example.net"},
            "bearer credential": {"authorization": "Bearer secret-token"},
            "real location label": {"location": {"label": "home"}},
            "precise coordinate": {"location": {"latitude": 51.507351}},
            "VIN": {"vehicle": "5YJ3E1EA7KF000001"},
        }
        for name, value in prohibited_values.items():
            with self.subTest(name=name):
                with tempfile.TemporaryDirectory() as temporary_directory:
                    fixture_dir = Path(temporary_directory)
                    with patch.object(build_fixtures, "FIXTURE_DIR", fixture_dir):
                        path = fixture_dir / f"{name.replace(' ', '-')}.json"
                        payload = (json.dumps(value, sort_keys=True) + "\n").encode("utf-8")
                        path.write_bytes(payload)
                        errors = build_fixtures.check_outputs({path: payload})
                        self.assertTrue(
                            any("prohibited fixture data" in error for error in errors),
                            errors,
                        )

    def test_fixture_output_check_reports_invalid_json(self) -> None:
        invalid_payloads = {
            "duplicate-key": b'{"latitude": 51.507351, "latitude": 51.5}\n',
            "malformed": b'{"broken": }\n',
            "non-finite": b'{"location": {"latitude": NaN}}\n',
        }
        for name, payload in invalid_payloads.items():
            with self.subTest(name=name):
                with tempfile.TemporaryDirectory() as temporary_directory:
                    fixture_dir = Path(temporary_directory)
                    with patch.object(build_fixtures, "FIXTURE_DIR", fixture_dir):
                        path = fixture_dir / f"{name}.json"
                        path.write_bytes(payload)
                        try:
                            errors = build_fixtures.check_outputs({path: payload})
                        except (json.JSONDecodeError, TypeError, ValueError) as error:
                            self.fail(f"fixture check raised {type(error).__name__}: {error}")
                        self.assertTrue(
                            any("invalid fixture JSON" in error for error in errors),
                            errors,
                        )

    def test_fixture_generator_is_byte_deterministic(self) -> None:
        script = ROOT / "tools" / "build_fixtures.py"
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
