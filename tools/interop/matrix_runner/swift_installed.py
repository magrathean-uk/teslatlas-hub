# SPDX-License-Identifier: AGPL-3.0-only
"""Source-fixed bindings for the reviewed Swift installed adapter.

This module intentionally contains the topology and reviewed-source identity
before any installed row is admitted.  Runtime inputs remain private and are
validated by the later builder/launcher hooks rather than by matrix JSON.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import ssl
import stat
import subprocess
import sys
import threading
import time
from types import MappingProxyType
from typing import Any, Mapping

from . import installed
from . import swift_entrypoint
from .adapter_wire import LoadedContract, ReviewedContract, strict_json
from .process_generation import OwnedProcess
from .swift_entrypoint import ReviewedSwiftAdapter, ReviewedSwiftImport
try:
    from ..installed_hosts.bounded import Deadline
except ImportError:  # pragma: no cover - top-level matrix_runner entrypoint
    from installed_hosts.bounded import Deadline


WORKSPACE = Path(__file__).resolve().parents[4]
SDK_ROOT = WORKSPACE / "teslatlas-sdk-swift"

CONTRACT = ReviewedContract(
    str(SDK_ROOT / "tools/matrix-contract.json"),
    "8057eae6af5dfeb55e14544c2a8fa10cdf132f40638fa6b2072c78004dcef823",
    str(SDK_ROOT / "tools/matrix_contract.py"),
    "1019fbfc64c91038dbc68025bf31fb9483abb0cba13588440ab0861892b083df",
)

REVIEWED_ADAPTER = ReviewedSwiftAdapter(
    str(SDK_ROOT / "tools/matrix_live.py"),
    "a7242473cfb091db8cf812c56314e28ebaf559859b1d1e4981f81534ff92ff05",
    (
        ReviewedSwiftImport(
            "matrix_contract", str(SDK_ROOT / "tools/matrix_contract.py"),
            "1019fbfc64c91038dbc68025bf31fb9483abb0cba13588440ab0861892b083df",
        ),
        ReviewedSwiftImport(
            "matrix_wire", str(SDK_ROOT / "tools/matrix_wire.py"),
            "fa9cf4abfd072c1e167568aff6f1efb35b0078a228cadf94bf7f834da5d05422",
        ),
    ),
)

PHASE_CONTRACTS = MappingProxyType({
    "swift_macos": (
        SDK_ROOT / "tools/swift_macos-phases.json",
        "8f4669502b4bbfa87d0ed6d9d2c42f3344306d1f8eba913cf14699a1193372f0",
    ),
    "swift_linux": (
        SDK_ROOT / "tools/swift_linux-phases.json",
        "7f2250e31f3200c24a041a6bb7f7cc4519fcb39c173f31508a8d2b11a2b779c7",
    ),
})

PROFILE_MEMBERS = (
    "SHA256SUMS", "auth.schema.json", "cases.json", "discovery.schema.json",
    "errors.schema.json", "examples/claim.json", "examples/current.json",
    "examples/discovery.json", "examples/drives.json", "examples/health.json",
    "examples/invitation.json", "examples/ready.json", "examples/vehicles.json",
    "field-semantics.json", "openapi.json", "profile.json",
    "resources.schema.json", "sync-regression.json",
)

ENVIRONMENT_PATHS = MappingProxyType({
    "product_root": "TESLATLAS_SWIFT_PRODUCT_ROOT",
    "product_manifest": "TESLATLAS_SWIFT_PRODUCT_MANIFEST",
    "swift_macos_manifest": "TESLATLAS_SWIFT_MACOS_INPUT_MANIFEST",
    "swift_linux_manifest": "TESLATLAS_SWIFT_LINUX_INPUT_MANIFEST",
})


def execution_by_target():
    """Return the reviewed worker transport for each Hub target."""
    return (
        ("macos_arm64", "local"),
        ("debian13_amd64", "docker_exec_pipe"),
        ("debian13_arm64", "docker_exec_pipe"),
    )


def broker_kind_by_target():
    """The Swift coordinator owns one Unix broker across both workers."""
    return (
        ("macos_arm64", "unix"),
        ("debian13_amd64", "unix"),
        ("debian13_arm64", "unix"),
    )


def controller_observations(session, session_input, deadline):
    """Expose only runner-owned, immutable observations to the coordinator."""
    view = installed.build_controller_admission_view(session, session_input, deadline=deadline)
    observations = view.get("observations") if hasattr(view, "get") else None
    if not isinstance(observations, dict) and not hasattr(observations, "items"):
        raise SwiftInstalledPending("runner controller observations are unavailable")
    return {sequence: dict(value) for sequence, value in observations.items()}


def run_coordinator(session_input_path, session_input, session, deadline, workers):
    """Run reviewed coordinator bytes with runner-owned worker authority."""
    if set(workers) != {"swift_macos", "swift_linux"}:
        raise SwiftInstalledPending("fixed Swift worker registry is unavailable")
    capability = swift_entrypoint.SwiftLauncherCapability(
        workers=workers,
        observations=lambda: controller_observations(session, session_input, deadline),
        session_id=session_input["session_id"], deadline=deadline,
    )
    return swift_entrypoint.run(session_input_path, capability, REVIEWED_ADAPTER)


class SwiftInstalledPending(RuntimeError):
    pass


class SwiftCoordinatorProcess(subprocess.Popen):
    """A real owned process whose lifetime is closed over the root coordinator."""

    _SENTINEL = (
        "import os,sys; fd=int(sys.argv[1]); raw=os.read(fd,1); os.close(fd); "
        "raise SystemExit(raw[0] if len(raw)==1 else 1)"
    )

    def __init__(self, target):
        read_fd, write_fd = os.pipe()
        try:
            super().__init__(
                [sys.executable, "-c", self._SENTINEL, str(read_fd)],
                pass_fds=(read_fd,), stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                start_new_session=True, close_fds=True,
            )
        except BaseException:
            os.close(read_fd)
            os.close(write_fd)
            raise
        os.close(read_fd)
        self.coordinator_error = None
        self._coordinator_thread = threading.Thread(
            target=self._run_coordinator, args=(target, write_fd),
            name="teslatlas-swift-coordinator", daemon=False,
        )
        self._coordinator_thread.start()

    def _run_coordinator(self, target, write_fd):
        status = 1
        try:
            status = 0 if target() == 0 else 1
        except BaseException as error:
            self.coordinator_error = error
        try:
            os.write(write_fd, bytes((status,)))
        except OSError:
            pass
        finally:
            os.close(write_fd)

    def wait(self, timeout=None):
        started = time.monotonic()
        result = super().wait(timeout=timeout)
        remaining = None if timeout is None else max(0, timeout - (time.monotonic() - started))
        self._coordinator_thread.join(timeout=remaining)
        if self._coordinator_thread.is_alive():
            raise subprocess.TimeoutExpired(self.args, timeout)
        return result


def start_coordinator_process(target):
    if not callable(target):
        raise SwiftInstalledPending("Swift coordinator target is unavailable")
    return SwiftCoordinatorProcess(target)


def _remaining_seconds(end: float) -> float:
    remaining = end - time.monotonic()
    if remaining <= 0:
        raise SwiftInstalledPending("Swift worker deadline expired")
    return remaining


def _wait_for_worker_file(path: Path, process: subprocess.Popen, end: float) -> None:
    while not path.exists():
        if process.poll() is not None:
            raise SwiftInstalledPending("Swift worker exited before publishing its phase")
        time.sleep(min(0.005, _remaining_seconds(end)))


def run_worker_process(argv, cwd, environment, worker_config_binding,
                       phase_callback, runtime, *, timeout_seconds):
    """Run one fixed XCTest command and bridge its retained phase files."""
    if (not isinstance(argv, tuple) or not argv
            or not all(isinstance(item, str) and item for item in argv)
            or not isinstance(cwd, Path) or not cwd.is_absolute() or not cwd.is_dir()
            or not isinstance(environment, dict)
            or not all(isinstance(key, str) and key and isinstance(value, str)
                       for key, value in environment.items())
            or not callable(phase_callback) or not isinstance(runtime, Mapping)
            or type(timeout_seconds) is not int or not 1 <= timeout_seconds <= 3600):
        raise SwiftInstalledPending("Swift worker launch contract is invalid")
    config_path, config_raw = _bound_source(
        worker_config_binding, "Swift worker config", 1_048_576,
    )
    try:
        config = strict_json(config_raw)
        phase_stage = config["phase_contract"]
        phase_binding = phase_stage["local"]
        _phase_path, phase_raw = _bound_source(
            phase_binding, "Swift worker phase contract", 1_048_576,
        )
        phase_contract = strict_json(phase_raw)
        phases = phase_contract["phases"]
        remaining_cell_ms = config["remaining_cell_ms"]
    except Exception as error:
        raise SwiftInstalledPending("Swift worker configuration is invalid") from error
    if (not isinstance(config, Mapping) or not isinstance(phases, list) or not phases
            or phase_contract.get("actor_id") != config.get("actor_id")
            or type(remaining_cell_ms) is not int or remaining_cell_ms <= 0):
        raise SwiftInstalledPending("Swift worker configuration is incomplete")
    end = time.monotonic() + min(timeout_seconds, remaining_cell_ms / 1000)
    coordination = _canonical(Path(config["coordination_dir"]), "Swift coordination directory")
    log_path = _canonical(Path(config["log_path"]), "Swift worker log")
    evidence_path = _canonical(Path(config["evidence_path"]), "Swift worker evidence")
    log_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    log_handle = log_path.open("xb")
    os.chmod(log_path, 0o600)
    process = None
    owner = None
    try:
        process = subprocess.Popen(
            argv, cwd=str(cwd), env=environment, stdin=subprocess.DEVNULL,
            stdout=log_handle, stderr=subprocess.STDOUT,
            start_new_session=True, close_fds=True,
        )
        owner = OwnedProcess(process, Deadline(_remaining_seconds(end)))
        for ordinal, phase in enumerate(phases, 1):
            if (not isinstance(phase, Mapping) or phase.get("ordinal") != ordinal
                    or not isinstance(phase.get("phase_id"), str)):
                raise SwiftInstalledPending("Swift worker phase order is invalid")
            ready_path = coordination / ("worker-ready-%06d.json" % ordinal)
            _wait_for_worker_file(ready_path, process, end)
            ready_raw = _read_regular(
                ready_path, "Swift retained WorkerReady", 1_048_576,
            )
            ready_binding = {"path": str(ready_path), "sha256": _digest(ready_raw)}
            acknowledgement = phase_callback(ready_binding)
            if (not isinstance(acknowledgement, Mapping)
                    or acknowledgement.get("ready_sha256") != ready_binding["sha256"]
                    or acknowledgement.get("sequence") != ordinal
                    or acknowledgement.get("actor_id") != config.get("actor_id")
                    or acknowledgement.get("phase") != phase["phase_id"]):
                raise SwiftInstalledPending("Swift worker acknowledgement is invalid")
            _write_bytes(
                coordination / ("worker-ack-%06d.json" % ordinal),
                _json_bytes(acknowledgement),
            )
        exit_code = process.wait(timeout=_remaining_seconds(end))
        if exit_code != 0:
            raise SwiftInstalledPending("Swift XCTest worker failed")
        owner.require_absent(Deadline(_remaining_seconds(end)))
    except subprocess.TimeoutExpired as error:
        raise SwiftInstalledPending("Swift worker deadline expired") from error
    finally:
        if process is not None and (process.poll() is None or owner is not None):
            if owner is not None:
                try:
                    owner.cleanup(Deadline(45))
                except BaseException:
                    installed._terminate(process)
            elif process.poll() is None:
                installed._terminate(process)
        log_handle.close()
    evidence_raw = _read_regular(
        evidence_path, "Swift worker evidence", 8_388_608,
    )
    log_raw = _read_regular(log_path, "Swift worker log", 8_388_608)
    return {
        "schema_version": 1, "kind": "swift-worker-outcome",
        "actor_id": config["actor_id"], "status": "passed",
        "framework_exit_code": 0, "timed_out": False,
        "evidence": {"path": str(evidence_path), "sha256": _digest(evidence_raw)},
        "log": {"path": str(log_path), "sha256": _digest(log_raw)},
        "runtime": dict(runtime),
        "cleanup": {
            "process_exited": True, "stdout_closed": True, "stderr_closed": True,
        },
    }


def _digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _canonical(path: Path, label: str) -> Path:
    if not path.is_absolute() or path.resolve(strict=False) != path:
        raise SwiftInstalledPending(label + " path is not canonical")
    return path


def _read_regular(path: Path, label: str, maximum: int = 8_388_608, *, private: bool = True) -> bytes:
    path = _canonical(path, label)
    try:
        before = path.lstat()
    except OSError as error:
        raise SwiftInstalledPending(label + " is unavailable") from error
    if (not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
            or before.st_uid != os.getuid() or before.st_size > maximum
            or private and stat.S_IMODE(before.st_mode) & 0o077):
        raise SwiftInstalledPending(label + " is not a private regular file")
    raw = path.read_bytes()
    after = path.lstat()
    if (len(raw) != before.st_size
            or (before.st_dev, before.st_ino, before.st_mtime_ns)
            != (after.st_dev, after.st_ino, after.st_mtime_ns)):
        raise SwiftInstalledPending(label + " changed while reading")
    return raw


def _json_bytes(value: Mapping[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def _read_json(path: Path, label: str) -> Mapping[str, Any]:
    try:
        value = strict_json(_read_regular(path, label, 1_048_576))
    except Exception as error:
        raise SwiftInstalledPending(label + " is invalid") from error
    if not isinstance(value, Mapping):
        raise SwiftInstalledPending(label + " is not an object")
    return value


def _write_bytes(path: Path, raw: bytes) -> Mapping[str, str]:
    path = _canonical(path, "staged file")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError as error:
        raise SwiftInstalledPending("staged file path is not fresh") from error
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        try:
            path.unlink()
        except OSError:
            pass
        raise
    return {"path": str(path), "sha256": _digest(raw)}


def _stage(root: Path, identifier: str, raw: bytes, suffix: str = ".bin",
           relative: Path | None = None) -> Mapping[str, Any]:
    leaf = relative if relative is not None else Path(identifier.replace("/", "_") + suffix)
    return {
        "id": identifier,
        "root": _write_bytes(root / "root" / leaf, raw),
        "local": _write_bytes(root / "local" / leaf, raw),
    }


def _bound_source(binding: Mapping[str, Any], label: str, maximum: int = 8_388_608) -> tuple[Path, bytes]:
    if not isinstance(binding, Mapping) or set(binding) != {"path", "sha256"}:
        raise SwiftInstalledPending(label + " binding is invalid")
    path = _canonical(Path(binding["path"]), label)
    raw = _read_regular(path, label, maximum)
    if _digest(raw) != binding["sha256"]:
        raise SwiftInstalledPending(label + " digest changed")
    return path, raw


def _profile_inputs(profile: Mapping[str, Any], stage: Path):
    manifest_path, manifest_raw = _bound_source(
        {"path": profile.get("path"), "sha256": profile.get("sha256")},
        "profile manifest", 1_048_576,
    )
    if manifest_path.name != "SHA256SUMS":
        raise SwiftInstalledPending("profile manifest is not SHA256SUMS")
    try:
        checksums = {
            name: digest
            for digest, name in (line.split("  ", 1) for line in manifest_raw.decode("utf-8").splitlines())
        }
    except (UnicodeError, ValueError) as error:
        raise SwiftInstalledPending("profile manifest syntax is invalid") from error
    if set(checksums) != set(PROFILE_MEMBERS) - {"SHA256SUMS"}:
        raise SwiftInstalledPending("profile member set is incomplete")
    members = []
    for index, name in enumerate(PROFILE_MEMBERS, 1):
        raw = manifest_raw if name == "SHA256SUMS" else _read_regular(
            manifest_path.parent / name, "profile member", 1_048_576,
        )
        if name != "SHA256SUMS" and _digest(raw) != checksums[name]:
            raise SwiftInstalledPending("profile member digest differs from manifest")
        identifier = "profile_member_{:02d}".format(index)
        members.append(_stage(
            stage / "profile", identifier, raw,
            relative=Path("hub-http-v1") / "1.0.0" / name,
        ))
    manifest = next(item for item, name in zip(members, PROFILE_MEMBERS) if name == "SHA256SUMS")
    return manifest, members


def _selected_artifact(job: Mapping[str, Any]) -> tuple[Mapping[str, Any], bytes]:
    selected = [
        item for item in job.get("artifacts", [])
        if isinstance(item, Mapping) and item.get("role") == "swift_sdk_product"
    ]
    if len(selected) != 1 or selected[0].get("embedded_version") != "2026.36.2":
        raise SwiftInstalledPending("selected Swift product is unavailable")
    artifact = selected[0]
    path = _canonical(Path(artifact.get("path", "")), "Swift product")
    raw = _read_regular(path, "Swift product", 1_073_741_824)
    if _digest(raw) != artifact.get("sha256"):
        raise SwiftInstalledPending("Swift product digest changed")
    return artifact, raw


def _environment(job: Mapping[str, Any]) -> Mapping[str, str]:
    value = _read_json(Path(job["environment_file"]), "Swift installed environment")
    if any(not isinstance(key, str) or not isinstance(item, str) for key, item in value.items()):
        raise SwiftInstalledPending("Swift installed environment is invalid")
    return value


def _environment_path(environment: Mapping[str, str], name: str) -> Path:
    key = ENVIRONMENT_PATHS[name]
    value = environment.get(key)
    if not isinstance(value, str) or not value.startswith("/") or any(mark in value for mark in ("\x00", "\n", "\r")):
        raise SwiftInstalledPending("pending: " + key + " is unavailable")
    return _canonical(Path(value), key)


def _actor_manifest(path: Path, label: str) -> tuple[Mapping[str, Any], bytes]:
    raw = _read_regular(path, label, 8_388_608)
    try:
        value = strict_json(raw)
    except Exception as error:
        raise SwiftInstalledPending(label + " is invalid") from error
    required = {"schema_version", "build_record", "files"}
    if (not isinstance(value, Mapping) or set(value) != required
            or value.get("schema_version") != 1 or not isinstance(value.get("files"), list)
            or not value["files"]):
        raise SwiftInstalledPending(label + " is incomplete")
    return value, raw


def _private_root(descriptor: Mapping[str, Any], cell_id: str) -> Path:
    broker = _canonical(Path(descriptor["broker_socket"]), "broker socket")
    root = broker.parent / ("swift-" + cell_id + "-" + descriptor["session_id"])
    root.mkdir(mode=0o700, exist_ok=False)
    return root


def build_session_input(job, cell, config, matrix, contract: LoadedContract, descriptor, running):
    """Stage a closed Swift SessionInput from fixed source and private inputs."""
    del matrix
    if (not isinstance(running, Mapping)
            or not isinstance(running.get("descriptor"), Mapping)
            or contract.manifest.get("adapter_id") != "swift"):
        raise SwiftInstalledPending("installed Hub descriptor is unavailable")
    rich = running["descriptor"]
    session_config = _read_json(
        Path(job["installed_session"]["config"]["path"]), "installed session config",
    )
    environment = _environment(job)
    stage = _private_root(descriptor, cell["id"])
    profile_manifest, profile_members = _profile_inputs(config["profile"], stage)

    scenario_path, scenario_raw = _bound_source(session_config["scenario"], "scenario", 1_048_576)
    del scenario_path
    scenario = _stage(stage / "inputs", "scenario", scenario_raw, ".json")
    certificate_path = _canonical(Path(rich["certificate_path"]), "certificate")
    certificate_raw = _read_regular(certificate_path, "certificate", 1_048_576)
    certificate = _stage(stage / "inputs", "certificate", certificate_raw, ".pem")
    try:
        certificate_der = ssl.PEM_cert_to_DER_cert(certificate_raw.decode("ascii"))
        certificate_der_sha256 = _digest(
            bytes.fromhex(certificate_der) if isinstance(certificate_der, str) else certificate_der,
        )
    except (UnicodeError, ValueError, ssl.SSLError) as error:
        raise SwiftInstalledPending("certificate is not PEM") from error

    artifact, artifact_raw = _selected_artifact(job)
    product = _stage(stage / "products", "swift_sdk_product", artifact_raw, ".tar.gz")
    product_root = _environment_path(environment, "product_root")
    inventory_source = _read_json(
        _environment_path(environment, "product_manifest"), "Swift product inventory",
    )
    inventory = validate_installed_product(product_root, artifact, inventory_source)
    product_manifest = _stage(
        stage / "products", "swift_sdk_manifest",
        _json_bytes({
            "schema_version": inventory["schema_version"],
            "artifact_sha256": inventory["artifact_sha256"],
            "files": inventory["files"],
        }), ".json",
    )

    actors = []
    for actor_id, execution, runtime_ref, entrypoint in (
        ("swift_macos", "local_worker", "swift_macos", "swift_current_native_test"),
        ("swift_linux", "docker_worker", "swift_container", "swift_current_linux_test"),
    ):
        manifest_path = _environment_path(environment, actor_id + "_manifest")
        _manifest, manifest_raw = _actor_manifest(manifest_path, actor_id + " input manifest")
        actor_manifest = _stage(stage / "actors", actor_id + "_manifest", manifest_raw, ".json")
        phase_path, expected_digest = PHASE_CONTRACTS[actor_id]
        phase_raw = _read_regular(phase_path, actor_id + " phase contract", 1_048_576, private=False)
        if _digest(phase_raw) != expected_digest:
            raise SwiftInstalledPending("pending: reviewed Swift phase contract changed")
        phase_contract = _stage(stage / "actors", actor_id + "_phases", phase_raw, ".json")
        actors.append({
            "id": actor_id, "kind": "current_swift_transport",
            "execution": execution, "runtime_ref": runtime_ref,
            "artifact_roles": ["swift_sdk_product"],
            "source_roles": ["swift_sdk_source"], "entrypoint_ref": entrypoint,
            "input_manifest": actor_manifest, "phase_contract": phase_contract,
        })

    header = {
        "schema_version": 1, "execution_kind": "actual_hub_acceptance",
        "adapter": job["adapter"], "cell_id": job["cell_id"],
        "product_version": config["product_version"],
        "profile_id": config["profile"]["id"],
        "profile_revision": config["profile"]["revision"],
        "profile_sha256": config["profile"]["sha256"],
        "source_identities": job["source_identities"],
        "artifacts": job["artifacts"], "runtime": job["runtime"],
    }
    header_stage = _stage(stage / "inputs", "header", _json_bytes(header), ".json")
    contract_raw = _read_regular(
        Path(CONTRACT.manifest_path), "reviewed Swift contract", 1_048_576, private=False,
    )
    if _digest(contract_raw) != CONTRACT.manifest_sha256:
        raise SwiftInstalledPending("pending: reviewed Swift contract changed")
    contract_stage = _stage(stage / "inputs", "case_contract", contract_raw, ".json")

    output_root = stage / "outputs"
    output_root.mkdir(mode=0o700)
    outputs = {
        "normalized": str(output_root / "normalized.json"),
        "actor_evidence": str(output_root / "actor-evidence.json"),
        "coordination_dir": str(output_root / "coordination"),
        "framework_log": str(output_root / "framework.log"),
    }
    value = {
        "schema_version": 1, "kind": "matrix-adapter-session",
        "run_id": session_config["run_id"], "cell_id": cell["id"],
        "adapter_id": "swift", "client_id": "swift",
        "session_id": descriptor["session_id"], "instance_nonce": os.urandom(32).hex(),
        "header": header_stage, "case_contract": contract_stage,
        "host_session": descriptor,
        "broker": {"kind": "unix", "socket_path": descriptor["broker_socket"]},
        "inputs": {
            "profile_manifest": profile_manifest, "profile_members": profile_members,
            "scenario": scenario, "certificate": certificate,
            "certificate_der_sha256": certificate_der_sha256,
            "product_inputs": [{
                "artifact_role": "swift_sdk_product", "staged": product,
                "installed_manifest": product_manifest, "local_root": str(product_root),
            }],
        },
        "actors": actors, "outputs": outputs,
        "bounds": {
            "cell_timeout_ms": job["timeout_seconds"] * 1000,
            "cleanup_timeout_ms": 45000, "frame_bytes": 1048576,
            "evidence_bytes": 8388608, "framework_log_bytes": 8388608,
        },
    }
    path = stage / "session-input.json"
    _write_bytes(path, _json_bytes(value))
    return path, value


def validate_product_inventory(artifact, inventory):
    """Accept only the runner's exact installed Swift product inventory."""
    required = {"schema_version", "artifact_sha256", "package_name", "version", "files"}
    if (not isinstance(artifact, Mapping) or artifact.get("role") != "swift_sdk_product"
            or not isinstance(inventory, Mapping) or set(inventory) != required
            or inventory["schema_version"] != 1
            or inventory["artifact_sha256"] != artifact.get("sha256")
            or inventory["package_name"] != "teslatlas-sdk-swift"
            or inventory["version"] != artifact.get("embedded_version")
            or not isinstance(inventory["files"], list) or not inventory["files"]):
        raise SwiftInstalledPending("Swift installed product inventory is unavailable")
    previous = None
    for item in inventory["files"]:
        if (not isinstance(item, Mapping) or set(item) != {"path", "bytes", "mode", "sha256"}
                or not isinstance(item["path"], str) or item["path"].startswith("/")
                or ".." in Path(item["path"]).parts or previous is not None and item["path"] <= previous
                or type(item["bytes"]) is not int or item["bytes"] < 0
                or type(item["mode"]) is not int or not re.fullmatch(r"[0-9a-f]{64}", str(item["sha256"]))):
            raise SwiftInstalledPending("Swift installed product inventory is invalid")
        previous = item["path"]
    return inventory


def validate_installed_product(root: Path, artifact, inventory):
    """Rehash every claimed installed member beneath the admitted product root."""
    inventory = validate_product_inventory(artifact, inventory)
    root = _canonical(root, "Swift installed product root")
    try:
        root_info = root.lstat()
    except OSError as error:
        raise SwiftInstalledPending("Swift installed product root is unavailable") from error
    if not stat.S_ISDIR(root_info.st_mode) or root_info.st_uid != os.getuid():
        raise SwiftInstalledPending("Swift installed product root is invalid")
    for item in inventory["files"]:
        member = _canonical(root / item["path"], "Swift installed product member")
        raw = _read_regular(member, "Swift installed product member", private=False)
        mode = stat.S_IMODE(member.lstat().st_mode)
        if (len(raw) != item["bytes"] or mode != item["mode"]
                or _digest(raw) != item["sha256"]):
            raise SwiftInstalledPending("Swift installed product member differs from inventory")
    return inventory


def _binding(path: Path, label: str, maximum: int = 8_388_608) -> Mapping[str, str]:
    """Return a fresh binding only after reading the owner-only regular file."""
    raw = _read_regular(path, label, maximum)
    return {"path": str(path), "sha256": _digest(raw)}


def _staged_json(binding: Mapping[str, Any], label: str, maximum: int = 8_388_608) -> Mapping[str, Any]:
    try:
        value = strict_json(_bound_source(binding, label, maximum)[1])
    except Exception as error:
        if isinstance(error, SwiftInstalledPending):
            raise
        raise SwiftInstalledPending(label + " is invalid") from error
    if not isinstance(value, Mapping):
        raise SwiftInstalledPending(label + " is not an object")
    return value


def _actor_spec(session_input: Mapping[str, Any], actor_id: str) -> Mapping[str, Any]:
    actors = session_input.get("actors")
    if not isinstance(actors, list):
        raise SwiftInstalledPending("Swift SessionInput actors are unavailable")
    selected = [item for item in actors if isinstance(item, Mapping) and item.get("id") == actor_id]
    if len(selected) != 1:
        raise SwiftInstalledPending("Swift SessionInput actor registry is incomplete")
    return selected[0]


def _worker_executable(environment: Mapping[str, str], actor_id: str) -> tuple[Path, Path]:
    prefix = "TESLATLAS_SWIFT_MACOS" if actor_id == "swift_macos" else "TESLATLAS_SWIFT_LINUX"
    executable_value = environment.get(prefix + "_WORKER_EXECUTABLE")
    cwd_value = environment.get(prefix + "_WORKER_CWD")
    if not isinstance(executable_value, str) or not isinstance(cwd_value, str):
        raise SwiftInstalledPending("pending: " + prefix + " fixed worker registration is unavailable")
    executable = _canonical(Path(executable_value), prefix + " worker executable")
    cwd = _canonical(Path(cwd_value), prefix + " worker cwd")
    try:
        executable_info = executable.lstat()
        cwd_info = cwd.lstat()
    except OSError as error:
        raise SwiftInstalledPending(prefix + " fixed worker path is unavailable") from error
    if (not stat.S_ISREG(executable_info.st_mode) or executable_info.st_uid != os.getuid()
            or stat.S_IMODE(executable_info.st_mode) & 0o022
            or not stat.S_ISDIR(cwd_info.st_mode) or cwd_info.st_uid != os.getuid()
            or stat.S_IMODE(cwd_info.st_mode) & 0o022):
        raise SwiftInstalledPending(prefix + " fixed worker path is not owner-controlled")
    return executable, cwd


def _worker_command(actor_id: str, config_path: Path, environment: Mapping[str, str]) -> tuple[tuple[str, ...], Path, dict[str, str]]:
    """Resolve only the source-fixed worker executable and its config env."""
    executable, cwd = _worker_executable(environment, actor_id)
    child_environment = dict(environment)
    child_environment["TESLATLAS_CURRENT_HUB_MATRIX_WORKER_CONFIG"] = str(config_path)
    # The registered worker is a direct executable.  It reads the config from
    # this fixed environment key; no job argv or shell is admitted here.
    return (str(executable),), cwd, child_environment


def _runtime_seed(actor_id: str, environment: Mapping[str, str]) -> Mapping[str, Any]:
    prefix = "TESLATLAS_SWIFT_MACOS" if actor_id == "swift_macos" else "TESLATLAS_SWIFT_LINUX"
    descriptor_value = environment.get(prefix + "_RUNTIME_DESCRIPTOR")
    if isinstance(descriptor_value, str):
        descriptor = _read_json(Path(descriptor_value), prefix + " runtime descriptor")
        if descriptor.get("runtime_ref") != ("swift_macos" if actor_id == "swift_macos" else "swift_container"):
            raise SwiftInstalledPending(prefix + " runtime descriptor has a foreign runtime")
        return descriptor
    return {
        "schema_version": 1,
        "runtime_ref": "swift_macos" if actor_id == "swift_macos" else "swift_container",
        "runtime_kind": "native-process" if actor_id == "swift_macos" else "docker-container",
    }


def _phase_admissions(actor_id: str, coordination: Path, phases: list[Mapping[str, Any]]) -> list[Mapping[str, Any]]:
    admissions = []
    for ordinal, phase in enumerate(phases, 1):
        ready_path = coordination / ("worker-ready-%06d.json" % ordinal)
        ready_raw = _read_regular(ready_path, "Swift retained WorkerReady", 1_048_576)
        ready = strict_json(ready_raw)
        if (not isinstance(ready, Mapping)
                or ready.get("actor_id") != actor_id
                or ready.get("sequence") != ordinal
                or not isinstance(ready.get("observation"), Mapping)
                or type(ready["observation"].get("session_sequence")) is not int):
            raise SwiftInstalledPending("Swift retained WorkerReady is invalid")
        before = ready["observation"]["session_sequence"]
        if ordinal < len(phases):
            next_ready = _read_json(
                coordination / ("worker-ready-%06d.json" % (ordinal + 1)),
                "Swift next WorkerReady",
            )
            after = next_ready.get("observation", {}).get("session_sequence")
        else:
            result = _read_json(
                coordination / ("worker-result-%06d.json" % ordinal),
                "Swift final worker result",
            )
            results = result.get("results")
            last = results[-1] if isinstance(results, list) and results else None
            proof = last.get("result", {}).get("proof") if isinstance(last, Mapping) else None
            after = proof.get("sequence") if isinstance(proof, Mapping) else None
        if type(after) is not int or after <= before:
            raise SwiftInstalledPending("Swift worker phase sequence is incomplete")
        admissions.append({
            "ordinal": ordinal,
            "phase_id": phase.get("phase_id"),
            "session_sequence_before": before,
            "session_sequence_after": after,
            "ready_sha256": _digest(ready_raw),
        })
    return admissions


def _complete_runtime(actor_id: str, seed: Mapping[str, Any], config: Mapping[str, Any]) -> Mapping[str, Any]:
    coordination = _canonical(Path(config["coordination_dir"]), "Swift worker coordination directory")
    phase_binding = config.get("phase_contract")
    phase = _staged_json(phase_binding.get("local"), "Swift worker phase contract") if isinstance(phase_binding, Mapping) else None
    phases = phase.get("phases") if isinstance(phase, Mapping) else None
    if not isinstance(phases, list) or len(phases) != 6:
        raise SwiftInstalledPending("Swift worker phase contract is incomplete")
    value = dict(seed)
    value["phase_admissions"] = _phase_admissions(actor_id, coordination, phases)
    return value


def _fixed_worker(job, cell, session_input, actor_id, session, deadline):
    def run(worker_config_binding, phase_callback):
        config_path, config_raw = _bound_source(
            worker_config_binding, "Swift worker config", 1_048_576,
        )
        config = strict_json(config_raw)
        if not isinstance(config, Mapping) or config.get("actor_id") != actor_id:
            raise SwiftInstalledPending("Swift worker config actor identity is invalid")
        environment = _environment(job)
        argv, cwd, child_environment = _worker_command(actor_id, config_path, environment)
        remaining_ms = config.get("remaining_cell_ms")
        if type(remaining_ms) is not int or remaining_ms <= 0:
            raise SwiftInstalledPending("Swift worker deadline is invalid")
        timeout_seconds = max(1, min(
            int(job.get("timeout_seconds", 3600)), max(1, remaining_ms // 1000),
        ))
        outcome = run_worker_process(
            argv, cwd, child_environment, worker_config_binding, phase_callback,
            _runtime_seed(actor_id, environment), timeout_seconds=timeout_seconds,
        )
        runtime = _complete_runtime(actor_id, outcome["runtime"], config)
        coordination = _canonical(Path(config["coordination_dir"]), "Swift worker coordination directory")
        _write_bytes(
            coordination / ("runtime-" + actor_id + ".json"), _json_bytes(runtime),
        )
        # The reviewed Swift launcher outcome has a closed key set; the
        # coordinator receives runtime as a value and the runner later
        # re-reads the retained file through runtime_inventory.
        outcome["runtime"] = runtime
        return outcome
    return run


def launch_adapter(
    job, cell, config, contract, session_input_path, session_input,
    *, deadline=None, session=None, running=None,
):
    """Launch only the reviewed coordinator with two source-fixed workers."""
    del config, contract, running
    if cell.get("client_id") != "swift" or not isinstance(session_input, Mapping):
        raise SwiftInstalledPending("Swift launch identity is invalid")
    if not callable(getattr(deadline, "remaining", None)):
        raise SwiftInstalledPending("Swift root deadline is unavailable")
    if {item.get("id") for item in session_input.get("actors", []) if isinstance(item, Mapping)} != {
        "swift_macos", "swift_linux"
    }:
        raise SwiftInstalledPending("Swift fixed actor registry is incomplete")
    framework = _canonical(Path(session_input["outputs"]["framework_log"]), "Swift coordinator log")
    stderr = Path(str(framework) + ".stderr.log")
    framework.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        for path in (framework, stderr):
            descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.close(descriptor)
    except OSError as error:
        raise SwiftInstalledPending("Swift coordinator logs are not fresh") from error
    workers = {
        actor_id: _fixed_worker(job, cell, session_input, actor_id, session, deadline)
        for actor_id in ("swift_macos", "swift_linux")
    }
    target = lambda: run_coordinator(
        session_input_path, session_input, session, deadline, workers,
    )
    return start_coordinator_process(target)


def _runtime_documents(session_input: Mapping[str, Any]) -> Mapping[str, Mapping[str, Any]]:
    coordination = _canonical(
        Path(session_input["outputs"]["coordination_dir"]),
        "Swift runtime coordination directory",
    )
    result = {}
    for actor_id, runtime_ref in (("swift_macos", "swift_macos"), ("swift_linux", "swift_container")):
        path = coordination / ("runtime-" + actor_id + ".json")
        raw = _read_regular(path, "Swift observed runtime", 8_388_608)
        try:
            runtime = strict_json(raw)
        except Exception as error:
            raise SwiftInstalledPending("Swift observed runtime is invalid") from error
        if (not isinstance(runtime, Mapping)
                or runtime.get("schema_version") != 1
                or runtime.get("runtime_ref") != runtime_ref
                or not isinstance(runtime.get("runtime_kind"), str)
                or not runtime["runtime_kind"]
                or not re.fullmatch(r"[0-9a-f]{64}", str(runtime.get("identity_sha256")))):
            raise SwiftInstalledPending("Swift observed runtime identity is invalid")
        result[actor_id] = {
            "binding": {"path": str(path), "sha256": _digest(raw)},
            "runtime": dict(runtime),
        }
    return result


def runtime_inventory(job, cell, config, contract, session_input, deadline=None):
    """Read both runner-retained worker runtimes; never echo job metadata."""
    del config, contract
    if deadline is not None:
        deadline.remaining()
    if cell.get("client_id") != "swift":
        raise SwiftInstalledPending("Swift runtime cell is foreign")
    actors = _runtime_documents(session_input)
    runtime_documents = {actor_id: value["runtime"] for actor_id, value in actors.items()}
    observed_actual = [
        runtime.get("matrix_runtime") for runtime in runtime_documents.values()
        if isinstance(runtime.get("matrix_runtime"), Mapping)
    ]
    if observed_actual and any(value != observed_actual[0] for value in observed_actual[1:]):
        raise SwiftInstalledPending("Swift observed runtime identities disagree")
    runtime_actual = (
        dict(observed_actual[0]) if observed_actual
        else {"actors": runtime_documents}
    )
    identity = _digest(_json_bytes(runtime_documents))
    result = {
        "schema_version": 1,
        "runtime_ref": "swift_dual_transport",
        "runtime_kind": "swift-installed-workers",
        "identity_sha256": identity,
        "actors": actors,
        "runtime_actual": runtime_actual,
    }
    if deadline is not None:
        deadline.remaining()
    return result


def _swift_context(contract, cell, session_input, actors, raw, runtime_documents, admission_views):
    actor_values = {}
    for actor_id in ("swift_macos", "swift_linux"):
        claim = actors[actor_id]
        runtime_value = runtime_documents[actor_id]["runtime"]
        actor_values[actor_id] = contract.module.AdmittedActor(
            id=claim["id"], kind=claim["kind"], runtime_ref=claim["runtime_ref"],
            entrypoint_ref=claim["entrypoint_ref"], artifact_roles=tuple(claim["artifact_roles"]),
            source_roles=tuple(claim["source_roles"]),
            installed_manifest=claim["installed_manifest"], runtime=runtime_value,
        )
    invocations = []
    for item in actors["_invocations"]:
        try:
            value = dict(item)
            value["request_ids"] = tuple(value["request_ids"])
            invocations.append(contract.module.AdmittedInvocation(**value))
        except (KeyError, TypeError) as error:
            raise SwiftInstalledPending("Swift invocation inventory is invalid") from error
    controller = admission_views.get("controller_observations") if isinstance(admission_views, Mapping) else None
    observations = controller.get("observations") if isinstance(controller, Mapping) else None
    if not isinstance(observations, Mapping):
        raise SwiftInstalledPending("runner-owned Swift controller observations are unavailable")
    return contract.module.AdmissionContext(
        adapter_id="swift", cell_id=cell["id"], session_id=session_input["session_id"],
        header=_staged_json(session_input["header"]["local"], "Swift evidence header"),
        scenario=_staged_json(session_input["inputs"]["scenario"]["local"], "Swift scenario"),
        actors=actor_values, invocations=tuple(invocations), raw=raw,
        controller_observations=observations,
    )


def admit(
    job, cell, config, matrix, contract, normalized, actors, *,
    admission_views=None, runtime_context=None, deadline=None,
):
    """Re-run every Swift case through the hash-bound semantic validator."""
    del job, config, matrix
    if deadline is not None:
        deadline.remaining()
    if not isinstance(admission_views, Mapping) or not isinstance(runtime_context, Mapping):
        raise SwiftInstalledPending("runner-owned Swift admission views are required")
    session_input = runtime_context.get("session_input")
    runtime_view = runtime_context.get("runtime_view")
    if not isinstance(session_input, Mapping) or not isinstance(runtime_view, Mapping):
        raise SwiftInstalledPending("runner-owned Swift runtime inventory is unavailable")
    actor_runtime = runtime_view.get("actors")
    if not isinstance(actor_runtime, Mapping):
        raise SwiftInstalledPending("Swift actor runtime inventory is unavailable")
    if (not isinstance(actors, Mapping)
            or set(actors) != {"schema_version", "session_id", "cell_id", "session_input_sha256", "actors", "invocations"}
            or actors.get("schema_version") != 1
            or actors.get("session_id") != session_input.get("session_id")
            or actors.get("cell_id") != cell.get("id")
            or not isinstance(actors.get("actors"), list)
            or not isinstance(actors.get("invocations"), list)):
        raise SwiftInstalledPending("Swift actor evidence is invalid")
    claims = {claim.get("id"): claim for claim in actors["actors"] if isinstance(claim, Mapping)}
    if (len(claims) != 2 or set(claims) != {"swift_macos", "swift_linux"}
            or len(claims) != len(actors["actors"])):
        raise SwiftInstalledPending("Swift actor evidence names a foreign actor")
    raw = {}
    for claim in claims.values():
        required = {"id", "kind", "runtime_ref", "entrypoint_ref", "artifact_roles", "source_roles", "installed_manifest", "raw_evidence"}
        if set(claim) != required or not isinstance(claim["raw_evidence"], list):
            raise SwiftInstalledPending("Swift actor evidence shape is invalid")
        for item in claim["raw_evidence"]:
            if (not isinstance(item, Mapping)
                    or set(item) != {"id", "schema_id", "binding"}
                    or item["schema_id"] != "swift-raw-v1"
                    or item["id"] in raw):
                raise SwiftInstalledPending("Swift raw evidence inventory is invalid")
            raw[item["id"]] = _staged_json(item["binding"], "Swift raw evidence")
    actors_for_context = dict(claims)
    actors_for_context["_invocations"] = actors["invocations"]
    runtime_documents = {
        actor_id: actor_runtime.get(actor_id)
        for actor_id in ("swift_macos", "swift_linux")
    }
    if any(not isinstance(value, Mapping) or not isinstance(value.get("runtime"), Mapping)
           for value in runtime_documents.values()):
        raise SwiftInstalledPending("Swift actor runtime evidence is incomplete")
    context = _swift_context(
        contract, cell, session_input, actors_for_context, raw,
        runtime_documents, admission_views,
    )
    if (not isinstance(normalized, Mapping)
            or set(normalized) != set(context.header) | {"cases"}
            or any(normalized.get(key) != context.header.get(key) for key in context.header)
            or not isinstance(normalized.get("cases"), list)):
        raise SwiftInstalledPending("Swift normalized evidence identity is invalid")
    cases = {item.get("id"): item for item in normalized["cases"] if isinstance(item, Mapping)}
    required_cases = list(contract.manifest["required_cases"])
    if len(cases) != len(normalized["cases"]) or list(cases) != required_cases:
        raise SwiftInstalledPending("Swift normalized case coverage is incomplete")
    admitted = []
    for case_id in required_cases:
        case = dict(cases[case_id])
        decision = contract.module.admit_case(case, context)
        expected = (
            decision.status == "pending" and decision.code == "runner_owned_service_runtime"
            if case_id == "installed_service_runtime"
            else decision.status == "passed" and decision.code == "accepted"
        )
        if not expected:
            raise SwiftInstalledPending("installed Swift case admission failed: " + case_id)
        admitted.append(case)
    if deadline is not None:
        deadline.remaining()
    return {"schema_version": 1, "adapter_id": "swift", "cases": admitted}


def _closed_result_evidence(result, session_input, cell):
    completion = result.completion
    if (not isinstance(completion, Mapping)
            or completion.get("session_id") != session_input.get("session_id")
            or completion.get("cell_id") != cell.get("id")
            or result.exit_code != 0):
        raise SwiftInstalledPending("retained Swift completion is invalid")
    evidence = result.session_evidence
    if (getattr(evidence, "state", None) != "closed"
            or tuple(getattr(evidence, "cleanup_errors", (None,))) != ()
            or not isinstance(getattr(evidence, "final_stopped", None), Mapping)
            or evidence.final_stopped.get("status") != "stopped"
            or not isinstance(evidence.final_stopped.get("service"), Mapping)
            or evidence.final_stopped["service"].get("state") != "stopped"):
        raise SwiftInstalledPending("runner-owned Swift close evidence is unavailable")
    return completion, evidence


def build_supplement(
    job, cell, config, matrix, contract, result, *, runtime_context=None,
):
    """Bind completion, both actor claims, controller close and cleanup."""
    del config, matrix
    if not isinstance(runtime_context, Mapping) or not isinstance(runtime_context.get("session_input"), Mapping):
        raise SwiftInstalledPending("runner Swift context is unavailable for supplement")
    session_input = runtime_context["session_input"]
    completion, evidence = _closed_result_evidence(result, session_input, cell)
    actor_evidence = _staged_json(completion.get("actor_evidence"), "retained Swift actor evidence")
    if not isinstance(actor_evidence.get("actors"), list):
        raise SwiftInstalledPending("retained Swift actor evidence is invalid")
    claims = {item.get("id"): item for item in actor_evidence["actors"] if isinstance(item, Mapping)}
    if set(claims) != {"swift_macos", "swift_linux"}:
        raise SwiftInstalledPending("retained Swift actor evidence lacks both actors")
    runtime_view = runtime_context.get("runtime_view")
    actor_runtime = runtime_view.get("actors") if isinstance(runtime_view, Mapping) else None
    if not isinstance(actor_runtime, Mapping):
        raise SwiftInstalledPending("Swift actor runtime inventory is unavailable for supplement")
    actor_rows = []
    for actor_id in ("swift_macos", "swift_linux"):
        runtime_row = actor_runtime.get(actor_id)
        if not isinstance(runtime_row, Mapping) or not isinstance(runtime_row.get("binding"), Mapping):
            raise SwiftInstalledPending("Swift actor runtime binding is unavailable")
        runtime_value = _staged_json(runtime_row["binding"], "retained Swift actor runtime")
        if runtime_value != runtime_row.get("runtime"):
            raise SwiftInstalledPending("retained Swift actor runtime changed")
        actor_rows.append({**dict(claims[actor_id]), "runtime_evidence": runtime_row["binding"]})
    coordination = _canonical(Path(session_input["outputs"]["coordination_dir"]), "Swift coordination directory")
    controller_view = runtime_context.get("controller_view")
    observations = controller_view.get("observations") if isinstance(controller_view, Mapping) else None
    if not isinstance(observations, Mapping):
        raise SwiftInstalledPending("runner-owned Swift controller observations are unavailable")
    observation_binding = _write_bytes(
        coordination / "controller-observations.json",
        _json_bytes({"schema_version": 1, "session_id": session_input["session_id"],
                     "observations": [_plain(observations[key]) for key in sorted(observations)]}),
    )
    journal_path = getattr(evidence, "journal_path", None)
    journal_sha256 = getattr(evidence, "journal_sha256", None)
    if not isinstance(journal_path, str) or not isinstance(journal_sha256, str):
        raise SwiftInstalledPending("retained Swift controller journal is unavailable")
    _bound_source({"path": journal_path, "sha256": journal_sha256}, "retained Swift controller journal")
    final_binding = _write_bytes(coordination / "final-stopped.json", _json_bytes(_plain(evidence.final_stopped)))
    transport = evidence.local_transport
    if not isinstance(transport, (list, tuple)) or not transport:
        raise SwiftInstalledPending("Swift transport cleanup evidence is unavailable")
    transport_binding = _write_bytes(
        coordination / "transport-cleanup.json",
        _json_bytes({"schema_version": 1, "session_id": session_input["session_id"],
                     "status": "passed", "resources": _plain(transport)}),
    )
    command_binding = _write_bytes(
        coordination / "command-outcome.json",
        _json_bytes({"schema_version": 1, "session_id": session_input["session_id"],
                     "cell_id": cell["id"], "exit_code": result.exit_code,
                     "outcome": "passed", "logs": []}),
    )
    adapter_completion = _binding(coordination / "adapter-completion.json", "Swift adapter completion", 1_048_576)
    if _staged_json(adapter_completion, "Swift adapter completion") != completion:
        raise SwiftInstalledPending("retained Swift adapter completion changed")
    ready = _binding(coordination / "ready-000001.json", "Swift retained Ready", 65_536)
    ack = _binding(coordination / "ack-000001.json", "Swift retained Ack", 65_536)
    invocations = actor_evidence.get("invocations")
    if not isinstance(invocations, list):
        raise SwiftInstalledPending("retained Swift invocation inventory is invalid")
    case_ids = list(contract.manifest.get("required_cases", [])) if isinstance(getattr(contract, "manifest", None), Mapping) else []
    case_bindings = [
        {"case_id": case_id,
         "actor_ids": sorted({item.get("actor_id") for item in invocations if item.get("case_id") == case_id}),
         "invocations": [dict(item) for item in invocations if item.get("case_id") == case_id]}
        for case_id in case_ids
    ]
    config_binding = job.get("installed_session", {}).get("config", {})
    supplement = {
        "schema_version": 2, "cell_id": cell["id"], "session_id": session_input["session_id"],
        "actors": actor_rows, "case_bindings": case_bindings,
        "controller_evidence": {
            "registration_sha256": session_input["host_session"]["registration_sha256"],
            "session_config_sha256": config_binding.get("sha256"),
            "observations": observation_binding,
            "journal": {"path": journal_path, "sha256": journal_sha256},
            "final_stopped": final_binding, "transport_cleanup": transport_binding,
        },
        "completion": {
            "adapter_completion": adapter_completion, "ready": ready, "ack": ack,
            "normalized": completion["normalized"], "actor_evidence": completion["actor_evidence"],
            "command_outcome": command_binding, "status": "passed",
        },
    }
    return _write_bytes(coordination / "installed-supplement.json", _json_bytes(supplement))


def execution_logs(job, cell, config, contract, result):
    """Bind the retained coordinator streams and fixed command record."""
    del job, config, contract
    completion = result.completion
    normalized = completion.get("normalized") if isinstance(completion, Mapping) else None
    if not isinstance(normalized, Mapping):
        raise SwiftInstalledPending("Swift completion lacks normalized output")
    output_root = _canonical(Path(normalized["path"]).parent, "Swift output root")
    framework = output_root / "framework.log"
    stderr = Path(str(framework) + ".stderr.log")
    stdout_binding = _binding(framework, "Swift coordinator stdout")
    stderr_binding = _binding(stderr, "Swift coordinator stderr")
    command_binding = _write_bytes(
        output_root / "command-record.json",
        _json_bytes({"argv": ["swift-fixed-coordinator", "swift_current_native_test", "swift_current_linux_test"],
                     "cwd": str(SDK_ROOT), "exit_code": result.exit_code,
                     "outcome": "passed" if result.exit_code == 0 else "failed"}),
    )
    return {
        "stdout": {**stdout_binding, "bytes": len(_read_regular(framework, "Swift coordinator stdout")), "truncated": False},
        "stderr": {**stderr_binding, "bytes": len(_read_regular(stderr, "Swift coordinator stderr")), "truncated": False},
        "command_record": command_binding, "duration_ms": 0,
    }


def _plain(value):
    if isinstance(value, Mapping):
        return {key: _plain(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(item) for item in value]
    return value
