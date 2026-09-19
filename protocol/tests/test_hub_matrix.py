"""Synthetic local TLS tests for the full Protocol matrix adapter contract."""

from __future__ import annotations

import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
from pathlib import Path
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock
import urllib.parse
import shutil

from conformance import hub_matrix


ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-http-v1/1.0.0"
HUB_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
NORMAL_PAIRING = "44444444-4444-4444-8444-444444444444"
EXPIRED_PAIRING = "66666666-6666-4666-8666-666666666666"
REAUTH_PAIRING = "55555555-5555-4555-8555-555555555555"
FIRST_DEVICE = "77777777-7777-4777-8777-777777777777"
SECOND_DEVICE = "88888888-8888-4888-8888-888888888888"
TOKEN_1 = "1" * 64
TOKEN_2 = "2" * 64
TOKEN_3 = "3" * 64
SYNTHETIC_SCENARIO = {
    "schema_version": 1,
    "name": "two-vehicles-five-drives",
    "provenance": "synthetic-only",
    "vehicle_ids": [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    ],
    "vehicles": [
        {"vehicle_id": "11111111-1111-4111-8111-111111111111", "display_name": "Interop – Árvíztűrő 🚗"},
        {"vehicle_id": "22222222-2222-4222-8222-222222222222", "display_name": "Interop empty"},
    ],
    "current": {
        "battery_level": 0,
        "inside_temp": 21.5,
        "outside_temp": None,
        "observed_at_ms": 1788566400000,
        "est_battery_range_km": 160.93,
        "odometer": 16093.44,
        "speed": 16,
        "scheduled_charging_start_time": 1788570000,
        "active_route_miles_to_arrival": 12.5,
    },
    "empty_vehicle_observed_at_ms": None,
    "drive_pages_at_limit_2": [[105, 104], [103, 102], [101]],
    "drives": {str(drive_id): {} for drive_id in (105, 104, 103, 102, 101)},
}


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def advance(self, seconds):
        self.now += seconds


def profile_sha256():
    return hashlib.sha256((PROFILE / "SHA256SUMS").read_bytes()).hexdigest()


class SyntheticHub:
    def __init__(self, root: Path):
        self.root = root
        self.available = True
        self.requests = []
        self.used_pairings = set()
        self.revoked_devices = set()
        self.current_token = None
        self.current_device = None
        self.claim_count = 0
        self.next_request = 0
        self.discovery_capabilities = None
        self.outage_mode = "disconnect"
        self.trickle_path = None
        self.trickle_delay = 0.0
        self.cert = root / "ca.pem"
        self.key = root / "key.pem"
        config = root / "openssl.cnf"
        config.write_text(
            "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3\n"
            "[dn]\nCN=127.0.0.1\n[v3]\nbasicConstraints=critical,CA:TRUE\n"
            "keyUsage=critical,digitalSignature,keyCertSign\nsubjectAltName=IP:127.0.0.1\n",
            encoding="utf-8",
        )
        generated = subprocess.run(
            [
                "openssl", "req", "-x509", "-nodes", "-newkey", "rsa:2048",
                "-keyout", str(self.key), "-out", str(self.cert), "-days", "1",
                "-config", str(config),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if generated.returncode != 0:
            raise AssertionError("synthetic TLS certificate generation failed")
        self.replacement_cert = root / "replacement.pem"
        self.replacement_key = root / "replacement-key.pem"
        replacement_csr = root / "replacement.csr"
        replacement_config = root / "replacement.cnf"
        replacement_config.write_text(
            "[req]\nprompt=no\ndistinguished_name=dn\n[dn]\nCN=127.0.0.1\n"
            "[v3]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\n"
            "extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n",
            encoding="utf-8",
        )
        for command in (
            [
                "openssl", "req", "-new", "-nodes", "-newkey", "rsa:2048",
                "-keyout", str(self.replacement_key), "-out", str(replacement_csr),
                "-config", str(replacement_config),
            ],
            [
                "openssl", "x509", "-req", "-in", str(replacement_csr),
                "-CA", str(self.cert), "-CAkey", str(self.key), "-CAcreateserial",
                "-out", str(self.replacement_cert), "-days", "1",
                "-extfile", str(replacement_config), "-extensions", "v3",
            ],
        ):
            completed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
            if completed.returncode != 0:
                raise AssertionError("synthetic replacement TLS certificate generation failed")
        der = ssl.PEM_cert_to_DER_cert(self.cert.read_text(encoding="ascii"))
        self.der_sha256 = hashlib.sha256(der).hexdigest()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, _format, *_args):
                return

            def _begin(self):
                if not owner.available and owner.outage_mode == "disconnect":
                    try:
                        self.connection.shutdown(2)
                    except OSError:
                        pass
                    self.connection.close()
                    return None
                owner.next_request += 1
                parsed = urllib.parse.urlsplit(self.path)
                body = b""
                if self.command == "POST":
                    length = int(self.headers.get("content-length", "0"))
                    body = self.rfile.read(length)
                owner.requests.append((self.command, parsed.path, parsed.query))
                return parsed, body, "synthetic-" + str(owner.next_request)

            def _send(self, status, request_id, value=None, *, headers=None):
                raw = b"" if value is None else json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
                self.send_response(status)
                if request_id is not None:
                    self.send_header("X-Request-ID", request_id)
                if value is not None:
                    self.send_header("Content-Type", "application/json")
                for key, item in (headers or {}).items():
                    self.send_header(key, item)
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                if raw:
                    if owner.trickle_path == urllib.parse.urlsplit(self.path).path:
                        for byte in raw:
                            try:
                                self.wfile.write(bytes((byte,)))
                                self.wfile.flush()
                            except OSError:
                                break
                            time.sleep(owner.trickle_delay)
                    else:
                        self.wfile.write(raw)

            def _authorized(self):
                return self.headers.get("authorization") == "Bearer " + str(owner.current_token)

            def do_GET(self):
                begun = self._begin()
                if begun is None:
                    return
                parsed, _body, request_id = begun
                if not owner.available:
                    if owner.outage_mode == "reachable_401":
                        return self._send(401, request_id)
                    if owner.outage_mode == "invalid_200":
                        return self._send(200, request_id, {"invalid": True})
                    if owner.outage_mode == "missing_request_id":
                        return self._send(200, None, {"vehicles": SYNTHETIC_SCENARIO["vehicles"]})
                if parsed.path == "/.well-known/teslatlas-hub":
                    value = json.loads((PROFILE / "examples/discovery.json").read_text())
                    value.update(hub_id=HUB_ID, version="2026.36.2")
                    if owner.discovery_capabilities is not None:
                        value["capabilities"] = owner.discovery_capabilities
                    return self._send(200, request_id, value)
                if parsed.path == "/healthz":
                    value = json.loads((PROFILE / "examples/health.json").read_text())
                    value["version"] = "2026.36.2"
                    return self._send(200, request_id, value)
                if parsed.path == "/readyz":
                    return self._send(200, request_id, json.loads((PROFILE / "examples/ready.json").read_text()))
                if not self._authorized() or owner.current_device in owner.revoked_devices:
                    return self._send(401, request_id)
                if parsed.path == "/v1/vehicles":
                    return self._send(200, request_id, {"vehicles": SYNTHETIC_SCENARIO["vehicles"]})
                if parsed.path.endswith("/current"):
                    vehicle_id = parsed.path.split("/")[3]
                    if vehicle_id == "33333333-3333-4333-8333-333333333333":
                        return self._send(404, request_id)
                    scenario = SYNTHETIC_SCENARIO
                    value = json.loads((PROFILE / "examples/current.json").read_text())
                    if vehicle_id == scenario["vehicle_ids"][0]:
                        value.update(scenario["current"])
                    elif vehicle_id == scenario["vehicle_ids"][1]:
                        value["observed_at_ms"] = None
                    else:
                        return self._send(404, request_id)
                    value["vehicle_id"] = vehicle_id
                    return self._send(200, request_id, value)
                if parsed.path.endswith("/drives"):
                    return self._drives(parsed, request_id)
                return self._send(404, request_id)

            def _drives(self, parsed, request_id):
                scenario = SYNTHETIC_SCENARIO
                vehicle_id = parsed.path.split("/")[3]
                query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
                cursor = query.get("cursor", [None])[0]
                if cursor is not None and (
                    vehicle_id != scenario["vehicle_ids"][0]
                    or set(query) != {"limit", "cursor"}
                ):
                    return self._send(
                        400,
                        request_id,
                        {"error": {"code": "invalid_cursor", "message": "synthetic"}},
                    )
                pages = {None: (0, "opaque-one"), "opaque-one": (1, "opaque-two"), "opaque-two": (2, None)}
                if cursor not in pages:
                    return self._send(400, request_id, {"error": {"code": "invalid_cursor", "message": "synthetic"}})
                page_index, next_cursor = pages[cursor]
                etag = '"synthetic-page-' + str(page_index + 1) + '"'
                headers = {"ETag": etag, "Cache-Control": "no-store"}
                if self.headers.get("if-none-match") == etag:
                    return self._send(304, request_id, headers=headers)
                example = json.loads((PROFILE / "examples/drives.json").read_text())["items"][0]
                items = []
                for drive_id in scenario["drive_pages_at_limit_2"][page_index]:
                    item = dict(example)
                    item.update(scenario["drives"][str(drive_id)])
                    item.update(id=drive_id, vehicle_id=scenario["vehicle_ids"][0])
                    items.append(item)
                return self._send(200, request_id, {"items": items, "next_cursor": next_cursor}, headers=headers)

            def do_POST(self):
                begun = self._begin()
                if begun is None:
                    return
                parsed, body, request_id = begun
                if parsed.path.startswith("/v1/pairings/") and parsed.path.endswith("/claim"):
                    pairing_id = parsed.path.split("/")[3]
                    try:
                        value = json.loads(body)
                    except ValueError:
                        return self._send(400, request_id)
                    secret = "4" * 64 if pairing_id == NORMAL_PAIRING else "5" * 64
                    if pairing_id not in (NORMAL_PAIRING, REAUTH_PAIRING) or pairing_id in owner.used_pairings or value.get("secret") != secret:
                        return self._send(401, request_id)
                    owner.used_pairings.add(pairing_id)
                    owner.claim_count += 1
                    owner.current_device = FIRST_DEVICE if owner.claim_count == 1 else SECOND_DEVICE
                    owner.current_token = TOKEN_1 if owner.claim_count == 1 else TOKEN_3
                    return self._send(
                        200,
                        request_id,
                        {"device_id": owner.current_device, "access_token": owner.current_token, "expires_at_ms": int(time.time() * 1000) + 900_000},
                    )
                if parsed.path == "/v1/device/rotate":
                    if not self._authorized():
                        return self._send(401, request_id)
                    owner.current_token = TOKEN_2
                    return self._send(
                        200,
                        request_id,
                        {"device_id": owner.current_device, "access_token": TOKEN_2, "expires_at_ms": int(time.time() * 1000) + 900_000},
                    )
                return self._send(404, request_id)

        class Server(ThreadingHTTPServer):
            daemon_threads = True

        self.server = Server(("127.0.0.1", 0), Handler)
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(self.cert, self.key)
        self.server.socket = self.context.wrap_socket(self.server.socket, server_side=True)
        self.endpoint = "https://127.0.0.1:" + str(self.server.server_address[1])
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def use_replacement_certificate(self):
        self.context.load_cert_chain(self.replacement_cert, self.replacement_key)

    def invitation(self, pairing_id, secret, expires):
        query = urllib.parse.urlencode(
            {"pairing_id": pairing_id, "secret": secret, "endpoint": self.endpoint, "tls_pin": self.der_sha256}
        )
        return {
            "pairingId": pairing_id,
            "secret": secret,
            "expiresAtMs": expires,
            "endpoint": self.endpoint,
            "tlsPin": self.der_sha256,
            "pairingUri": "teslatlas-hub://pair?" + query,
        }

    def close(self):
        self.available = True
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


class SyntheticControl:
    def __init__(self, hub: SyntheticHub):
        self.hub = hub
        self.profile = hub.root / "controller-profile"
        shutil.copytree(PROFILE, self.profile)
        self.scenario = hub.root / "controller-scenario.json"
        self.scenario.write_text(
            json.dumps(SYNTHETIC_SCENARIO, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
            encoding="utf-8",
        )
        self.generation = 1
        self.sequence = 0
        self.operations = []
        now = int(time.time() * 1000)
        self.normal = hub.invitation(NORMAL_PAIRING, "4" * 64, now + 900_000)
        self.expired = hub.invitation(EXPIRED_PAIRING, "6" * 64, now - 1_000)
        self.reauth = hub.invitation(REAUTH_PAIRING, "5" * 64, now + 900_000)

    def _running(self, invitation=None):
        self.sequence += 1
        descriptor = {
            "status": "ready",
            "provenance": "installed-package-service",
            "endpoint": self.hub.endpoint,
            "hub_id": HUB_ID,
            "hub_pid": 4242,
            "hub_started_at": "synthetic-start-" + str(self.generation),
            "service_generation": "synthetic-generation-" + str(self.generation),
            "binary_sha256": "a" * 64,
            "seed_binary_sha256": "b" * 64,
            "profile_id": "hub-http-v1@1.0.0",
            "profile_path": str(self.profile),
            "profile_sha256": profile_sha256(),
            "scenario_path": str(self.scenario),
            "scenario_sha256": hashlib.sha256(self.scenario.read_bytes()).hexdigest(),
            "certificate_path": str(self.hub.cert),
        }
        proof = {
            "status": "verified",
            "session_id": "11111111-1111-4111-8111-111111111111",
            "sequence": self.sequence,
            "tls": {"endpoint": self.hub.endpoint, "certificate_der_sha256": self.hub.der_sha256, "verified_chain": True, "verified_hostname": True, "redirect_count": 0},
            "discovery": {"hub_id": HUB_ID, "product_version": "2026.36.2", "profile_id": "hub-http-v1@1.0.0", "profile_sha256": profile_sha256()},
            "service": {"mode": "installed-deb-systemd", "generation": descriptor["service_generation"], "hub": {"pid": 4242, "start_identity": descriptor["hub_started_at"], "executable_sha256": "a" * 64}},
            "config": {"seed_sha256": "b" * 64, "scenario_sha256": descriptor["scenario_sha256"]},
        }
        return {"descriptor": descriptor, "proof": proof, "invitation": invitation or self.normal, "expired_invitation": self.expired, "events": list(self.operations)}

    def request(self, op, *, device_id=None):
        self.operations.append({"operation": op})
        if op == "verify":
            if not self.hub.available:
                raise AssertionError("verify called while stopped")
            return self._running()
        if op == "stop":
            self.hub.available = False
            return {"stopped": True, "events": list(self.operations)}
        if op == "start":
            self.generation += 1
            self.hub.available = True
            return self._running()
        if op == "pair":
            self.generation += 1
            self.hub.available = True
            return self._running(self.reauth)
        if op == "revoke":
            self.generation += 1
            self.hub.revoked_devices.add(device_id)
            self.hub.available = True
            return self._running()
        raise AssertionError("unexpected operation")

    def close(self):
        return None


class HubMatrixTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.hub = SyntheticHub(self.root)
        self.addCleanup(self.hub.close)

    def config(self):
        return self.config_for(self.hub, self.root)

    def config_for(self, _hub, root):
        return {
            "schema_version": 1,
            "kind": "protocol-actual-hub-matrix",
            "execution_kind": "actual_hub_acceptance",
            "adapter": "protocol_actual_hub",
            "cell_id": "protocol_actual_hub__debian13_amd64",
            "product_version": "2026.36.2",
            "profile": {"id": "hub-http-v1", "revision": "1.0.0", "path": str(PROFILE), "sha256": profile_sha256()},
            "source_identities": [
                {"role": "hub_source", "repo": str(ROOT / "synthetic-hub-source"), "head": "0" * 40, "dirty_patch_sha256": "c" * 64, "untracked_source_manifest_sha256": "d" * 64},
                {"role": "protocol_source", "repo": str(ROOT), "head": "1" * 40, "dirty_patch_sha256": "e" * 64, "untracked_source_manifest_sha256": "f" * 64},
            ],
            "artifacts": [
                {"role": "hub_executable", "name": "teslatlas-hub", "path": str(ROOT / "conformance/hub_matrix.py"), "embedded_version": "2026.36.2", "sha256": "a" * 64},
                {"role": "protocol_fixture_seed", "name": "interop-seed", "path": str(ROOT / "conformance/hub_control.py"), "embedded_version": "tooling", "sha256": "b" * 64},
            ],
            "runtime": {
                "hub": {"os": "Debian 13", "architecture": "amd64", "native_or_emulated": "emulated", "service_mode": "installed-deb-systemd", "tool_versions": {"hub": "2026.36.2"}},
                "client": {"os": "macOS", "architecture": "arm64", "native_or_emulated": "native", "service_mode": "protocol-conformance-process", "tool_versions": {"python": "3.11"}},
                "browser_engines": [],
                "client_transports": ["urllib"],
            },
            "host_session": {
                "schema_version": 1,
                "kind": "installed-host",
                "broker_socket": str(root / "unused.sock"),
                "session_id": "11111111-1111-4111-8111-111111111111",
                "registration_sha256": "9" * 64,
            },
        }

    def test_full_raw_matrix_has_all_twenty_one_typed_cases_and_redacted_transcripts(self):
        control = SyntheticControl(self.hub)
        evidence = hub_matrix.run_matrix(self.config(), control=control)
        self.assertEqual(evidence["execution_kind"], "actual_hub_acceptance")
        self.assertEqual(len(evidence["cases"]), 21)
        self.assertEqual({case["status"] for case in evidence["cases"]}, {"passed"})
        cases = {case["id"]: case for case in evidence["cases"]}
        self.assertEqual(cases["exact_current_values"]["actual"]["battery_level"], 0)
        self.assertIsNone(cases["exact_current_values"]["actual"]["outside_temp"])
        self.assertEqual(cases["drives_three_page_order"]["actual"]["pages"], [[105, 104], [103, 102], [101]])
        self.assertIsNone(cases["drives_terminal_cursor"]["actual"]["next_cursor"])
        self.assertEqual(cases["drives_etag_304"]["request_transcript"][0]["status"], 304)
        for case_id in ("drives_wrong_vehicle_cursor", "drives_wrong_filter_cursor"):
            self.assertEqual(cases[case_id]["evidence_kind"], "http")
            self.assertEqual(cases[case_id]["actual"], {"typed_error": "hub_http_error", "http_status": 400})
            self.assertEqual(cases[case_id]["request_transcript"][0]["status"], 400)
        self.assertEqual(cases["expired_invitation"]["request_transcript"], [])
        self.assertEqual(cases["unsupported_operation_zero_requests"]["request_transcript"], [])
        self.assertTrue(cases["endpoint_restart"]["actual"]["new_process"])
        self.assertEqual(
            cases["outage_recovery"]["request_transcript"][0],
            {"method": "GET", "route": "/v1/vehicles", "failure": "transport_unavailable"},
        )
        self.assertEqual(cases["outage_recovery"]["request_transcript"][-1]["status"], 200)
        rendered = json.dumps(evidence, sort_keys=True)
        self.assertNotIn("opaque-one", rendered)
        self.assertNotIn(TOKEN_1, rendered)
        self.assertNotIn(NORMAL_PAIRING, rendered)
        self.assertGreaterEqual(sum(1 for item in control.operations if item["operation"] == "verify"), 21)

    def test_entrypoint_uses_the_dispatched_mode_for_exit_semantics(self):
        native_pass = {"status": "passed", "cases": [{"status": 200}]}
        native_fail = {"status": "failed", "cases": []}
        matrix_pass = {"cases": [{"status": "passed"}, {"status": "passed"}]}
        matrix_fail = {"cases": [{"status": "passed"}, {"status": "failed"}]}
        for config, worker, result, expected_exit in (
            ({"kind": "native"}, "native", native_pass, 0),
            ({"kind": "native"}, "native", native_fail, 1),
            ({"kind": hub_matrix.MATRIX_KIND}, "matrix", matrix_pass, 0),
            ({"kind": hub_matrix.MATRIX_KIND}, "matrix", matrix_fail, 1),
        ):
            with self.subTest(worker=worker, result=result):
                with (
                    mock.patch.object(sys, "argv", ["actual-hub", "--config", "/private/config.json", "--json"]),
                    mock.patch.object(hub_matrix.hub_http, "read_private", return_value=config),
                    mock.patch.object(hub_matrix.hub_http, "run_network", return_value=result) as native,
                    mock.patch.object(hub_matrix, "run_matrix", return_value=result) as matrix,
                    mock.patch("sys.stdout", new_callable=io.StringIO),
                ):
                    self.assertEqual(hub_matrix.main(), expected_exit)
                self.assertEqual(native.call_count, int(worker == "native"))
                self.assertEqual(matrix.call_count, int(worker == "matrix"))

    def test_matrix_config_is_exact_and_does_not_accept_proof_or_commands(self):
        valid = self.config()
        self.assertEqual(hub_matrix.validate_matrix_config(valid), valid)
        for changed in (
            {**valid, "proof": {}},
            {**valid, "schema_version": True},
            {**valid, "kind": "installed-host"},
            {**valid, "host_session": {**valid["host_session"], "command": ["start"]}},
            {**valid, "source_identities": [{**valid["source_identities"][0], "extra": True}, valid["source_identities"][1]]},
            {**valid, "artifacts": [{**valid["artifacts"][0], "extra": True}, valid["artifacts"][1]]},
        ):
            with self.subTest(changed=changed), self.assertRaises(hub_matrix.MatrixAcceptanceError):
                hub_matrix.validate_matrix_config(changed)

    def test_strict_typed_comparison_rejects_bool_integer_equivalence(self):
        self.assertTrue(hub_matrix.typed_equal({"value": 0}, {"value": 0}))
        self.assertFalse(hub_matrix.typed_equal({"value": 0}, {"value": False}))
        self.assertFalse(hub_matrix.typed_equal([1], [1.0]))

    def test_unsupported_profile_operation_refuses_locally(self):
        with self.assertRaises(hub_matrix.UnsupportedOperationError):
            hub_matrix.reject_unsupported_operation("charges")

        cases = hub_matrix.MatrixCases(self.config(), SyntheticControl(self.hub))
        before = cases.http.request_count
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            cases._require_local_refusal(lambda: None, hub_matrix.UnsupportedOperationError)
        self.assertEqual(cases.http.request_count, before)

    def test_unsupported_case_fails_if_the_refusal_helper_returns(self):
        with mock.patch.object(hub_matrix, "reject_unsupported_operation", return_value=None):
            evidence = hub_matrix.run_matrix(self.config(), control=SyntheticControl(self.hub))
        cases = {case["id"]: case for case in evidence["cases"]}
        self.assertEqual(cases["unsupported_operation_zero_requests"]["status"], "failed")

    def test_claim_preflight_refuses_invalid_invitations_before_network(self):
        cases = hub_matrix.MatrixCases(self.config(), SyntheticControl(self.hub))
        before = cases.http.request_count
        for field, value in (
            ("expiresAtMs", int(time.time() * 1000) - 1),
            ("endpoint", "https://localhost.invalid"),
            ("tlsPin", "0" * 64),
        ):
            invitation = dict(cases.invitation)
            invitation[field] = value
            with self.subTest(field=field), self.assertRaises(hub_matrix.InvitationUseError) as captured:
                cases._claim(invitation, "Rejected synthetic invitation")
            self.assertEqual(captured.exception.typed_error, "protocol_validation")
            self.assertEqual(cases.http.request_count, before)
        claim = cases._claim(cases.invitation, "Accepted synthetic invitation")
        self.assertEqual(claim["device_id"], FIRST_DEVICE)
        self.assertEqual(cases.http.request_count, before + 1)

    def test_tls_pin_is_checked_on_the_connection_before_credentials_are_sent(self):
        client = hub_matrix.RawHttpClient(
            self.hub.endpoint,
            str(self.hub.cert),
            self.hub.der_sha256,
            str(PROFILE),
        )
        self.hub.use_replacement_certificate()
        before = len(self.hub.requests)
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            client.exchange(
                "claim",
                "/v1/pairings/" + NORMAL_PAIRING + "/claim",
                method="POST",
                status=401,
                bearer=TOKEN_1,
                body={"secret": "0" * 64, "device_name": "must not arrive"},
            )
        self.assertEqual(len(self.hub.requests), before)
        self.assertEqual(client.request_count, 0)

    def test_tls_ca_hostname_and_configured_pin_controls_fail_closed(self):
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            hub_matrix.RawHttpClient(self.hub.endpoint, str(self.hub.cert), "0" * 64, str(PROFILE))

        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            hostname_client = hub_matrix.RawHttpClient(
                self.hub.endpoint.replace("127.0.0.1", "localhost"),
                str(self.hub.cert),
                self.hub.der_sha256,
                str(PROFILE),
            )
            hostname_client.exchange("discovery", "/.well-known/teslatlas-hub")

        with tempfile.TemporaryDirectory() as other_root:
            other = SyntheticHub(Path(other_root))
            try:
                with self.assertRaises(hub_matrix.MatrixAcceptanceError):
                    untrusted_client = hub_matrix.RawHttpClient(
                        self.hub.endpoint,
                        str(other.cert),
                        other.der_sha256,
                        str(PROFILE),
                    )
                    untrusted_client.exchange("discovery", "/.well-known/teslatlas-hub")
            finally:
                other.close()
        self.assertEqual(self.hub.requests, [])

    def test_http_total_deadline_rejects_a_trickled_body(self):
        client = hub_matrix.RawHttpClient(
            self.hub.endpoint,
            str(self.hub.cert),
            self.hub.der_sha256,
            str(PROFILE),
            _total_timeout=0.08,
        )
        self.hub.trickle_path = "/readyz"
        self.hub.trickle_delay = 0.03
        started = time.monotonic()
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            client.exchange("ready", "/readyz")
        self.assertLess(time.monotonic() - started, 0.5)

    def test_http_total_deadline_expires_between_tcp_and_tls(self):
        clock = FakeClock()
        client = hub_matrix.RawHttpClient(
            self.hub.endpoint,
            str(self.hub.cert),
            self.hub.der_sha256,
            str(PROFILE),
            _total_timeout=1.0,
            _clock=clock,
        )
        raw_socket = mock.Mock()

        def consume_tcp_budget(_connection):
            clock.advance(1.0)
            return raw_socket

        with (
            mock.patch.object(hub_matrix._DeadlineHTTPSConnection, "_connect_tcp", consume_tcp_budget),
            mock.patch.object(hub_matrix._DeadlineHTTPSConnection, "_wrap_tls") as wrap_tls,
            self.assertRaises(hub_matrix.HttpTransportUnavailable),
        ):
            client.exchange("ready", "/readyz")
        wrap_tls.assert_not_called()
        raw_socket.close.assert_called_once_with()
        self.assertEqual(client.request_count, 0)

    def test_http_total_deadline_covers_separate_header_and_body_sends(self):
        clock = FakeClock()
        client = hub_matrix.RawHttpClient(
            self.hub.endpoint,
            str(self.hub.cert),
            self.hub.der_sha256,
            str(PROFILE),
            _total_timeout=1.0,
            _clock=clock,
        )
        leaf = ssl.PEM_cert_to_DER_cert(self.hub.cert.read_text(encoding="ascii"))

        class SplitSendSocket:
            def __init__(self):
                self.sent = []

            def getpeercert(self, *, binary_form=False):
                return leaf if binary_form else {}

            def settimeout(self, _timeout):
                return None

            def setblocking(self, _blocking):
                return None

            def send(self, value):
                self.sent.append(bytes(value))
                clock.advance(0.51)
                return len(value)

            def close(self):
                return None

        split_socket = SplitSendSocket()

        def connect(connection):
            connection.sock = split_socket

        with (
            mock.patch.object(hub_matrix._DeadlineHTTPSConnection, "connect", connect),
            self.assertRaises(hub_matrix.HttpTransportUnavailable),
        ):
            client.exchange(
                "claim",
                "/v1/pairings/" + NORMAL_PAIRING + "/claim",
                method="POST",
                bearer=TOKEN_1,
                body={"secret": "0" * 64, "device_name": "split send"},
            )
        self.assertEqual(len(split_socket.sent), 2)
        self.assertIn(b"Authorization: Bearer " + TOKEN_1.encode(), split_socket.sent[0])
        self.assertIn(b'"device_name":"split send"', split_socket.sent[1])
        self.assertEqual(self.hub.requests, [])

    def test_http_total_deadline_rejects_completion_after_validation(self):
        clock = FakeClock()
        client = hub_matrix.RawHttpClient(
            self.hub.endpoint,
            str(self.hub.cert),
            self.hub.der_sha256,
            str(PROFILE),
            _total_timeout=1.0,
            _clock=clock,
        )
        validate_raw = hub_matrix.hub_http.validate_raw

        def delayed_validation(*args, **kwargs):
            problems = validate_raw(*args, **kwargs)
            clock.advance(1.0)
            return problems

        with (
            mock.patch.object(hub_matrix.hub_http, "validate_raw", side_effect=delayed_validation),
            self.assertRaises(hub_matrix.MatrixAcceptanceError),
        ):
            client.exchange("ready", "/readyz")
        self.assertEqual(len(self.hub.requests), 1)

    def test_discovery_requires_the_full_fixture_capabilities(self):
        self.hub.discovery_capabilities = ["query.vehicles", "query.current"]
        evidence = hub_matrix.run_matrix(self.config(), control=SyntheticControl(self.hub))
        cases = {case["id"]: case for case in evidence["cases"]}
        self.assertEqual(cases["discovery_identity_profile"]["status"], "failed")

    def test_outage_rejects_reachable_or_contract_invalid_responses(self):
        for mode in ("reachable_401", "invalid_200", "missing_request_id"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as root:
                hub = SyntheticHub(Path(root))
                try:
                    hub.outage_mode = mode
                    evidence = hub_matrix.run_matrix(self.config_for(hub, Path(root)), control=SyntheticControl(hub))
                    cases = {case["id"]: case for case in evidence["cases"]}
                    self.assertEqual(cases["outage_recovery"]["status"], "failed")
                    self.assertNotIn(
                        {"method": "GET", "route": "/v1/vehicles", "failure": "transport_unavailable"},
                        cases["outage_recovery"]["request_transcript"],
                    )
                finally:
                    hub.close()

    def test_transport_client_does_not_emit_outage_evidence_without_confirmed_stop(self):
        client = hub_matrix.RawHttpClient(
            self.hub.endpoint,
            str(self.hub.cert),
            self.hub.der_sha256,
            str(PROFILE),
        )
        self.hub.available = False
        with self.assertRaises(hub_matrix.HttpTransportUnavailable):
            client.exchange("vehicles", "/v1/vehicles", bearer=TOKEN_1)
        self.assertEqual(client.transcript, [])

    def test_oversize_request_body_is_rejected_before_network(self):
        client = hub_matrix.RawHttpClient(
            self.hub.endpoint,
            str(self.hub.cert),
            self.hub.der_sha256,
            str(PROFILE),
        )
        before = len(self.hub.requests)
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            client.exchange(
                "claim",
                "/v1/pairings/{pairing_id}/claim",
                method="POST",
                body={"secret": "x" * 4080, "device_name": "synthetic"},
            )
        self.assertEqual(len(self.hub.requests), before)
        with self.assertRaises(hub_matrix.MatrixAcceptanceError):
            client.exchange(
                "claim",
                "/v1/pairings/{pairing_id}/claim",
                method="POST",
                body={"secret": "x" * (hub_matrix.hub_http.MAX_BYTES + 1), "device_name": "synthetic"},
            )
        self.assertEqual(len(self.hub.requests), before)


if __name__ == "__main__":
    unittest.main()
