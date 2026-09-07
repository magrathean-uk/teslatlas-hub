"""Synthetic tests for the private installed-host broker data protocol."""

from __future__ import annotations

import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest

from conformance.hub_control import (
    BrokerOperationError,
    ControlClient,
    ControlProtocolError,
    DEADLINES,
    GREETING_DEADLINE,
    validate_host_session,
)


SESSION_ID = "11111111-1111-4111-8111-111111111111"
CHALLENGE_0 = "a" * 64
CHALLENGE_1 = "b" * 64
CHALLENGE_2 = "c" * 64


class SyntheticBroker:
    def __init__(self, root: Path, handler, *, greeting_raw=None, greeting_chunks=1, greeting_delay=0.0):
        self.path = root / "broker.sock"
        self.handler = handler
        self.greeting_raw = greeting_raw
        self.greeting_chunks = greeting_chunks
        self.greeting_delay = greeting_delay
        self.requests = []
        self.error = None
        self.closing = False
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(self.path))
        os.chmod(self.path, 0o600)
        self.listener.listen(1)
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        try:
            connection, _ = self.listener.accept()
            with connection:
                greeting = self.greeting_raw or (
                    json.dumps(
                        {
                            "schema_version": 1,
                            "type": "challenge",
                            "session_id": SESSION_ID,
                            "sequence": 0,
                            "challenge": CHALLENGE_0,
                        },
                        separators=(",", ":"),
                    ).encode()
                    + b"\n"
                )
                self.send_chunks(connection, greeting, self.greeting_chunks, self.greeting_delay)
                stream = connection.makefile("rb")
                try:
                    self.handler(self, connection, stream)
                finally:
                    stream.close()
        except BaseException as error:  # surfaced by close(), not swallowed in a thread
            if not self.closing:
                self.error = error
        finally:
            self.listener.close()

    def read_request(self, stream):
        value = json.loads(stream.readline())
        self.requests.append(value)
        return value

    def send_reply(self, connection, request, challenge, result, *, message_type="reply"):
        value = {
            "schema_version": 1,
            "type": message_type,
            "session_id": SESSION_ID,
            "sequence": request["sequence"],
            "challenge": challenge,
        }
        value["result" if message_type == "reply" else "error"] = result
        connection.sendall(json.dumps(value, separators=(",", ":")).encode() + b"\n")

    def reply_bytes(self, request, challenge, result, *, message_type="reply", encoding="utf-8"):
        value = {
            "schema_version": 1,
            "type": message_type,
            "session_id": SESSION_ID,
            "sequence": request["sequence"],
            "challenge": challenge,
        }
        value["result" if message_type == "reply" else "error"] = result
        return json.dumps(value, separators=(",", ":")).encode(encoding) + b"\n"

    def send_chunks(self, connection, raw, chunks, delay):
        width = max(1, (len(raw) + chunks - 1) // chunks)
        for offset in range(0, len(raw), width):
            try:
                connection.sendall(raw[offset:offset + width])
            except OSError:
                return
            if offset + width < len(raw):
                time.sleep(delay)

    def close(self):
        self.closing = True
        try:
            self.listener.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.listener.close()
        self.thread.join(timeout=3)
        if self.thread.is_alive():
            raise AssertionError("synthetic broker did not stop")
        if self.error is not None:
            raise self.error


class HubControlTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.root.chmod(0o700)

    def descriptor(self, path):
        return {
            "schema_version": 1,
            "kind": "installed-host",
            "broker_socket": str(path),
            "session_id": SESSION_ID,
            "registration_sha256": "9" * 64,
        }

    def test_total_deadline_contract_covers_controller_lifecycle_budgets(self):
        self.assertEqual(GREETING_DEADLINE, 10.0)
        self.assertEqual(
            DEADLINES,
            {"verify": 30.0, "stop": 60.0, "start": 60.0, "pair": 160.0, "revoke": 160.0},
        )

    def test_one_attachment_sequences_exact_requests_and_operation_results(self):
        def serve(broker, connection, stream):
            verify = broker.read_request(stream)
            broker.send_reply(
                connection,
                verify,
                CHALLENGE_1,
                {
                    "descriptor": {"status": "ready"},
                    "proof": {"status": "verified"},
                    "invitation": {"pairingId": SESSION_ID},
                    "expired_invitation": {"pairingId": SESSION_ID},
                    "events": [],
                },
            )
            stop = broker.read_request(stream)
            broker.send_reply(
                connection,
                stop,
                CHALLENGE_2,
                {"stopped": True, "events": [{"operation": "stop"}]},
            )

        broker = SyntheticBroker(self.root, serve)
        self.addCleanup(broker.close)
        with ControlClient.connect(self.descriptor(broker.path)) as client:
            self.assertEqual(client.request("verify")["proof"]["status"], "verified")
            self.assertTrue(client.request("stop")["stopped"])
        self.assertEqual(
            broker.requests,
            [
                {
                    "schema_version": 1,
                    "session_id": SESSION_ID,
                    "sequence": 1,
                    "challenge": CHALLENGE_0,
                    "op": "verify",
                },
                {
                    "schema_version": 1,
                    "session_id": SESSION_ID,
                    "sequence": 2,
                    "challenge": CHALLENGE_1,
                    "op": "stop",
                },
            ],
        )

    def test_revoke_requires_a_canonical_uuid_and_sends_no_extra_authority(self):
        def serve(broker, connection, stream):
            request = broker.read_request(stream)
            broker.send_reply(
                connection,
                request,
                CHALLENGE_1,
                {
                    "descriptor": {},
                    "proof": {},
                    "invitation": {},
                    "expired_invitation": {},
                    "events": [],
                },
            )

        broker = SyntheticBroker(self.root, serve)
        self.addCleanup(broker.close)
        with ControlClient.connect(self.descriptor(broker.path)) as client:
            for invalid in ("not-a-uuid", "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA", None, True):
                with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                    client.request("revoke", device_id=invalid)
            client.request("revoke", device_id=SESSION_ID)
        self.assertEqual(set(broker.requests[0]), {"schema_version", "session_id", "sequence", "challenge", "op", "device_id"})

    def test_stale_challenge_and_malformed_result_close_the_attachment(self):
        for result, challenge in (
            ({"stopped": True, "events": []}, CHALLENGE_0),
            ({"stopped": True, "events": [], "extra": True}, CHALLENGE_1),
        ):
            with self.subTest(result=result, challenge=challenge):
                def serve(broker, connection, stream):
                    request = broker.read_request(stream)
                    broker.send_reply(connection, request, challenge, result)
                    stream.readline()

                broker = SyntheticBroker(self.root, serve)
                try:
                    with ControlClient.connect(self.descriptor(broker.path)) as client:
                        with self.assertRaises(ControlProtocolError):
                            client.request("stop")
                        with self.assertRaises(ControlProtocolError):
                            client.request("verify")
                finally:
                    broker.close()
                    if broker.path.exists():
                        broker.path.unlink()

    def test_closed_broker_error_is_fatal_and_never_exposes_platform_text(self):
        for code, prior_sequence in (("invalid-request", False), ("operation-failed", True)):
            with self.subTest(code=code, prior_sequence=prior_sequence):
                def serve(broker, connection, stream):
                    request = broker.read_request(stream)
                    if prior_sequence:
                        request = {**request, "sequence": request["sequence"] - 1}
                    broker.send_reply(connection, request, CHALLENGE_1, {"code": code}, message_type="error")

                broker = SyntheticBroker(self.root, serve)
                try:
                    with ControlClient.connect(self.descriptor(broker.path)) as client:
                        with self.assertRaises(BrokerOperationError) as captured:
                            client.request("verify")
                        self.assertEqual(captured.exception.code, code)
                        self.assertNotIn("host", str(captured.exception).lower())
                finally:
                    broker.close()
                    if broker.path.exists():
                        broker.path.unlink()

    def test_descriptor_rejects_extra_fields_wrong_types_and_nonprivate_socket(self):
        broker = SyntheticBroker(self.root, lambda _broker, _connection, _stream: None)
        self.addCleanup(broker.close)
        valid = self.descriptor(broker.path)
        self.assertEqual(validate_host_session(valid), valid)
        invalid = [
            {**valid, "proof": {}},
            {**valid, "schema_version": True},
            {**valid, "session_id": "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"},
            {**valid, "session_id": "11111111-1111-1111-8111-111111111111"},
            {**valid, "registration_sha256": "A" * 64},
            {**valid, "broker_socket": "relative.sock"},
            {**valid, "broker_socket": str(self.root / "nested" / ".." / "broker.sock")},
        ]
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(ValueError):
                validate_host_session(value)
        os.chmod(broker.path, 0o666)
        with self.assertRaises(ControlProtocolError):
            ControlClient.connect(valid)

    def test_wire_rejects_non_utf8_greeting_and_reply(self):
        greeting_value = {
            "schema_version": 1,
            "type": "challenge",
            "session_id": SESSION_ID,
            "sequence": 0,
            "challenge": CHALLENGE_0,
        }
        greeting = json.dumps(greeting_value, separators=(",", ":")).encode("utf-16") + b"\n"
        broker = SyntheticBroker(self.root, lambda *_args: None, greeting_raw=greeting)
        try:
            with self.assertRaises(ControlProtocolError):
                ControlClient.connect(self.descriptor(broker.path))
        finally:
            broker.close()
            if broker.path.exists():
                broker.path.unlink()

        def serve_reply(broker, connection, stream):
            request = broker.read_request(stream)
            raw = broker.reply_bytes(
                request,
                CHALLENGE_1,
                {
                    "descriptor": {},
                    "proof": {},
                    "invitation": {},
                    "expired_invitation": {},
                    "events": [],
                },
                encoding="utf-16",
            )
            connection.sendall(raw)

        broker = SyntheticBroker(self.root, serve_reply)
        try:
            with ControlClient.connect(self.descriptor(broker.path)) as client:
                with self.assertRaises(ControlProtocolError):
                    client.request("verify")
        finally:
            broker.close()

    def test_greeting_and_reply_use_monotonic_total_deadlines(self):
        broker = SyntheticBroker(
            self.root,
            lambda *_args: None,
            greeting_chunks=4,
            greeting_delay=0.03,
        )
        started = time.monotonic()
        try:
            with self.assertRaises(ControlProtocolError):
                ControlClient.connect(self.descriptor(broker.path), connect_timeout=0.05)
            self.assertLess(time.monotonic() - started, 0.2)
        finally:
            broker.close()
            if broker.path.exists():
                broker.path.unlink()

        attachment_closed = threading.Event()

        def trickle_reply(broker, connection, stream):
            request = broker.read_request(stream)
            raw = broker.reply_bytes(
                request,
                CHALLENGE_1,
                {
                    "descriptor": {},
                    "proof": {},
                    "invitation": {},
                    "expired_invitation": {},
                    "events": [],
                },
            )
            broker.send_chunks(connection, raw, 5, 0.03)
            if stream.readline() == b"":
                attachment_closed.set()

        broker = SyntheticBroker(self.root, trickle_reply)
        try:
            with ControlClient.connect(
                self.descriptor(broker.path),
                _deadline_overrides={"verify": 0.07},
            ) as client:
                with self.assertRaises(ControlProtocolError):
                    client.request("verify")
                with self.assertRaises(ControlProtocolError):
                    client.request("verify")
            self.assertTrue(attachment_closed.wait(0.5))
        finally:
            broker.close()

    def test_delayed_valid_reply_within_total_budget_succeeds(self):
        def serve(broker, connection, stream):
            request = broker.read_request(stream)
            time.sleep(0.05)
            broker.send_reply(
                connection,
                request,
                CHALLENGE_1,
                {
                    "descriptor": {},
                    "proof": {},
                    "invitation": {},
                    "expired_invitation": {},
                    "events": [],
                },
            )

        broker = SyntheticBroker(self.root, serve)
        self.addCleanup(broker.close)
        with ControlClient.connect(
            self.descriptor(broker.path),
            _deadline_overrides={"verify": 0.2},
        ) as client:
            self.assertEqual(client.request("verify")["events"], [])


if __name__ == "__main__":
    unittest.main()
