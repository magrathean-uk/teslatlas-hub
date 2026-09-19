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
import resource
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
import uuid

MAX_CONFIG_BYTES = 64 * 1024
EDGE_PRIMARY_BASE_URL = "https://127.0.0.1:18510/"
EDGE_DOCKER_BASE_URL = "https://127.0.0.1:19443/"
EDGE_R1_DOCKER_BASE_URL = "https://127.0.0.1:20443/"
EDGE_R10_DOCKER_BASE_URL = "https://127.0.0.1:20543/"
EDGE_R12_DOCKER_BASE_URL = "https://127.0.0.1:20743/"
EDGE_R12_REPLACEMENT_DOCKER_BASE_URL = "https://127.0.0.1:20843/"
EDGE_R13_AUTHORIZED_DOCKER_BASE_URL = "https://127.0.0.1:20943/"
EDGE_ALLOWED_BASE_URLS = frozenset((
    EDGE_PRIMARY_BASE_URL,
    EDGE_DOCKER_BASE_URL,
    EDGE_R1_DOCKER_BASE_URL,
    EDGE_R10_DOCKER_BASE_URL,
    EDGE_R12_DOCKER_BASE_URL,
    EDGE_R12_REPLACEMENT_DOCKER_BASE_URL,
    EDGE_R13_AUTHORIZED_DOCKER_BASE_URL,
))
DEFAULT_SCENARIO_ID = "b1-five-drives"
SCENARIO_SOURCES = {
    DEFAULT_SCENARIO_ID: Path(__file__).resolve().parents[2] / "tests/interop/scenario.json",
    "viewer-r1-51-drives": Path(__file__).resolve().parents[2] / "tests/interop/scenario-viewer-r1-51-drives.json",
}
EDGE_COLLECTOR_FIELDS = {
    "base_url", "ca_certificate_path", "client_certificate_path",
    "client_private_key_path", "bearer_token_path", "installation_id",
    "lineage", "source_id", "vehicle_id", "vin", "car_id",
}

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
    allowed = {"binary", "seed_binary", "output_dir", "port", "lifetime_seconds", "profile_id", "profile_path", "profile_sha256", "allowed_origins", "edge_collector", "scenario_id"}
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
    scenario_source(config)
    if "edge_collector" in config:
        config["edge_collector"] = validate_edge_collector(config["edge_collector"])
    return config


def scenario_source(config):
    scenario_id = config.get("scenario_id", DEFAULT_SCENARIO_ID)
    if not isinstance(scenario_id, str) or scenario_id not in SCENARIO_SOURCES:
        raise ValueError("unknown scenario selector")
    source = SCENARIO_SOURCES[scenario_id]
    if not source.is_file():
        raise ValueError("scenario source is unavailable")
    return source


def copy_selected_scenario(root, config):
    source = scenario_source(config)
    raw = source.read_bytes()
    destination = Path(root) / "scenario.json"
    fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(raw)
    return destination, hashlib.sha256(raw).hexdigest()


def configure_allowed_origins(config_path, origins):
    if not origins:
        return
    path = Path(config_path)
    original = path.read_text()
    if "[http]" in original:
        raise ValueError("seeded fixture unexpectedly defines an HTTP section")
    with open(path, "a", encoding="utf-8") as stream:
        stream.write("[http]\nallowed_origins = %s\n" % json.dumps(origins, separators=(",", ":")))


def validate_edge_collector(value):
    if not isinstance(value, dict) or set(value) != EDGE_COLLECTOR_FIELDS:
        raise ValueError("invalid Edge collector binding")
    if value["base_url"] not in EDGE_ALLOWED_BASE_URLS:
        raise ValueError("Edge collector must use a reserved endpoint")
    for field in ("installation_id", "lineage"):
        item = value[field]
        if not isinstance(item, str) or not re.fullmatch(r"[a-z0-9._-]{1,128}", item):
            raise ValueError("invalid Edge collector binding")
    for field in ("source_id", "vehicle_id"):
        item = value[field]
        if not isinstance(item, str):
            raise ValueError("invalid Edge collector binding")
        try:
            if uuid.UUID(item).int == 0:
                raise ValueError("invalid Edge collector binding")
        except ValueError as error:
            raise ValueError("invalid Edge collector binding") from error
    vin = value["vin"]
    if (not isinstance(vin, str) or len(vin) != 17
            or not vin.isascii() or not vin.isalnum()
            or any(item in vin for item in "IOQ")
            or type(value["car_id"]) is not int or value["car_id"] <= 0):
        raise ValueError("invalid Edge collector binding")
    for field in ("ca_certificate_path", "client_certificate_path", "client_private_key_path", "bearer_token_path"):
        item = value[field]
        if not isinstance(item, str) or not Path(item).is_absolute():
            raise ValueError("invalid Edge collector private path")
        candidate = Path(item)
        try:
            metadata = candidate.lstat()
        except OSError as error:
            raise ValueError("invalid Edge collector private path") from error
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or metadata.st_nlink != 1 or metadata.st_mode & 0o077):
            raise ValueError("invalid Edge collector private path")
    paths = [value[field] for field in ("ca_certificate_path", "client_certificate_path", "client_private_key_path", "bearer_token_path")]
    if len(set(paths)) != len(paths):
        raise ValueError("invalid Edge collector private path")
    return value


def configure_edge_collector(config_path, edge):
    path = Path(config_path)
    original = path.read_text()
    collector = "[collector]\ninterval_seconds = 0\n"
    if original.count(collector) != 1 or "[collector.edge]" in original or not original.endswith("\n"):
        raise ValueError("seeded fixture has no disabled collector section")
    updated = original.replace("[collector]\n", '[collector]\nprovider = "fleet"\n', 1)
    fields = (
        "base_url", "ca_certificate_path", "client_certificate_path",
        "client_private_key_path", "bearer_token_path", "installation_id",
        "lineage", "source_id", "vehicle_id", "vin", "car_id",
    )
    with open(path, "w", encoding="utf-8") as stream:
        stream.write(updated)
        stream.write("[collector.edge]\n")
        for field in fields:
            stream.write("%s = %s\n" % (field, json.dumps(edge[field])))


def configure_seeded_config(config_path, config):
    if "edge_collector" in config:
        configure_edge_collector(config_path, config["edge_collector"])
    configure_allowed_origins(config_path, config["allowed_origins"])


def seed_command(config, seed_binary, output_dir, port):
    command = [str(seed_binary), "--output", str(output_dir), "--port", str(port)]
    if "edge_collector" in config:
        command.extend((
            "--source-id", config["edge_collector"]["source_id"],
            "--vehicle-id", config["edge_collector"]["vehicle_id"],
            "--vin", config["edge_collector"]["vin"],
            "--car-id", str(config["edge_collector"]["car_id"]),
        ))
    if "scenario_id" in config:
        command.extend(("--scenario", config["scenario_id"]))
    return command


def subprocess_diagnostic(stage, command, timeout):
    started = time.monotonic()
    limits = {}
    for name in ("RLIMIT_NOFILE", "RLIMIT_STACK", "RLIMIT_AS"):
        limit = getattr(resource, name, None)
        if limit is not None:
            soft, hard = resource.getrlimit(limit)
            limits[name.removeprefix("RLIMIT_").lower()] = {"soft": soft, "hard": hard}
    result = {
        "stage": stage,
        "command_sha256": hashlib.sha256("\0".join(map(str, command)).encode()).hexdigest(),
        "working_directory_sha256": hashlib.sha256(os.getcwd().encode()).hexdigest(),
        "environment_names_sha256": hashlib.sha256("\n".join(sorted(os.environ)).encode()).hexdigest(),
        "io": {"stdin": "inherited", "stdout": "devnull", "child_error": "captured_not_persisted"},
        "timeout_seconds": timeout,
        "limits": limits,
        "parent_maxrss": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
    }
    try:
        completed = subprocess.run(
            command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=timeout, umask=0o077
        )
    except subprocess.TimeoutExpired:
        result["termination"] = "timeout"
    except OSError as error:
        result["termination"] = "spawn_error"
        result["error_type"] = type(error).__name__
    else:
        if completed.returncode < 0:
            result["termination"] = "signal"
            result["signal"] = signal.Signals(-completed.returncode).name
        else:
            result["termination"] = "exit"
            result["exit_code"] = completed.returncode
    result["elapsed_ms"] = int((time.monotonic() - started) * 1000)
    return result


def write_fixture_diagnostic(output_dir, diagnostic):
    write_private_json(Path(output_dir).parent / "fixture-diagnostic.json", diagnostic)


def subprocess_failed(diagnostic):
    return diagnostic["termination"] != "exit" or diagnostic.get("exit_code") != 0


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
            seeded = subprocess_diagnostic(
                "seed", seed_command(config, seed_binary, config["output_dir"], port), 60
            )
            if subprocess_failed(seeded):
                write_fixture_diagnostic(config["output_dir"], seeded)
                raise RuntimeError("synthetic seed failed (%s)" % seeded["termination"])
        root = Path(config["output_dir"])
        descriptor = read_private_json(root / "connection.json")
        if descriptor["endpoint"] != "https://127.0.0.1:%d" % port:
            raise ValueError("seeded endpoint mismatch")
        configure_seeded_config(descriptor["config_path"], config)
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
        scenario_path, scenario_sha256 = copy_selected_scenario(root, config)
        descriptor.update(invitation_path=str(invitation_path), profile_id=config["profile_id"],
                          profile_path=config["profile_path"], profile_sha256=config["profile_sha256"],
                          scenario_path=str(scenario_path), scenario_sha256=scenario_sha256,
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
