#!/usr/bin/env python3
"""Offline pinned receiver/bridge proof for the one-event synthetic lane."""

from __future__ import annotations

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid


VIN = "5YJ3E1EA7KF000099"
BRIDGE_PORT = 8080


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(*arguments: str, cwd: Path | None = None) -> None:
    completed = subprocess.run(
        arguments,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            completed.stderr.decode("utf-8", errors="replace")[-1200:]
        )


def reserve_loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def wait_status(port: int, process: subprocess.Popen[bytes]) -> None:
    last: Exception | None = None
    for _ in range(100):
        if process.poll() is not None:
            raise RuntimeError("receiver exited before readiness")
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{port}/status", timeout=1
            ) as response:
                if response.status == 200:
                    return
        except Exception as error:
            last = error
        time.sleep(0.05)
    raise RuntimeError(f"receiver status did not become ready: {last}")


def openssl_identity(root: Path) -> dict[str, Path]:
    vehicle_ca_key = root / "vehicle-ca.key"
    vehicle_ca = root / "vehicle-ca.crt"
    vehicle_key = root / "vehicle.key"
    vehicle_csr = root / "vehicle.csr"
    vehicle_crt = root / "vehicle.crt"
    vehicle_ext = root / "vehicle.ext"
    receiver_ca_key = root / "receiver-ca.key"
    receiver_ca = root / "receiver-ca.crt"
    receiver_key = root / "receiver.key"
    receiver_csr = root / "receiver.csr"
    receiver_crt = root / "receiver.crt"
    receiver_ext = root / "receiver.ext"

    run(
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-nodes",
        "-days", "1", "-subj", "/O=Teslatlas Synthetic/CN=Tesla Motors Products CA",
        "-addext", "basicConstraints=critical,CA:TRUE,pathlen:1",
        "-addext", "keyUsage=critical,keyCertSign,cRLSign",
        "-keyout", str(vehicle_ca_key), "-out", str(vehicle_ca),
    )
    vehicle_ext.write_text(
        "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=clientAuth\n",
        encoding="ascii",
    )
    run(
        "openssl", "req", "-new", "-newkey", "rsa:2048", "-sha256", "-nodes",
        "-subj", f"/O=Teslatlas Synthetic/CN={VIN}", "-keyout", str(vehicle_key),
        "-out", str(vehicle_csr),
    )
    run(
        "openssl", "x509", "-req", "-in", str(vehicle_csr), "-CA", str(vehicle_ca),
        "-CAkey", str(vehicle_ca_key), "-CAcreateserial", "-days", "1", "-sha256",
        "-extfile", str(vehicle_ext), "-out", str(vehicle_crt),
    )
    run(
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-nodes",
        "-days", "1", "-subj", "/O=Teslatlas Synthetic/CN=Receiver Integration CA",
        "-addext", "basicConstraints=critical,CA:TRUE,pathlen:1",
        "-addext", "keyUsage=critical,keyCertSign,cRLSign",
        "-keyout", str(receiver_ca_key), "-out", str(receiver_ca),
    )
    receiver_ext.write_text(
        "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n",
        encoding="ascii",
    )
    run(
        "openssl", "req", "-new", "-newkey", "rsa:2048", "-sha256", "-nodes",
        "-subj", "/O=Teslatlas Synthetic/CN=127.0.0.1", "-keyout", str(receiver_key),
        "-out", str(receiver_csr),
    )
    run(
        "openssl", "x509", "-req", "-in", str(receiver_csr), "-CA", str(receiver_ca),
        "-CAkey", str(receiver_ca_key), "-CAcreateserial", "-days", "1", "-sha256",
        "-extfile", str(receiver_ext), "-out", str(receiver_crt),
    )
    for path in root.iterdir():
        if path.is_file():
            path.chmod(0o600)
    return {
        "vehicle_ca": vehicle_ca,
        "vehicle_key": vehicle_key,
        "vehicle_crt": vehicle_crt,
        "receiver_ca": receiver_ca,
        "receiver_key": receiver_key,
        "receiver_crt": receiver_crt,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--receiver", required=True, type=Path)
    parser.add_argument("--helper", required=True, type=Path)
    arguments = parser.parse_args()
    receiver = arguments.receiver.resolve(strict=True)
    helper = arguments.helper.resolve(strict=True)
    if receiver.is_symlink() or helper.is_symlink():
        raise SystemExit("artifacts must not be symlinks")
    if not os.access(receiver, os.X_OK) or not os.access(helper, os.X_OK):
        raise SystemExit("artifacts must be executable")

    bodies: list[bytes] = []
    bearer = "tte-test-" + secrets.token_urlsafe(24)

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802
            if self.path != "/v1/internal/fleet-telemetry":
                self.send_response(404)
                self.end_headers()
                return
            if self.headers.get("Authorization") != f"Bearer {bearer}":
                self.send_response(401)
                self.end_headers()
                return
            length = int(self.headers.get("Content-Length", "0"))
            bodies.append(self.rfile.read(length))
            self.send_response(204)
            self.end_headers()

        def log_message(self, _format: str, *args: object) -> None:
            return

    server = ThreadingHTTPServer(("127.0.0.1", BRIDGE_PORT), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    receiver_process: subprocess.Popen[bytes] | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="teslatlas-one-event-bridge-") as name:
            root = Path(name)
            root.chmod(0o700)
            identities = openssl_identity(root)
            bearer_file = root / "receiver-bearer"
            bearer_file.write_text(bearer, encoding="ascii")
            bearer_file.chmod(0o600)
            data_port = reserve_loopback_port()
            status_port = reserve_loopback_port()
            config = root / "fleet-telemetry.json"
            config.write_text(
                json.dumps(
                    {
                        "host": "127.0.0.1",
                        "port": data_port,
                        "status_port": status_port,
                        "log_level": "info",
                        "json_log_enable": True,
                        "namespace": "teslatlas-edge-integration",
                        "teslatlas": {"timeout_ms": 2000},
                        "reliable_ack_sources": {"V": "teslatlas"},
                        "records": {"V": ["teslatlas"]},
                        "tls": {
                            "server_cert": str(identities["receiver_crt"]),
                            "server_key": str(identities["receiver_key"]),
                            "ca_file": str(identities["vehicle_ca"]),
                        },
                    },
                    sort_keys=True,
                    separators=(",", ":"),
                )
                + "\n",
                encoding="utf-8",
            )
            config.chmod(0o600)
            environment = dict(os.environ)
            environment["TESLATLAS_FLEET_TELEMETRY_BEARER_FILE"] = str(bearer_file)
            receiver_process = subprocess.Popen(
                [str(receiver), "-config", str(config)],
                cwd=root,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            wait_status(status_port, receiver_process)
            txid = "edge-bridge-integration-" + uuid.uuid4().hex
            completed = subprocess.run(
                [
                    str(helper), "-endpoint", f"wss://127.0.0.1:{data_port}/",
                    "-cert", str(identities["vehicle_crt"]), "-key", str(identities["vehicle_key"]),
                    "-server-ca", str(identities["receiver_ca"]), "-vin", VIN,
                    "-txid", txid, "-created-at-ms", str(time.time_ns() // 1_000_000),
                ],
                cwd=root,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=30,
                check=False,
            )
            time.sleep(0.2)
            if completed.returncode != 0:
                raise RuntimeError(
                    completed.stderr.decode("utf-8", errors="replace")[-1200:]
                )
            if len(bodies) != 1:
                raise RuntimeError(f"bridge admission count {len(bodies)} != 1")
            envelope = json.loads(bodies[0])
            if envelope.get("tx_type") != "V" or envelope.get("txid") != txid:
                raise RuntimeError("bridge admitted an unexpected envelope")
            result = {
                "status": "passed",
                "receiver_sha256": sha256(receiver),
                "helper_sha256": sha256(helper),
                "websocket_writes": 1,
                "http_admissions": 1,
                "binary_acks": 1,
                "connectivity_routed": False,
                "transaction_id_sha256": hashlib.sha256(txid.encode()).hexdigest(),
                "envelope_sha256": hashlib.sha256(bodies[0]).hexdigest(),
            }
            print(json.dumps(result, sort_keys=True))
    finally:
        if receiver_process is not None and receiver_process.poll() is None:
            receiver_process.send_signal(signal.SIGTERM)
            try:
                receiver_process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                receiver_process.kill()
                receiver_process.wait(timeout=5)
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
