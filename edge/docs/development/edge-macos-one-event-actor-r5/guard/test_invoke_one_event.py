from __future__ import annotations

import hashlib
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


import sys

sys.path.insert(0, str(Path(__file__).parent))

from invoke_one_event import AlreadyClaimed, ContractError, run_once


MARKER = b"teslatlas-synthetic-vehicle-identity-v2"
TEST_VIN = "5YJ3E1EA7KF000099"
ENDPOINT = "wss://127.0.0.1:29999/edge-r5-test"
TRANSACTION_ID = "edge-r5-test-v-001"
CREATED_AT_MS = 1_900_000_000_000


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
    path.chmod(0o600)


class Fixture:
    def __init__(self, root: Path):
        self.root = root
        self.contract_path = root / "contract.json"
        self.authorization_path = root / "authorization.json"
        self.binding_path = root / "binding.json"
        self.helper_path = root / "synthetic-vehicle"
        self.certificate_path = root / "vehicle-client.crt"
        self.key_path = root / "vehicle-client.key"
        self.server_ca_path = root / "receiver-ca.crt"
        self.guard_path = root / "producer-invocation-guard.json"
        self.result_path = root / "producer-result.json"
        self.output_path = root / "producer-output.log"
        self.calls_path = root / "calls.log"

        payload = {
            "topic": "V",
            "transaction_type": "V",
            "field": "VehicleName",
            "value": "synthetic-b2",
            "value_type": "string",
            "timestamp_source": "single-wrapper-created-at-ms",
        }
        binding = {
            "schema_version": 1,
            "event_authorized": False,
            "vin": TEST_VIN,
            "endpoint": ENDPOINT,
            "transaction_type": "V",
            "transaction_id": TRANSACTION_ID,
            "payload": payload,
        }
        self.helper_path.write_bytes(
            b"#!/bin/sh\n"
            b"set -eu\n"
            b"printf 'call\\n' >> \"$FAKE_HELPER_CALLS\"\n"
            b"if [ \"${FAKE_HELPER_SLEEP:-0}\" != 0 ]; then exec sleep \"$FAKE_HELPER_SLEEP\"; fi\n"
            b"exit 0\n"
            + MARKER
        )
        self.helper_path.chmod(0o500)
        for path, value in (
            (self.certificate_path, b"fresh-test-certificate"),
            (self.key_path, b"fresh-test-key"),
            (self.server_ca_path, b"fresh-test-ca"),
        ):
            path.write_bytes(value)
            path.chmod(0o600)
        write_json(self.binding_path, binding)

        payload_hash = hashlib.sha256(
            json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        authorization = {
            "schema_version": 1,
            "event_authorized": True,
            "cohort_id": "edge-macos-one-event-actor-r5-test",
            "binding_sha256": sha256(self.binding_path),
            "helper_sha256": sha256(self.helper_path),
            "transaction_id": TRANSACTION_ID,
            "payload_sha256": payload_hash,
            "endpoint": ENDPOINT,
            "certificate_sha256": sha256(self.certificate_path),
            "key_sha256": sha256(self.key_path),
            "server_ca_sha256": sha256(self.server_ca_path),
            "invocation_count": 1,
            "retry_or_replay": False,
            "timeout_seconds": 30,
            "expires_at_ms": CREATED_AT_MS + 60_000,
        }
        write_json(self.authorization_path, authorization)
        self.contract = {
            "schema_version": 1,
            "authorization": authorization,
            "binding": {
                "path": str(self.binding_path),
                "sha256": sha256(self.binding_path),
                "mode": "0600",
            },
            "helper": {
                "path": str(self.helper_path),
                "sha256": sha256(self.helper_path),
                "mode": "0500",
                "identity_marker": MARKER.decode(),
                "forbidden_literals": ["edge-b2-envelope-0001", "5YJ3E1EA7KF000001"],
            },
            "tls_inputs": {
                "certificate": {
                    "path": str(self.certificate_path),
                    "sha256": sha256(self.certificate_path),
                    "mode": "0600",
                },
                "key": {
                    "path": str(self.key_path),
                    "sha256": sha256(self.key_path),
                    "mode": "0600",
                },
                "server_ca": {
                    "path": str(self.server_ca_path),
                    "sha256": sha256(self.server_ca_path),
                    "mode": "0600",
                },
            },
            "outputs": {
                "guard": {"path": str(self.guard_path), "mode": "0600"},
                "result": {"path": str(self.result_path), "mode": "0600"},
                "output": {"path": str(self.output_path), "mode": "0600"},
            },
        }
        write_json(self.contract_path, self.contract)

    def replace_output_paths(self, *, guard=None, result=None, output=None) -> None:
        self.guard_path = guard or self.guard_path
        self.result_path = result or self.result_path
        self.output_path = output or self.output_path
        self.contract["outputs"] = {
            "guard": {"path": str(self.guard_path), "mode": "0600"},
            "result": {"path": str(self.result_path), "mode": "0600"},
            "output": {"path": str(self.output_path), "mode": "0600"},
        }
        write_json(self.contract_path, self.contract)

    def kwargs(self, clock):
        return {
            "contract_path": self.contract_path,
            "authorization_path": self.authorization_path,
            "binding_path": self.binding_path,
            "helper_path": self.helper_path,
            "certificate_path": self.certificate_path,
            "key_path": self.key_path,
            "server_ca_path": self.server_ca_path,
            "guard_path": self.guard_path,
            "result_path": self.result_path,
            "output_path": self.output_path,
            "clock_ms": clock,
        }


class InvokeOneEventR5Tests(unittest.TestCase):
    def fixture(self) -> Fixture:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        return Fixture(Path(temporary.name))

    def env(self, fixture: Fixture) -> dict[str, str]:
        return {"FAKE_HELPER_CALLS": str(fixture.calls_path)}

    def assertRejectedBeforeInvocation(self, fixture: Fixture, error_text: str) -> None:
        clock_calls = []

        def clock():
            clock_calls.append(CREATED_AT_MS)
            return CREATED_AT_MS

        with patch.dict(os.environ, self.env(fixture), clear=False):
            with self.assertRaisesRegex(ContractError, error_text):
                run_once(**fixture.kwargs(clock))
        self.assertEqual(clock_calls, [])
        self.assertFalse(fixture.guard_path.exists())
        self.assertFalse(fixture.calls_path.exists())

    def test_existing_result_rejects_before_clock_guard_or_spawn_and_preserves_sentinel(self):
        fixture = self.fixture()
        sentinel = b"result-sentinel\n"
        fixture.result_path.write_bytes(sentinel)
        fixture.result_path.chmod(0o600)

        self.assertRejectedBeforeInvocation(fixture, "result already exists")
        self.assertEqual(fixture.result_path.read_bytes(), sentinel)

    def test_existing_output_rejects_before_clock_guard_or_spawn_and_preserves_sentinel(self):
        fixture = self.fixture()
        sentinel = b"output-sentinel\n"
        fixture.output_path.write_bytes(sentinel)
        fixture.output_path.chmod(0o600)

        self.assertRejectedBeforeInvocation(fixture, "output already exists")
        self.assertEqual(fixture.output_path.read_bytes(), sentinel)

    def test_existing_result_temp_rejects_before_clock_guard_or_spawn_and_preserves_sentinel(self):
        fixture = self.fixture()
        temporary = fixture.result_path.parent / f".{fixture.result_path.name}.tmp"
        sentinel = b"temporary-sentinel\n"
        temporary.write_bytes(sentinel)
        temporary.chmod(0o600)

        self.assertRejectedBeforeInvocation(fixture, "result temporary file already exists")
        self.assertEqual(temporary.read_bytes(), sentinel)

    def test_existing_guard_is_final_before_clock_or_spawn(self):
        fixture = self.fixture()
        fixture.guard_path.write_bytes(b"guard-sentinel\n")
        fixture.guard_path.chmod(0o600)
        clock_calls = []

        def clock():
            clock_calls.append(CREATED_AT_MS)
            return CREATED_AT_MS

        with patch.dict(os.environ, self.env(fixture), clear=False):
            with self.assertRaises(AlreadyClaimed):
                run_once(**fixture.kwargs(clock))
        self.assertEqual(clock_calls, [])
        self.assertEqual(fixture.guard_path.read_bytes(), b"guard-sentinel\n")
        self.assertFalse(fixture.calls_path.exists())

    def test_output_aliases_reject_before_clock_guard_or_spawn(self):
        for alias in ("result_guard", "result_output", "guard_output"):
            with self.subTest(alias=alias):
                fixture = self.fixture()
                if alias == "result_guard":
                    fixture.replace_output_paths(result=fixture.guard_path)
                elif alias == "result_output":
                    fixture.replace_output_paths(result=fixture.output_path)
                else:
                    fixture.replace_output_paths(output=fixture.guard_path)
                self.assertRejectedBeforeInvocation(fixture, "output paths must be pairwise distinct")

    def test_output_input_alias_rejects_before_clock_and_preserves_input(self):
        fixture = self.fixture()
        original = fixture.binding_path.read_bytes()
        fixture.replace_output_paths(output=fixture.binding_path)

        self.assertRejectedBeforeInvocation(fixture, "output path aliases an input")
        self.assertEqual(fixture.binding_path.read_bytes(), original)

    def test_valid_absent_output_paths_still_allow_one_local_non_network_invocation(self):
        fixture = self.fixture()
        with patch.dict(os.environ, self.env(fixture), clear=False):
            result = run_once(**fixture.kwargs(lambda: CREATED_AT_MS))

        self.assertEqual(result["state"], "completed")
        self.assertEqual(fixture.calls_path.read_text(), "call\n")


if __name__ == "__main__":
    unittest.main()
