#!/usr/bin/env python3
"""Focused tests for the fixed Edge matrix coordinator."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import socket
import sys
import threading
import unittest


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("edge_matrix_launcher", HERE / "edge.py")
assert SPEC is not None and SPEC.loader is not None
edge = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = edge
SPEC.loader.exec_module(edge)


SESSION_ID = "11111111-1111-4111-8111-111111111111"
DIGEST = "a" * 64


class FakeBroker:
    def __init__(self, socket_path: str):
        self.socket_path = socket_path
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(socket_path)
        self.server.listen(1)
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.requests: list[dict] = []
        self.thread.start()

    def _serve(self):
        connection, _ = self.server.accept()
        with connection:
            challenge = DIGEST
            connection.sendall(
                (json.dumps(
                    {"schema_version": 1, "type": "challenge", "session_id": SESSION_ID, "sequence": 0, "challenge": challenge}
                ) + "\n").encode()
            )
            buffer = b""
            while True:
                chunk = connection.recv(4096)
                if not chunk:
                    return
                buffer += chunk
                while b"\n" in buffer:
                    raw, buffer = buffer.split(b"\n", 1)
                    request = json.loads(raw)
                    self.requests.append(request)
                    next_challenge = ("b" if len(self.requests) == 1 else "c") * 64
                    if request["op"] == "stop":
                        result = {"stopped": True, "events": []}
                    else:
                        result = {
                            "descriptor": {},
                            "proof": {
                                "schema_version": 1,
                                "status": "verified",
                                "session_id": SESSION_ID,
                                "sequence": request["sequence"],
                                "challenge": request["challenge"],
                            },
                            "invitation": {},
                            "expired_invitation": {},
                            "events": [],
                        }
                    connection.sendall(
                        (json.dumps(
                            {"schema_version": 1, "type": "reply", "session_id": SESSION_ID, "sequence": request["sequence"], "challenge": next_challenge, "result": result}
                        ) + "\n").encode()
                    )
                    challenge = next_challenge

    def close(self):
        self.server.close()
        self.thread.join(timeout=1)


def session_for(socket_path: str) -> edge.Session:
    value = {"session_id": SESSION_ID, "broker": {"socket_path": socket_path}}
    return edge.Session(value, b"session", DIGEST, {}, {}, {})


class LauncherTests(unittest.TestCase):
    def test_reviewed_manifest_hashes_and_phase_table_are_bound(self):
        manifest = edge._strict_json(edge._read_public(edge.MANIFEST_PATH, label="manifest", maximum=1_048_576), "manifest")
        edge._validate_manifest(manifest)
        self.assertTrue(all(item["schema"]["sha256"] != "0" * 64 for item in manifest["raw_schemas"]))
        self.assertNotEqual(manifest["validator"]["sha256"], "0" * 64)

    def test_strict_json_rejects_duplicate_trailing_and_nonfinite(self):
        with self.assertRaises(edge.LauncherError):
            edge._strict_json(b'{"a":1,"a":2}')
        with self.assertRaises(edge.LauncherError):
            edge._strict_json(b'{"a":1} {"b":2}')
        with self.assertRaises(edge.LauncherError):
            edge._strict_json(b'{"a":NaN}')

    def test_session_shape_rejects_command_injection_before_file_resolution(self):
        import os
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session.json"
            value = {
                "schema_version": 1,
                "kind": "matrix-adapter-session",
                "run_id": "run",
                "cell_id": "edge_v2__debian13_amd64",
                "adapter_id": "edge_v2",
                "client_id": "edge_v2",
                "session_id": SESSION_ID,
                "instance_nonce": DIGEST,
                "command": "/bin/sh",
            }
            path.write_text(json.dumps(value), encoding="utf-8")
            os.chmod(path, 0o600)
            with self.assertRaises(edge.LauncherError):
                edge.load_session(path)

    def test_fixed_schedule_is_closed_and_monotonic(self):
        phases = edge._fixed_phases()
        self.assertEqual(len(phases), 31)
        self.assertEqual([item[2] for item in phases], list(range(1, 32)))
        self.assertEqual(phases[0], ("edge_normal", "initial", 1, "edge-initial"))
        self.assertEqual(phases[-1], ("edge_normal", "actors_closed", 31, "edge-actors_closed"))

    def test_raw_schema_selection_rejects_unknown_payload_keys(self):
        payload = {"status": "clean", "errors": []}
        with self.assertRaises(edge.LauncherError):
            edge._raw_schema(payload, ("edge_cleanup_v1",))

    def test_linux_runtime_architecture_is_bound_to_the_reviewed_worker_cell(self):
        header = {"runtime": {"client": {"architecture": "arm64"}}}
        arm64_session = edge.Session(
            {"cell_id": "edge_v2__debian13_arm64"}, b"session", DIGEST,
            header, {}, {},
        )
        amd64_session = edge.Session(
            {"cell_id": "edge_v2__debian13_amd64"}, b"session", DIGEST,
            header, {}, {},
        )

        self.assertEqual(
            edge._facts("edge_linux_runtime", arm64_session)["architecture"],
            "arm64",
        )
        self.assertEqual(
            edge._facts("edge_linux_runtime", amd64_session)["architecture"],
            "amd64",
        )

    def test_broker_binds_challenge_sequence_and_single_attachment(self):
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            socket_path = str(Path(directory) / "broker.sock")
            server = FakeBroker(socket_path)
            broker = edge.Broker(session_for(socket_path))
            try:
                broker.open(timeout=1)
                result = broker.request("verify", timeout=1)
                self.assertEqual(result["proof"]["sequence"], 1)
                self.assertEqual(broker.sequence, 1)
                with self.assertRaises(edge.LauncherError):
                    broker.open(timeout=1)
                broker.request("stop", timeout=1)
                self.assertEqual([item["op"] for item in server.requests], ["verify", "stop"])
            finally:
                broker.close()
                server.close()


if __name__ == "__main__":
    unittest.main()
