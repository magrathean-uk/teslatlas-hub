#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""One synthetic receiver → Edge spool → app-managed Hub path."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import signal
import socket
import sqlite3
import subprocess
import time
import uuid
import urllib.error
import urllib.request
from pathlib import Path

VIN = "5YJ3E1EA7KF000099"
SOURCE_ID = "10a0be67-fca9-4f94-bcf5-004f82e442b8"
VEHICLE_ID = "4f414935-0523-4146-b6ef-286afe933c78"
INSTALLATION_ID = "local-edge"
LINEAGE = "spool-source-run"
CAR_ID = 9
LABEL = "com.teslatlas.hub.development.mrrepair20260920"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(arguments: list[str], env: dict[str, str] | None = None) -> subprocess.CompletedProcess[bytes]:
    completed = subprocess.run(
        arguments,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"{arguments[:3]} failed ({completed.returncode}): "
            f"{completed.stderr.decode('utf-8', errors='replace')[-2000:]}"
        )
    return completed


def reserve_loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def wait_tcp(port: int, process: subprocess.Popen[bytes] | None = None, attempts: int = 200) -> None:
    for _ in range(attempts):
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"process exited before 127.0.0.1:{port} listened")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"127.0.0.1:{port} did not listen")


def wait_http(url: str, process: subprocess.Popen[bytes] | None = None) -> None:
    import urllib.request

    last: Exception | None = None
    for _ in range(200):
        if process is not None and process.poll() is not None:
            raise RuntimeError(f"process exited before {url} was ready")
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if 200 <= response.status < 300:
                    return
        except Exception as error:
            last = error
        time.sleep(0.05)
    raise RuntimeError(f"{url} was not ready: {last}")


def openssl(*arguments: str) -> None:
    run(["openssl", *arguments])


def write_private(path: Path, text: str, mode: int = 0o600) -> None:
    path.write_text(text, encoding="utf-8")
    path.chmod(mode)


def make_tls_pair(
    root: Path,
    name: str,
    server: bool,
    ca_cn: str | None = None,
    leaf_cn: str | None = None,
) -> dict[str, Path]:
    ca_key = root / f"{name}-ca.key"
    ca_crt = root / f"{name}-ca.crt"
    key = root / f"{name}.key"
    csr = root / f"{name}.csr"
    crt = root / f"{name}.crt"
    ext = root / f"{name}.ext"
    openssl(
        "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-nodes", "-days", "1",
        "-subj", f"/O=Teslatlas Synthetic/CN={ca_cn or f'{name} CA'}",
        "-addext", "basicConstraints=critical,CA:TRUE,pathlen:1",
        "-addext", "keyUsage=critical,keyCertSign,cRLSign",
        "-keyout", str(ca_key), "-out", str(ca_crt),
    )
    usages = "serverAuth" if server else "clientAuth"
    san = "\nsubjectAltName=IP:127.0.0.1,DNS:localhost" if server else ""
    ext.write_text(
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\n"
        f"extendedKeyUsage={usages}{san}\n",
        encoding="ascii",
    )
    openssl(
        "req", "-new", "-newkey", "rsa:2048", "-sha256", "-nodes",
        "-subj", f"/O=Teslatlas Synthetic/CN={leaf_cn or ('127.0.0.1' if server else name)}",
        "-keyout", str(key), "-out", str(csr),
    )
    openssl(
        "x509", "-req", "-in", str(csr), "-CA", str(ca_crt), "-CAkey", str(ca_key),
        "-CAcreateserial", "-days", "1", "-sha256", "-extfile", str(ext), "-out", str(crt),
    )
    for path in (ca_key, ca_crt, key, crt, ext, csr):
        if path.exists():
            path.chmod(0o600)
    return {"ca": ca_crt, "key": key, "crt": crt}


def domain() -> str:
    return f"gui/{os.getuid()}"


def launchctl(*arguments: str) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        ["/bin/launchctl", *arguments],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )


def bootout(service: str) -> None:
    launchctl("bootout", service)
    for _ in range(50):
        printed = launchctl("print", service)
        if printed.returncode != 0:
            return
        time.sleep(0.05)


def projected_edge_count(database: Path) -> int:
    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    try:
        exists = connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='edge_applications'"
        ).fetchone()
        if exists is None:
            return 0
        row = connection.execute(
            "SELECT COUNT(*) FROM edge_applications WHERE disposition = 'projected_telemetry'"
        ).fetchone()
        return int(row[0]) if row else 0
    finally:
        connection.close()


def spool_receipt_count(spool: Path) -> int:
    receipts = spool / "receipts"
    if not receipts.is_dir():
        return 0
    return sum(1 for path in receipts.iterdir() if path.suffix == ".tlea")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--hub-bin", required=True, type=Path)
    parser.add_argument("--edge-bin", required=True, type=Path)
    parser.add_argument("--receiver-bin", required=True, type=Path)
    parser.add_argument("--helper-bin", required=True, type=Path)
    parser.add_argument("--interop-fixture", required=True, type=Path)
    parser.add_argument("--hub-port", type=int, default=21444)
    arguments = parser.parse_args()
    root = arguments.root.resolve()
    binaries = {
        "hub": arguments.hub_bin.resolve(strict=True),
        "edge": arguments.edge_bin.resolve(strict=True),
        "receiver": arguments.receiver_bin.resolve(strict=True),
        "helper": arguments.helper_bin.resolve(strict=True),
        "fixture": arguments.interop_fixture.resolve(strict=True),
    }
    for path in binaries.values():
        if path.is_symlink() or not os.access(path, os.X_OK):
            raise SystemExit(f"unsafe or non-executable artifact: {path}")

    runtime = root / "runtime"
    if runtime.exists():
        raise SystemExit(f"runtime root already exists: {runtime}")
    runtime.mkdir(mode=0o700)
    hub_fixture = runtime / "hub-fixture"
    edge_root = runtime / "edge"
    logs = runtime / "logs"
    for path in (edge_root, logs):
        path.mkdir(mode=0o700)

    hub_port = arguments.hub_port
    edge_receiver_port = 8080
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        if probe.connect_ex(("127.0.0.1", edge_receiver_port)) == 0:
            raise RuntimeError("pinned receiver dispatcher requires free 127.0.0.1:8080")
    edge_delivery_port = reserve_loopback_port()
    go_data_port = reserve_loopback_port()
    go_status_port = reserve_loopback_port()

    prepared = json.loads(
        run(
            [
                str(binaries["fixture"]),
                "--output", str(hub_fixture),
                "--scenario", "empty-edge-binding",
                "--source-id", SOURCE_ID,
                "--vehicle-id", VEHICLE_ID,
                "--vin", VIN,
                "--car-id", str(CAR_ID),
                "--installation-id", INSTALLATION_ID,
                "--lineage", LINEAGE,
            ]
        ).stdout
    )
    data_dir = Path(prepared["data_dir"])
    hub_tls = make_tls_pair(hub_fixture, "hub-server", server=True)
    edge_server = make_tls_pair(edge_root, "edge-server", server=True)
    edge_client = make_tls_pair(edge_root, "hub-client", server=False)

    edge_config = edge_root / "config.toml"
    write_private(
        edge_config,
        "\n".join(
            [
                "version = 1",
                f'state_directory = "{edge_root / "state"}"',
                f'receiver_bind = "127.0.0.1:{edge_receiver_port}"',
                f'hub_bind = "127.0.0.1:{edge_delivery_port}"',
                "allow_local_source_run_hub_loopback = true",
                f'receiver_bearer_path = "{edge_root / "state/receiver-token"}"',
                f'spool_key_path = "{edge_root / "state/spool-key"}"',
                f'credential_store_path = "{edge_root / "state/hub-credentials.json"}"',
                f'hub_server_certificate_path = "{edge_server["crt"]}"',
                f'hub_server_private_key_path = "{edge_server["key"]}"',
                f'hub_client_ca_path = "{edge_client["ca"]}"',
                "",
                "[spool]",
                "max_bytes = 1048576",
                "max_records = 2",
                "retention_seconds = 604800",
                "batch_max_bytes = 262144",
                "batch_max_records = 2",
                "",
            ]
        ),
    )
    run([str(binaries["edge"]), "--config", str(edge_config), "init"])
    enrolled = json.loads(
        run(
            [
                str(binaries["edge"]),
                "--config",
                str(edge_config),
                "credential",
                "enrol",
                "source-run-hub",
            ]
        ).stdout
    )
    bearer_path = edge_root / "delivery-token"
    write_private(bearer_path, enrolled["token"] + "\n")
    run([str(binaries["edge"]), "--config", str(edge_config), "doctor"])

    hub_config = hub_fixture / "config.toml"
    write_private(
        hub_config,
        "\n".join(
            [
                f'data_dir = "{data_dir}"',
                f'bind = "127.0.0.1:{hub_port}"',
                "[tls]",
                f'certificate_path = "{hub_tls["crt"]}"',
                f'private_key_path = "{hub_tls["key"]}"',
                f'public_url = "https://127.0.0.1:{hub_port}/"',
                "[collector]",
                "provider = 'fleet'",
                "interval_seconds = 0",
                "[collector.edge]",
                f'base_url = "https://127.0.0.1:{edge_delivery_port}/"',
                f'ca_certificate_path = "{edge_server["ca"]}"',
                f'client_certificate_path = "{edge_client["crt"]}"',
                f'client_private_key_path = "{edge_client["key"]}"',
                f'bearer_token_path = "{bearer_path}"',
                f'installation_id = "{INSTALLATION_ID}"',
                f'lineage = "{LINEAGE}"',
                f'source_id = "{SOURCE_ID}"',
                f'vehicle_id = "{VEHICLE_ID}"',
                f'vin = "{VIN}"',
                f"car_id = {CAR_ID}",
                "poll_milliseconds = 100",
                "timeout_seconds = 3",
                "max_backoff_seconds = 1",
                "[terrain]",
                "enabled = false",
                "[geocoder]",
                "enabled = false",
                "",
            ]
        ),
    )
    state_dir = runtime / "app-state"
    log_dir = runtime / "app-logs"
    state_dir.mkdir(mode=0o700)
    log_dir.mkdir(mode=0o700)
    hub_env = {
        **os.environ,
        "TESLATLAS_HUB_DEVELOPMENT": "1",
        "TESLATLAS_HUB_DEVELOPMENT_MODE": "edge",
        "RUST_LOG": "info,tower_http=debug",
    }
    run(
        [
            str(binaries["hub"]),
            "--config",
            str(hub_config),
            "serve-preflight",
            "--mode",
            "edge",
        ],
        env=hub_env,
    )

    plist = state_dir / f".{LABEL}.plist"
    write_private(
        plist,
        f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binaries["hub"]}</string>
    <string>--config</string>
    <string>{hub_config}</string>
    <string>serve</string>
  </array>
  <key>WorkingDirectory</key><string>{state_dir}</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>Umask</key><integer>63</integer>
  <key>EnvironmentVariables</key>
  <dict>
    <key>TESLATLAS_HUB_DEVELOPMENT</key><string>1</string>
    <key>TESLATLAS_HUB_DEVELOPMENT_MODE</key><string>edge</string>
    <key>RUST_LOG</key><string>info,tower_http=debug</string>
  </dict>
  <key>StandardOutPath</key><string>{log_dir / "hub.out.log"}</string>
  <key>StandardErrorPath</key><string>{log_dir / "hub.err.log"}</string>
</dict>
</plist>
""",
    )
    service = f"{domain()}/{LABEL}"
    bootout(service)
    for log_name in ("hub.out.log", "hub.err.log"):
        write_private(log_dir / log_name, "")

    edge_log = (logs / "edge.log").open("wb")
    edge_process: subprocess.Popen[bytes] | None = None
    receiver_process: subprocess.Popen[bytes] | None = None
    receiver_log = None
    keep_hub = False

    def stop_child(process: subprocess.Popen[bytes] | None) -> None:
        if process is None or process.poll() is not None:
            return
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)

    try:
        edge_process = subprocess.Popen(
            [str(binaries["edge"]), "--config", str(edge_config), "serve"],
            cwd=edge_root,
            stdin=subprocess.DEVNULL,
            stdout=edge_log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        wait_http(f"http://127.0.0.1:{edge_receiver_port}/healthz", edge_process)
        wait_tcp(edge_delivery_port, edge_process)
        loaded = launchctl("bootstrap", domain(), str(plist))
        if loaded.returncode != 0:
            raise RuntimeError(loaded.stdout.decode("utf-8", errors="replace")[-2000:])
        wait_tcp(hub_port)

        receiver_token = (edge_root / "state/receiver-token").read_text(encoding="ascii").strip()
        receiver_tls_dir = runtime / "receiver-tls"
        receiver_tls_dir.mkdir(mode=0o700)
        identities = make_tls_pair(
            receiver_tls_dir,
            "vehicle",
            server=False,
            ca_cn="Tesla Motors Products CA",
            leaf_cn=VIN,
        )
        receiver_tls = make_tls_pair(
            receiver_tls_dir,
            "receiver",
            server=True,
            ca_cn="Receiver Integration CA",
            leaf_cn="127.0.0.1",
        )
        bearer_file = runtime / "receiver-tls" / "receiver-bearer"
        write_private(bearer_file, receiver_token)
        receiver_config = runtime / "receiver-tls" / "fleet-telemetry.json"
        write_private(
            receiver_config,
            json.dumps(
                {
                    "host": "127.0.0.1",
                    "port": go_data_port,
                    "status_port": go_status_port,
                    "log_level": "info",
                    "json_log_enable": True,
                    "namespace": "teslatlas-source-run",
                    "teslatlas": {"timeout_ms": 2000},
                    "reliable_ack_sources": {"V": "teslatlas"},
                    "records": {"V": ["teslatlas"]},
                    "tls": {
                        "server_cert": str(receiver_tls["crt"]),
                        "server_key": str(receiver_tls["key"]),
                        "ca_file": str(identities["ca"]),
                    },
                },
                sort_keys=True,
            )
            + "\n",
        )

        receiver_env = dict(os.environ)
        receiver_env["TESLATLAS_FLEET_TELEMETRY_BEARER_FILE"] = str(bearer_file)
        receiver_log = (logs / "receiver.log").open("wb")
        receiver_process = subprocess.Popen(
            [str(binaries["receiver"]), f"-config={receiver_config}"],
            cwd=runtime / "receiver-tls",
            env=receiver_env,
            stdin=subprocess.DEVNULL,
            stdout=receiver_log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        wait_http(f"http://127.0.0.1:{go_status_port}/status", receiver_process)
        wait_tcp(edge_delivery_port, edge_process)

        database = data_dir / "hub.sqlite"
        before = projected_edge_count(database)
        txid = "source-run-" + uuid.uuid4().hex
        run(
            [
                str(binaries["helper"]),
                "-endpoint", f"wss://127.0.0.1:{go_data_port}/",
                "-cert", str(identities["crt"]),
                "-key", str(identities["key"]),
                "-server-ca", str(receiver_tls["ca"]),
                "-vin", VIN,
                "-txid", txid,
                "-created-at-ms", str(time.time_ns() // 1_000_000),
            ]
        )
        def next_batch_empty() -> bool:
            probed = subprocess.run(
                [
                    "curl", "-sS",
                    "--cacert", str(edge_server["ca"]),
                    "--cert", str(edge_client["crt"]),
                    "--key", str(edge_client["key"]),
                    "-H", f"Authorization: Bearer {enrolled['token']}",
                    f"https://127.0.0.1:{edge_delivery_port}/v2/hub/batches/next",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            if probed.returncode != 0:
                return False
            try:
                body = json.loads(probed.stdout.decode())
            except json.JSONDecodeError:
                return False
            records = body.get("records") or []
            gaps = body.get("gaps") or []
            return records == [] and gaps == []

        deadline = time.time() + 30
        after = before
        drained = False
        while time.time() < deadline:
            after = projected_edge_count(database)
            if after > before:
                drained = next_batch_empty()
                if drained:
                    break
            time.sleep(0.1)
        if after <= before:
            raise RuntimeError("Hub did not project the receiver-admitted event")
        if not drained:
            raise RuntimeError("Edge next-batch was not empty after Hub projection/ACK")

        def hub_http(path: str) -> str:
            probed = subprocess.run(
                [
                    "curl", "-sS", "--fail",
                    "--cacert", str(hub_tls["ca"]),
                    f"https://127.0.0.1:{hub_port}{path}",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            if probed.returncode != 0:
                raise RuntimeError(
                    f"Hub {path} failed: {probed.stderr.decode()[-400:]} {probed.stdout.decode()[-400:]}"
                )
            return probed.stdout.decode()

        health = hub_http("/healthz")
        readiness = hub_http("/readyz")
        edge_doctor = run([str(binaries["edge"]), "--config", str(edge_config), "doctor"])

        def post_envelope(index: int) -> int:
            body = json.dumps(
                {
                    "version": 1,
                    "vin": VIN,
                    "txid": f"capacity-{index}-{uuid.uuid4().hex}",
                    "tx_type": "V",
                    "received_at_ms": int(time.time() * 1000),
                    "timestamp_ms": int(time.time() * 1000),
                    "payload": {
                        "vin": VIN,
                        "createdAt": "2026-09-20T00:00:00Z",
                        "data": {"Soc": {"intValue": str(70 + index)}},
                    },
                }
            ).encode()
            request = urllib.request.Request(
                f"http://127.0.0.1:{edge_receiver_port}/v1/internal/fleet-telemetry",
                data=body,
                method="POST",
                headers={
                    "Authorization": f"Bearer {receiver_token}",
                    "Content-Type": "application/json",
                },
            )
            try:
                with urllib.request.urlopen(request, timeout=3) as response:
                    return int(response.status)
            except urllib.error.HTTPError as error:
                return int(error.code)

        capacity_status = None
        for index in range(6):
            capacity_status = post_envelope(index)
            if capacity_status == 507:
                break
        if capacity_status != 507:
            raise RuntimeError(f"expected spool backpressure HTTP 507, got {capacity_status}")

        rotated = json.loads(
            run(
                [
                    str(binaries["edge"]),
                    "--config",
                    str(edge_config),
                    "credential",
                    "rotate",
                    enrolled["credential_id"],
                    "--overlap-seconds",
                    "0",
                ]
            ).stdout
        )
        old_token = enrolled["token"]
        write_private(bearer_path, rotated["token"] + "\n")
        stale = subprocess.run(
            [
                "curl", "-sS", "-o", "/dev/null", "-w", "%{http_code}",
                "--cacert", str(edge_server["ca"]),
                "--cert", str(edge_client["crt"]),
                "--key", str(edge_client["key"]),
                "-H", f"Authorization: Bearer {old_token}",
                f"https://127.0.0.1:{edge_delivery_port}/v2/hub/batches/next",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        rotated_rejected = stale.stdout.decode().strip() in {"401", "403"}
        if not rotated_rejected:
            raise RuntimeError(
                f"old Edge delivery bearer was still accepted after rotation: "
                f"{stale.stdout.decode()} {stale.stderr.decode()[-400:]}"
            )

        launchctl("kickstart", "-k", service)
        wait_tcp(hub_port)
        restarted = projected_edge_count(database)
        if restarted < after:
            raise RuntimeError("Hub restart lost projected observations")

        receipt = {
            "schema_version": 1,
            "result": "MR_F6_RECEIVER_EDGE_APP_MANAGED_HUB_PASS",
            "hub_port": hub_port,
            "launch_label": LABEL,
            "mode": "edge",
            "txid_sha256": hashlib.sha256(txid.encode()).hexdigest(),
            "observations_before": before,
            "observations_after": after,
            "observations_after_restart": restarted,
            "capacity_http": capacity_status,
            "old_delivery_bearer_rejected": rotated_rejected,
            "health": health.strip(),
            "readiness": readiness.strip(),
            "edge_doctor": edge_doctor.stdout.decode().strip(),
            "artifacts": {name: sha256(path) for name, path in binaries.items()},
            "receiver_sha256": sha256(binaries["receiver"]),
        }
        evidence = root / "evidence" / "mr-f6-receiver-edge-hub.json"
        write_private(evidence, json.dumps(receipt, indent=2, sort_keys=True) + "\n")
        print(json.dumps(receipt, sort_keys=True))
        keep_hub = True
        return 0
    finally:
        stop_child(receiver_process)
        stop_child(edge_process)
        if edge_log:
            edge_log.close()
        if receiver_log:
            receiver_log.close()
        if not keep_hub:
            bootout(service)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(json.dumps({"result": "FAILED", "error": str(error)}))
        raise
