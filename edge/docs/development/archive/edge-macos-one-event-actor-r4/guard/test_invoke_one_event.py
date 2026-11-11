from __future__ import annotations

import hashlib
import json
import os
import stat
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from unittest.mock import patch


sys.path.insert(0, str(Path(__file__).parent))

from invoke_one_event import AlreadyClaimed, ContractError, run_once


MARKER = b"teslatlas-synthetic-vehicle-identity-v2"
TEST_VIN = "5YJ3E1EA7KF000009"
ENDPOINT = "wss://127.0.0.1:29999/edge-r4-test"
TRANSACTION_ID = "edge-r4-test-v-001"
FIXED_CREATED_AT_MS = 1_900_000_000_000


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path: Path, value: object, mode: int = 0o600) -> None:
    path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")
    path.chmod(mode)


def read_json(path: Path) -> dict:
    return json.loads(path.read_text())


def mode(path: Path) -> int:
    return stat.S_IMODE(path.stat().st_mode)


@contextmanager
def temporary_environment(values: dict[str, str]):
    with patch.dict(os.environ, values, clear=False):
        yield


class Fixture:
    def __init__(self, root: Path, exit_code: int = 0, timeout_seconds: float = 30.0):
        self.root = root
        self.exit_code = exit_code
        self.timeout_seconds = timeout_seconds
        self.binding_path = root / "binding.json"
        self.authorization_path = root / "authorization.json"
        self.contract_path = root / "contract.json"
        self.helper_path = root / "synthetic-vehicle"
        self.certificate_path = root / "vehicle-client.crt"
        self.key_path = root / "vehicle-client.key"
        self.server_ca_path = root / "receiver-ca.crt"
        self.guard_path = root / "producer-invocation-guard.json"
        self.result_path = root / "producer-result.json"
        self.output_path = root / "producer-output.log"
        self.calls_path = root / "calls.log"
        self.args_path = root / "args.bin"

        self.payload = {
            "topic": "V",
            "transaction_type": "V",
            "field": "VehicleName",
            "value": "synthetic-b2",
            "value_type": "string",
            "timestamp_source": "single-wrapper-created-at-ms",
        }
        self.binding = {
            "schema_version": 1,
            "event_authorized": False,
            "vin": TEST_VIN,
            "endpoint": ENDPOINT,
            "transaction_type": "V",
            "transaction_id": TRANSACTION_ID,
            "payload": self.payload,
        }

        self.helper_path.write_bytes(
            b"#!/bin/sh\n"
            b"set -eu\n"
            b"printf 'call\\n' >> \"$FAKE_HELPER_CALLS\"\n"
            b"printf '%s\\0' \"$@\" >> \"$FAKE_HELPER_ARGS\"\n"
            b"if [ \"${FAKE_HELPER_SLEEP:-0}\" != 0 ]; then exec sleep \"$FAKE_HELPER_SLEEP\"; fi\n"
            b"exit \"${FAKE_HELPER_EXIT:-0}\"\n"
            + MARKER
        )
        self.helper_path.chmod(0o500)
        self.certificate_path.write_bytes(b"fresh-test-client-certificate")
        self.key_path.write_bytes(b"fresh-test-client-key")
        self.server_ca_path.write_bytes(b"fresh-test-server-ca")
        for path in (self.certificate_path, self.key_path, self.server_ca_path):
            path.chmod(0o600)

        write_json(self.binding_path, self.binding)
        binding_hash = digest(self.binding_path)
        payload_hash = hashlib.sha256(
            json.dumps(self.payload, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest()
        self.authorization = {
            "schema_version": 1,
            "event_authorized": True,
            "cohort_id": "edge-macos-one-event-actor-r4-test",
            "binding_sha256": binding_hash,
            "helper_sha256": digest(self.helper_path),
            "transaction_id": TRANSACTION_ID,
            "payload_sha256": payload_hash,
            "endpoint": ENDPOINT,
            "certificate_sha256": digest(self.certificate_path),
            "key_sha256": digest(self.key_path),
            "server_ca_sha256": digest(self.server_ca_path),
            "invocation_count": 1,
            "retry_or_replay": False,
            "timeout_seconds": timeout_seconds,
            "expires_at_ms": FIXED_CREATED_AT_MS + 60_000,
        }
        write_json(self.authorization_path, self.authorization)
        self.contract = {
            "schema_version": 1,
            "authorization": self.authorization,
            "binding": {
                "path": str(self.binding_path),
                "sha256": binding_hash,
                "mode": "0600",
            },
            "helper": {
                "path": str(self.helper_path),
                "sha256": digest(self.helper_path),
                "mode": "0500",
                "identity_marker": MARKER.decode(),
                "forbidden_literals": ["edge-b2-envelope-0001", "5YJ3E1EA7KF000001"],
            },
            "tls_inputs": {
                "certificate": {
                    "path": str(self.certificate_path),
                    "sha256": digest(self.certificate_path),
                    "mode": "0600",
                },
                "key": {
                    "path": str(self.key_path),
                    "sha256": digest(self.key_path),
                    "mode": "0600",
                },
                "server_ca": {
                    "path": str(self.server_ca_path),
                    "sha256": digest(self.server_ca_path),
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

    def kwargs(self, clock_ms, **overrides):
        values = {
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
            "clock_ms": clock_ms,
        }
        values.update(overrides)
        return values


class InvokeOneEventTests(unittest.TestCase):
    def make_fixture(self, exit_code: int = 0, timeout_seconds: float = 30.0):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        fixture = Fixture(Path(temporary.name), exit_code, timeout_seconds)
        return fixture

    def helper_environment(self, fixture: Fixture, **overrides: str) -> dict[str, str]:
        values = {
            "FAKE_HELPER_CALLS": str(fixture.calls_path),
            "FAKE_HELPER_ARGS": str(fixture.args_path),
        }
        values.update(overrides)
        return values

    def test_success_creates_exclusive_guard_and_atomic_result_once(self):
        fixture = self.make_fixture()
        clock_calls: list[int] = []

        def clock_ms() -> int:
            clock_calls.append(FIXED_CREATED_AT_MS)
            return FIXED_CREATED_AT_MS

        with temporary_environment(self.helper_environment(fixture)):
            result = run_once(**fixture.kwargs(clock_ms))

        self.assertEqual(result["state"], "completed")
        self.assertEqual(clock_calls, [FIXED_CREATED_AT_MS])
        self.assertEqual(mode(fixture.guard_path), 0o600)
        self.assertEqual(mode(fixture.result_path), 0o600)
        self.assertEqual(mode(fixture.output_path), 0o600)
        self.assertNotIn(TEST_VIN, fixture.guard_path.read_text())
        self.assertNotIn(TEST_VIN, fixture.result_path.read_text())
        self.assertFalse((fixture.result_path.parent / ".producer-result.json.tmp").exists())
        self.assertEqual(fixture.calls_path.read_text(), "call\n")
        self.assertEqual(
            fixture.args_path.read_bytes().split(b"\0")[:-1],
            [
                b"-endpoint",
                ENDPOINT.encode(),
                b"-cert",
                str(fixture.certificate_path).encode(),
                b"-key",
                str(fixture.key_path).encode(),
                b"-server-ca",
                str(fixture.server_ca_path).encode(),
                b"-vin",
                TEST_VIN.encode(),
                b"-txid",
                TRANSACTION_ID.encode(),
                b"-created-at-ms",
                str(FIXED_CREATED_AT_MS).encode(),
            ],
        )
        self.assertEqual(read_json(fixture.guard_path)["invocation_count"], 1)
        self.assertEqual(read_json(fixture.result_path)["created_at_ms"], FIXED_CREATED_AT_MS)

    def test_existing_guard_is_final_and_does_not_spawn_or_read_clock(self):
        fixture = self.make_fixture()
        with temporary_environment(self.helper_environment(fixture)):
            run_once(**fixture.kwargs(lambda: FIXED_CREATED_AT_MS))

            def forbidden_clock() -> int:
                self.fail("clock must not be read after the exclusive guard exists")

            with self.assertRaises(AlreadyClaimed):
                run_once(**fixture.kwargs(forbidden_clock))

        self.assertEqual(fixture.calls_path.read_text(), "call\n")

    def test_nonzero_helper_is_final_no_retry_and_result_records_failure(self):
        fixture = self.make_fixture(exit_code=7)
        with temporary_environment(self.helper_environment(fixture, FAKE_HELPER_EXIT="7")):
            result = run_once(**fixture.kwargs(lambda: FIXED_CREATED_AT_MS))

        self.assertEqual(result["state"], "failed_no_retry")
        self.assertEqual(result["exit_code"], 7)
        self.assertEqual(fixture.calls_path.read_text(), "call\n")
        self.assertEqual(read_json(fixture.guard_path)["retry_or_replay"], False)

    def test_timeout_is_final_no_retry_and_result_records_timeout(self):
        fixture = self.make_fixture(timeout_seconds=0.2)
        with temporary_environment(
            self.helper_environment(fixture, FAKE_HELPER_SLEEP="0.5")
        ):
            result = run_once(**fixture.kwargs(lambda: FIXED_CREATED_AT_MS))

        self.assertEqual(result["state"], "timed_out_no_retry")
        self.assertEqual(result["timeout_seconds"], 0.2)
        self.assertEqual(read_json(fixture.guard_path)["invocation_count"], 1)

    def test_authorization_mismatch_fails_before_timestamp_guard_or_spawn(self):
        fixture = self.make_fixture()
        write_json(fixture.authorization_path, {**fixture.authorization, "transaction_id": "wrong"})

        with temporary_environment(self.helper_environment(fixture)):
            with self.assertRaises(ContractError):
                run_once(
                    **fixture.kwargs(
                        lambda: self.fail("clock must not be read before authorization passes")
                    )
                )

        self.assertFalse(fixture.guard_path.exists())
        self.assertFalse(fixture.result_path.exists())
        self.assertFalse(fixture.output_path.exists())
        self.assertFalse(fixture.calls_path.exists())

    def test_local_helper_suite_is_browser_free_and_does_not_open_network(self):
        fixture = self.make_fixture()
        with patch("socket.socket", side_effect=AssertionError("network is forbidden")):
            with temporary_environment(self.helper_environment(fixture)):
                result = run_once(**fixture.kwargs(lambda: FIXED_CREATED_AT_MS))

        self.assertEqual(result["state"], "completed")


if __name__ == "__main__":
    unittest.main()
