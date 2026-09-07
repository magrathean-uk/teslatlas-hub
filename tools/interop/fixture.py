#!/usr/bin/env python3
"""Own one real, isolated Hub process with synthetic data and verified TLS.

Usage: fixture.py --config /private/run.json
The process remains attached until SIGINT/SIGTERM, a lifetime limit, or Hub
failure. It never changes an installed service or deletes its evidence.
"""
import argparse
import importlib.util
import hashlib
import json
import os
import re
from pathlib import Path
import signal
import socket
import ssl
import stat
import subprocess
import sys
import threading
import tempfile
import shutil
import time
import urllib.error
import urllib.request
import urllib.parse

MAX_CONFIG_BYTES = 64 * 1024

_native_path = Path(__file__).resolve().parents[3] / "teslatlas-protocol/conformance/hub_native_evidence.py"
_native_spec = importlib.util.spec_from_file_location("hub_native_evidence", _native_path)
native_evidence = importlib.util.module_from_spec(_native_spec)
_native_spec.loader.exec_module(native_evidence)


def ready_process_fields(process, root):
    if process.poll() is not None:
        raise RuntimeError("owned Hub exited before readiness witness")
    hub = native_evidence.process_identity(process.pid)
    launcher = native_evidence.process_identity(os.getpid())
    if hub["parent_pid"] != os.getpid():
        raise ValueError("Hub is not a direct owned child")
    return {"hub_pid": process.pid, "launcher_pid": os.getpid(),
            "hub_started_at": hub["started_at"], "launcher_started_at": launcher["started_at"],
            "ready_path": str(root / "ready.json")}



def read_private_json(path):
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    try:
        fd = os.open(path, flags)
    except OSError as error:
        raise ValueError("private config cannot be safely opened") from error
    with os.fdopen(fd, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
            raise ValueError("config must be an owner-only regular file")
        raw = stream.read(MAX_CONFIG_BYTES + 1)
    if len(raw) > MAX_CONFIG_BYTES:
        raise ValueError("config exceeds byte limit")
    return json.loads(raw)


def load_config(path):
    config = read_private_json(path)
    allowed = {"binary", "seed_binary", "output_dir", "port", "lifetime_seconds", "profile_id", "profile_path", "profile_sha256", "allowed_origins"}
    if not isinstance(config, dict) or set(config) - allowed:
        raise ValueError("unknown config fields")
    if config.get("profile_id") != "hub-http-v1@1.0.0":
        raise ValueError("current-Hub profile binding required")
    profile_path = config.get("profile_path")
    if not isinstance(profile_path, str) or not Path(profile_path).is_absolute():
        raise ValueError("absolute profile path required")
    profile_manifest = (Path(profile_path) / "SHA256SUMS").read_bytes()
    if hashlib.sha256(profile_manifest).hexdigest() != config.get("profile_sha256"):
        raise ValueError("profile digest mismatch")
    listed = set()
    for line in profile_manifest.decode("ascii").splitlines():
        digest, name = line.split("  ", 1)
        if not re.fullmatch("[0-9a-f]{64}", digest) or Path(name).is_absolute() or ".." in Path(name).parts or name in listed:
            raise ValueError("invalid profile file list")
        listed.add(name)
        if hashlib.sha256((Path(profile_path) / name).read_bytes()).hexdigest() != digest:
            raise ValueError("profile member digest mismatch")
    if "profile.json" not in listed or json.loads((Path(profile_path) / "profile.json").read_bytes()).get("profile_id") != config["profile_id"]:
        raise ValueError("profile identity mismatch")
    for key in ("binary", "seed_binary", "output_dir"):
        value = config.get(key)
        if not isinstance(value, str) or not Path(value).is_absolute():
            raise ValueError(key + " must be an absolute path")
    for key in ("binary", "seed_binary"):
        if not Path(config[key]).is_file() or not os.access(config[key], os.X_OK):
            raise ValueError(key + " must identify an executable")
    if os.path.lexists(config["output_dir"]):
        raise ValueError("output directory must not already exist")
    if "port" in config and (type(config["port"]) is not int or not 1 <= config["port"] <= 65535):
        raise ValueError("invalid port")
    lifetime = config.setdefault("lifetime_seconds", 3600)
    if type(lifetime) is not int or not 1 <= lifetime <= 86400:
        raise ValueError("invalid lifetime")
    origins = config.setdefault("allowed_origins", [])
    if not isinstance(origins, list) or len(origins) > 32:
        raise ValueError("invalid allowed origins")
    for origin in origins:
        if not isinstance(origin, str) or len(origin) > 2048:
            raise ValueError("invalid allowed origin")
        parsed = urllib.parse.urlsplit(origin)
        if (parsed.scheme not in ("http", "https") or not parsed.hostname or parsed.username is not None
                or parsed.password is not None or parsed.path or parsed.query or parsed.fragment
                or parsed.geturl() != origin):
            raise ValueError("allowed origins must be exact canonical HTTP origins")
    return config


def configure_allowed_origins(config_path, origins):
    if not origins:
        return
    path = Path(config_path)
    original = path.read_text()
    if "[http]" in original:
        raise ValueError("seeded fixture unexpectedly defines an HTTP section")
    with open(path, "a", encoding="utf-8") as stream:
        stream.write("[http]\nallowed_origins = %s\n" % json.dumps(origins, separators=(",", ":")))


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def write_private_json(path, value):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")


def wait_ready(process, descriptor, timeout=30):
    context = ssl.create_default_context(cafile=descriptor["certificate_path"])
    opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=context), NoRedirect())
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if process.poll() is not None:
            raise RuntimeError("Hub exited before readiness; inspect private serve.log")
        try:
            with opener.open(descriptor["endpoint"] + "/readyz", timeout=0.5) as response:
                raw = response.read(4097)
                if len(raw) <= 4096 and response.status == 200 and json.loads(raw).get("status") == "ready":
                    return
        except (OSError, urllib.error.URLError, ValueError):
            pass
        time.sleep(0.1)
    raise RuntimeError("Hub readiness timeout; inspect private serve.log")


def stage_executable(source, destination):
    # Copy file bytes (including embedded Mach-O signature) into a new private
    # run-owned inode. Later rebuilds cannot replace the executed pathname.
    with open(source, "rb") as incoming:
        fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)
        with os.fdopen(fd, "wb") as outgoing:
            shutil.copyfileobj(incoming, outgoing)
            outgoing.flush()
            os.fsync(outgoing.fileno())
    destination = Path(destination)
    destination.chmod(0o500)
    return destination, hashlib.sha256(destination.read_bytes()).hexdigest()


def run(config):
    stop = threading.Event()
    previous = {}
    for sig in (signal.SIGINT, signal.SIGTERM):
        previous[sig] = signal.signal(sig, lambda _sig, _frame: stop.set())
    process = None
    try:
        staging = Path(tempfile.mkdtemp(prefix=Path(config["output_dir"]).name + "-executables-", dir=Path(config["output_dir"]).parent))
        binary, binary_hash = stage_executable(config["binary"], staging / "teslatlas-hub")
        seed_binary, seed_hash = stage_executable(config["seed_binary"], staging / "interop_fixture")
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", config.get("port", 0)))
            port = reservation.getsockname()[1]
            seeded = subprocess.run([str(seed_binary), "--output", config["output_dir"], "--port", str(port)],
                                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=60)
            if seeded.returncode:
                raise RuntimeError("synthetic seed failed (exit %d)" % seeded.returncode)
        root = Path(config["output_dir"])
        descriptor = read_private_json(root / "connection.json")
        if descriptor["endpoint"] != "https://127.0.0.1:%d" % port:
            raise ValueError("seeded endpoint mismatch")
        configure_allowed_origins(descriptor["config_path"], config["allowed_origins"])
        # Supported product CLI is the invitation authority for native runs.
        invitation_path = root / "cli-invitation.json"
        invitation_fd = os.open(invitation_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(invitation_fd, "wb") as invitation_output:
            paired = subprocess.run([str(binary), "--config", descriptor["config_path"], "pair", "--json", "--label", "Synthetic interop fixture", "--expires-in-seconds", "900"],
                                    stdin=subprocess.DEVNULL, stdout=invitation_output, stderr=subprocess.PIPE, timeout=60)
        if paired.returncode:
            raise RuntimeError("supported CLI pairing failed")
        invitation = read_private_json(invitation_path)
        if invitation.get("endpoint") != descriptor["endpoint"] or "pairingId" not in invitation:
            raise ValueError("supported CLI invitation mismatch")
        scenario_path = root / "scenario.json"
        scenario_bytes = (Path(__file__).resolve().parents[2] / "tests/interop/scenario.json").read_bytes()
        scenario_fd = os.open(scenario_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(scenario_fd, "wb") as scenario_output:
            scenario_output.write(scenario_bytes)
        descriptor.update(invitation_path=str(invitation_path), profile_id=config["profile_id"],
                          profile_path=config["profile_path"], profile_sha256=config["profile_sha256"],
                          scenario_path=str(scenario_path), scenario_sha256=hashlib.sha256(scenario_bytes).hexdigest(),
                          update_request_path=str(root / "advance.request"), update_receipt_path=str(root / "advance.json"))
        log_fd = os.open(root / "serve.log", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(log_fd, "wb") as log:
            process = subprocess.Popen([str(binary), "--config", descriptor["config_path"], "serve"],
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=log)
            wait_ready(process, descriptor)
            ready = dict(descriptor, **ready_process_fields(process, root),
                         binary_sha256=binary_hash,
                         binary_path=str(binary), seed_binary_path=str(seed_binary),
                         seed_binary_sha256=seed_hash,
                         provenance="synthetic-real-process", status="ready")
            write_private_json(root / "ready.json", ready)
            print(json.dumps({"status": "ready", "descriptor": str(root / "ready.json")}), flush=True)
            until = time.monotonic() + config["lifetime_seconds"]
            advanced = False
            while not stop.wait(0.25) and time.monotonic() < until:
                if not advanced and (root / "advance.request").is_file():
                    advanced = True
                    update = subprocess.run([str(seed_binary), "--advance", str(root)], stdin=subprocess.DEVNULL,
                                            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=30)
                    if update.returncode:
                        raise RuntimeError("owned synthetic update failed")
                if process.poll() is not None:
                    raise RuntimeError("Hub exited during fixture lifetime")
        return 0
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        if process is not None:
            root = Path(config["output_dir"])
            write_private_json(root / "stopped.json", {"hub_exit_code": process.returncode, "hub_pid": process.pid})
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    args = parser.parse_args()
    try:
        return run(load_config(args.config))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        # Do not echo loaded configuration or child output into public logs.
        print("fixture failed; inspect the private run evidence", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
