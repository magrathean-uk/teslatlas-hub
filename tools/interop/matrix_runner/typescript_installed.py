# SPDX-License-Identifier: AGPL-3.0-only
"""Fixed installed-host adapters for the reviewed TypeScript SDK lanes.

The registry is deliberately boring.  It does not accept an executable or a
validator path from matrix JSON.  Those values are fixed here, and every
private input used by the lane is copied into a fresh owner-only staging tree
before the child is started.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tarfile
import tempfile
from types import MappingProxyType
from typing import Any, Mapping
import uuid

from . import installed
from .adapter_wire import (
    LoadedContract,
    ReviewedContract,
    file_binding,
    read_bound_file,
    strict_json,
    write_exclusive_json,
)


WORKSPACE = Path(__file__).resolve().parents[4]
SDK_ROOT = WORKSPACE / "teslatlas-sdk-typescript"
LANE_ROOT = WORKSPACE / "hub" / "tools" / "interop" / "client_lanes"
NODE = "typescript_node"
BROWSER = "typescript_browser"
SDK_TARBALL_SHA256 = "42348d3688c5a723bd154e3c1e8172bc07b20d1bf28944818ccfdbf3d97891f7"
SDK_MEMBER_COUNT = 83
SDK_PACKAGE_VERSION = "2026.36.2"

CONTRACTS = MappingProxyType({
    NODE: ReviewedContract(
        str(SDK_ROOT / "tools/matrix-contract-node.json"),
        "7800f75b03e046f53e56b7356fac95089aae73ab7719e82ead88f39e45ae3299",
        str(SDK_ROOT / "tools/matrix_contract_node.py"),
        "600e1cc363d309e163b87e19498e1daf4ba309c325aeb8402513168ff1c12ef2",
    ),
    BROWSER: ReviewedContract(
        str(SDK_ROOT / "tools/matrix-contract-browser.json"),
        "3a04e33415330d2d3ce53d489841f0a28b768d982d2af5f255323d2f0cf6acb9",
        str(SDK_ROOT / "tools/matrix_contract_browser.py"),
        "784d1618efdc6739248426d7ee6538772a3560c1569c903f75fb2af72755bf92",
    ),
})

# The lane is Hub-owned source.  A changed lane is pending until a new source
# review binds its bytes; a sibling contract digest alone cannot authorize it.
LANE_FILES = MappingProxyType({
    "run.mjs": "7faa39b56310c0a2cec1de36327743cae9700bb19cd1118993aed8cffb20afe3",
    "installed_contract.mjs": "9035e5141ac7581b0c3810e3ac4e3d752ff82034350c93e1080a58291da43d0b",
    "typescript_lane.mjs": "55833f7d8dc16434009d13c7398ddd40404fc836082e1bbe39083a9ac0addcf2",
    "scenarios.mjs": "f04aa42956d1476dc3fbd21eab60200bf12cf3e823fdddfcc472aaf0f85b2dcd",
    "node-worker.mjs": "f1dfea5570dd93906028b5a3214dc0aecf241f98a7ccc41b2a52e1f395f161bf",
    "browser.mjs": "2a698cb07adec2ec336b2ffb15208a62c135f721cfc43f34ce382a7302022677",
})

REQUIRED_ENV = {
    "package_root": (
        "TESLATLAS_SDK_PACKAGE_ROOT", "TESLATLAS_SDK_ROOT", "TS_SDK_PACKAGE_ROOT",
        "SDK_PACKAGE_ROOT", "package_root", "typescript_sdk_package_root",
    ),
    "remote_root": ("TESLATLAS_BROWSER_REMOTE_ROOT", "browser_remote_root"),
    "ssh_config": ("TESLATLAS_BROWSER_SSH_CONFIG", "browser_ssh_config"),
    "ssh_alias": ("TESLATLAS_BROWSER_SSH_ALIAS", "browser_ssh_alias"),
    "playwright_entry": ("TESLATLAS_PLAYWRIGHT_ENTRY", "playwright_entry"),
    "local_log": ("TESLATLAS_BROWSER_LOCAL_LOG", "browser_local_log"),
}


class TypeScriptInstalledPending(RuntimeError):
    """The fixed lane cannot be admitted from the supplied private inputs."""


def _digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _canonical(path: Path, label: str) -> Path:
    if not path.is_absolute() or path.resolve(strict=False) != path:
        raise TypeScriptInstalledPending(label + " path is not canonical")
    return path


def _read_regular(path: Path, label: str, maximum: int = 8_388_608, *, private: bool = True) -> bytes:
    path = _canonical(path, label)
    try:
        info = path.lstat()
    except OSError as error:
        raise TypeScriptInstalledPending(label + " is unavailable") from error
    if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
            or info.st_uid != os.getuid() or (private and stat.S_IMODE(info.st_mode) & 0o077)
            or info.st_size > maximum):
        raise TypeScriptInstalledPending(label + " is not a private regular file")
    raw = path.read_bytes()
    after = path.lstat()
    if (len(raw) != info.st_size or (info.st_dev, info.st_ino, info.st_mtime_ns)
            != (after.st_dev, after.st_ino, after.st_mtime_ns)):
        raise TypeScriptInstalledPending(label + " changed while reading")
    return raw


def _json_binding(path: Path, label: str) -> Mapping[str, str]:
    raw = _read_regular(path, label)
    return {"path": str(path), "sha256": _digest(raw)}


def _write_bytes(path: Path, raw: bytes) -> Mapping[str, str]:
    path = _canonical(path, "staged file")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError as error:
        raise TypeScriptInstalledPending("staged file path is not fresh") from error
    try:
        with os.fdopen(fd, "wb") as handle:
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


def _stage(stage: Path, identifier: str, raw: bytes, suffix: str = ".bin") -> Mapping[str, Any]:
    """Create distinct logical root/local copies with the same exact bytes."""
    safe = identifier.replace("/", "_")
    root = _write_bytes(stage / "root" / (safe + suffix), raw)
    local = _write_bytes(stage / "local" / (safe + suffix), raw)
    return {"id": identifier, "root": root, "local": local}


def _json_bytes(value: Mapping[str, Any]) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n").encode()


def _plain(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {key: _plain(item) for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [_plain(item) for item in value]
    return value


def _bound_json(path: Path, label: str) -> Mapping[str, Any]:
    try:
        value = strict_json(_read_regular(path, label, 1_048_576))
    except Exception as error:
        raise TypeScriptInstalledPending(label + " is invalid") from error
    if not isinstance(value, Mapping):
        raise TypeScriptInstalledPending(label + " is not an object")
    return value


def _env(job: Mapping[str, Any]) -> Mapping[str, str]:
    path = Path(job["environment_file"])
    value = _bound_json(path, "installed environment")
    if any(not isinstance(key, str) or not isinstance(item, str) for key, item in value.items()):
        raise TypeScriptInstalledPending("installed environment is invalid")
    return value


def _env_value(environment: Mapping[str, str], name: str, *, required: bool, path: bool = True) -> str | None:
    for key in REQUIRED_ENV[name]:
        value = environment.get(key)
        if value is not None:
            if (path and not value.startswith("/")) or (not path and (not value or value.startswith("/"))) or "\x00" in value or "\n" in value or "\r" in value:
                raise TypeScriptInstalledPending(name + " environment path is invalid")
            return value
    if required:
        raise TypeScriptInstalledPending("pending: " + name + " private input is unavailable")
    return None


def _source_path(binding: Mapping[str, Any], label: str) -> Path:
    if not isinstance(binding, Mapping) or set(binding) != {"path", "sha256"}:
        raise TypeScriptInstalledPending(label + " binding is invalid")
    path = _canonical(Path(binding["path"]), label)
    raw = _read_regular(path, label)
    if _digest(raw) != binding["sha256"]:
        raise TypeScriptInstalledPending(label + " digest changed")
    return path


def _profile_inputs(profile_binding: Mapping[str, Any], stage: Path) -> tuple[Mapping[str, Any], list[Mapping[str, Any]]]:
    profile = _source_path(
        {"path": profile_binding.get("path"), "sha256": profile_binding.get("sha256")},
        "profile manifest",
    )
    if profile.name != "SHA256SUMS":
        raise TypeScriptInstalledPending("profile manifest is not SHA256SUMS")
    raw = _read_regular(profile, "profile manifest", 1_048_576)
    members = {"SHA256SUMS": raw}
    for line in raw.decode("utf-8").splitlines():
        parts = line.split("  ", 1)
        if len(parts) != 2 or not parts[1] or parts[1].startswith("/"):
            raise TypeScriptInstalledPending("profile manifest line is invalid")
        member = profile.parent / parts[1]
        member_raw = _read_regular(member, "profile member", 1_048_576)
        if _digest(member_raw) != parts[0]:
            raise TypeScriptInstalledPending("profile member digest differs from manifest")
        members[parts[1]] = member_raw
    if len(members) != 18:
        raise TypeScriptInstalledPending("profile must contain exactly 18 members")
    staged = []
    for name in sorted(members):
        staged.append(_stage(stage / "profile", name, members[name], ""))
    manifest = next(item for item in staged if item["id"] == "SHA256SUMS")
    return manifest, staged


def _artifact(job: Mapping[str, Any], role: str) -> Mapping[str, Any]:
    rows = [item for item in job.get("artifacts", []) if isinstance(item, Mapping) and item.get("role") == role]
    if len(rows) != 1:
        raise TypeScriptInstalledPending("selected artifact set is incomplete")
    if (rows[0].get("sha256") != SDK_TARBALL_SHA256
            or rows[0].get("embedded_version") != SDK_PACKAGE_VERSION):
        raise TypeScriptInstalledPending("pending: TypeScript SDK archive is outside the reviewed launch inventory")
    path = _source_path(
        {"path": rows[0].get("path"), "sha256": rows[0].get("sha256")},
        "artifact",
    )
    if _digest(_read_regular(path, "artifact", 1_073_741_824)) != rows[0]["sha256"]:
        raise TypeScriptInstalledPending("artifact digest changed")
    return rows[0]


def _installed_manifest(root: Path, artifact_sha256: str, tarball: Path) -> tuple[Mapping[str, Any], int]:
    root = _canonical(root, "installed SDK root")
    if root.name != "sdk" or root.parent.name != "@teslatlas":
        raise TypeScriptInstalledPending("installed SDK root is not @teslatlas/sdk")
    try:
        with tarfile.open(tarball, "r:gz") as archive:
            names = sorted(item.name for item in archive.getmembers() if item.isfile())
    except (OSError, tarfile.TarError) as error:
        raise TypeScriptInstalledPending("SDK tarball member inventory is invalid") from error
    files = []
    for name in names:
        if not name.startswith("package/") or name == "package/":
            raise TypeScriptInstalledPending("SDK tarball member path is invalid")
        relative = name[len("package/"):]
        installed = root / relative
        try:
            info = installed.lstat()
        except OSError as error:
            raise TypeScriptInstalledPending("installed SDK member is missing") from error
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise TypeScriptInstalledPending("installed SDK member is not a regular file")
        raw = _read_regular(installed, "installed SDK member", 8_388_608)
        files.append({"path": relative, "bytes": len(raw), "mode": stat.S_IMODE(info.st_mode), "sha256": _digest(raw)})
    if not files:
        raise TypeScriptInstalledPending("installed SDK manifest is empty")
    if len(files) != SDK_MEMBER_COUNT:
        raise TypeScriptInstalledPending("installed SDK member inventory differs from the reviewed archive")
    return {"schema_version": 1, "artifact_sha256": artifact_sha256, "files": files}, len(files)


def _lane_sources() -> None:
    for name, expected in LANE_FILES.items():
        path = LANE_ROOT / name
        try:
            actual = _digest(_read_regular(path, "Hub lane source", 1_048_576, private=False))
        except TypeScriptInstalledPending:
            raise
        if actual != expected:
            raise TypeScriptInstalledPending("pending: Hub TypeScript lane source review is stale")


def _private_root(descriptor: Mapping[str, Any], cell_id: str) -> Path:
    socket_path = _canonical(Path(descriptor["broker_socket"]), "broker socket")
    root = socket_path.parent / ("typescript-" + cell_id + "-" + descriptor["session_id"])
    root.mkdir(mode=0o700, exist_ok=False)
    return root


def build_session_input(job, cell, config, matrix, contract: LoadedContract, descriptor, running):
    """Stage and return one closed matrix-adapter SessionInput."""
    _lane_sources()
    if not isinstance(running, Mapping) or not isinstance(running.get("descriptor"), Mapping):
        raise TypeScriptInstalledPending("installed Hub descriptor is unavailable")
    rich = running["descriptor"]
    session_config = _bound_json(Path(job["installed_session"]["config"]["path"]), "installed session config")
    environment = _env(job)
    stage = _private_root(descriptor, cell["id"])
    profile_manifest, profile_members = _profile_inputs(config["profile"], stage)

    scenario_path = _source_path(session_config["scenario"], "scenario")
    scenario = _stage(stage / "inputs", "scenario", _read_regular(scenario_path, "scenario"), ".json")
    certificate_path = _canonical(Path(rich["certificate_path"]), "certificate")
    certificate_raw = _read_regular(certificate_path, "certificate", 1_048_576)
    certificate = _stage(stage / "inputs", "certificate", certificate_raw, ".pem")
    try:
        import ssl
        der_digest = _digest(bytes.fromhex(ssl.PEM_cert_to_DER_cert(certificate_raw.decode("ascii"))))
    except (UnicodeDecodeError, ValueError, ssl.SSLError) as error:
        raise TypeScriptInstalledPending("certificate is not PEM") from error

    artifact = _artifact(job, "typescript_sdk_tarball")
    tarball_path = Path(artifact["path"])
    product = _stage(stage / "products", "typescript_sdk_tarball", _read_regular(tarball_path, "SDK tarball", 1_073_741_824), ".tgz")
    package_root_value = _env_value(environment, "package_root", required=True)
    package_root = _canonical(Path(package_root_value), "installed SDK root")
    manifest, _member_count = _installed_manifest(package_root, artifact["sha256"], tarball_path)
    manifest_stage = _stage(stage / "products", "typescript_sdk_manifest", _json_bytes(manifest), ".json")

    actors = []
    actor_id = "sdk_browser" if cell["client_id"] == BROWSER else "sdk_node"
    actor_kind = "packed_sdk_browser" if actor_id == "sdk_browser" else "packed_sdk_node"
    runtime_ref = "root_browser" if actor_id == "sdk_browser" else "root_node"
    entrypoint_ref = "sdk_browser_worker" if actor_id == "sdk_browser" else "sdk_node_worker"
    actor_manifest = _stage(stage / "actors", actor_id + "-manifest", _json_bytes(manifest), ".json")
    phase_contract = None
    if actor_id == "sdk_browser":
        values = {name: _env_value(environment, name, required=True, path=name != "ssh_alias") for name in ("remote_root", "ssh_config", "ssh_alias", "playwright_entry", "local_log")}
        phase_contract = _stage(stage / "actors", "browser-phase-contract", _json_bytes(values), ".json")
    actors.append({
        "id": actor_id, "kind": actor_kind, "execution": "browser_worker" if actor_id == "sdk_browser" else "local_worker",
        "runtime_ref": runtime_ref, "artifact_roles": ["typescript_sdk_tarball"],
        "source_roles": ["typescript_sdk_source"], "entrypoint_ref": entrypoint_ref,
        "input_manifest": actor_manifest, "phase_contract": phase_contract,
    })

    header = {
        "schema_version": 1, "execution_kind": "actual_hub_acceptance", "adapter": job["adapter"],
        "cell_id": job["cell_id"], "product_version": config["product_version"],
        "profile_id": config["profile"]["id"], "profile_revision": config["profile"]["revision"],
        "profile_sha256": config["profile"]["sha256"], "source_identities": job["source_identities"],
        "artifacts": job["artifacts"], "runtime": job["runtime"],
    }
    header_stage = _stage(stage / "inputs", "header", _json_bytes(header), ".json")
    contract_stage = _stage(stage / "inputs", "case-contract", _read_regular(Path(contract.manifest_path), "reviewed case contract", private=False), ".json")

    output_root = stage / "outputs"
    output_root.mkdir(mode=0o700)
    outputs = {
        "normalized": str(output_root / "normalized.json"),
        "actor_evidence": str(output_root / "actor-evidence.json"),
        "coordination_dir": str(output_root / "coordination"),
        "framework_log": str(output_root / "framework.log"),
    }
    expected_execution = dict(_execution_by_target(cell["client_id"])).get(cell["hub_target"], "local")
    broker_kind = "stdio" if expected_execution == "docker_exec_pipe" else "unix"
    broker = {"kind": broker_kind, "socket_path": None if broker_kind == "stdio" else descriptor["broker_socket"]}
    session = {
        "schema_version": 1, "kind": "matrix-adapter-session", "run_id": session_config["run_id"],
        "cell_id": cell["id"], "adapter_id": job["adapter"], "client_id": cell["client_id"],
        "session_id": descriptor["session_id"], "instance_nonce": os.urandom(32).hex(),
        "header": header_stage, "case_contract": contract_stage, "host_session": descriptor,
        "broker": broker,
        "inputs": {
            "profile_manifest": profile_manifest, "profile_members": profile_members, "scenario": scenario,
            "certificate": certificate, "certificate_der_sha256": der_digest,
            "product_inputs": [{"artifact_role": "typescript_sdk_tarball", "staged": product, "installed_manifest": manifest_stage, "local_root": str(package_root)}],
        }, "actors": actors, "outputs": outputs,
        "bounds": {"cell_timeout_ms": job["timeout_seconds"] * 1000, "cleanup_timeout_ms": 45000, "frame_bytes": 1048576, "evidence_bytes": 8388608, "framework_log_bytes": 8388608},
    }
    session_path = stage / "session-input.json"
    _write_bytes(session_path, _json_bytes(session))
    return session_path, session


def _execution_by_target(client_id: str):
    # The current client worker is local to the runner.  Remote Hub targets
    # are reached through the installed controller broker; no unreviewed SSH
    # client command is accepted by this registry.
    return (("macos_arm64", "local"), ("debian13_amd64", "local"), ("debian13_arm64", "local"))


def launch_adapter(job, cell, config, contract, session_input_path, session_input):
    _lane_sources()
    mode = "browser" if cell["client_id"] == BROWSER else "node"
    node_path = Path(job["argv"][0]).resolve()
    if node_path.name != "node" or not node_path.is_file():
        raise TypeScriptInstalledPending("fixed Node launcher is unavailable")
    descriptor_path = Path(job["argv"][2])
    descriptor = {"schema_version": 2, "kind": "installed-client-lane", "mode": mode, "session_input": file_binding(session_input_path, maximum=1_048_576)}
    _write_bytes(descriptor_path, _json_bytes(descriptor))
    framework = Path(session_input["outputs"]["framework_log"])
    stderr_path = Path(str(framework) + ".stderr.log")
    stdout_handle = framework.open("xb")
    stderr_handle = stderr_path.open("xb")
    try:
        process = subprocess.Popen(
            [str(node_path), str(LANE_ROOT / "run.mjs"), str(descriptor_path)],
            cwd=str(WORKSPACE), stdin=subprocess.DEVNULL, stdout=stdout_handle, stderr=stderr_handle,
            start_new_session=True, close_fds=True,
        )
    finally:
        stdout_handle.close(); stderr_handle.close()
    return process


def _read_staged_json(binding: Mapping[str, Any], label: str) -> Mapping[str, Any]:
    value = strict_json(read_bound_file(binding, label=label, maximum=8_388_608))
    if not isinstance(value, Mapping):
        raise TypeScriptInstalledPending(label + " is not an object")
    return value


def _runtime_document(session_input: Mapping[str, Any], actor_id: str) -> tuple[Mapping[str, str], Mapping[str, Any]]:
    path = Path(session_input["outputs"]["coordination_dir"]) / ("runtime-" + actor_id + ".json")
    binding = file_binding(path, maximum=8_388_608)
    return binding, _read_staged_json(binding, "installed runtime")


def runtime_inventory(job, cell, config, contract, session_input, deadline=None):
    actor_id = "sdk_browser" if cell["client_id"] == BROWSER else "sdk_node"
    binding, runtime = _runtime_document(session_input, actor_id)
    if runtime.get("runtime_ref") != ("root_browser" if actor_id == "sdk_browser" else "root_node"):
        raise TypeScriptInstalledPending("installed runtime reference is foreign")
    expected = job["runtime"]["client"]
    platform = runtime.get("platform", {})
    if platform.get("os") != expected["os"] or platform.get("architecture") != expected["architecture"]:
        raise TypeScriptInstalledPending("installed client runtime differs from admitted runtime")
    expected_tools = expected.get("tool_versions", {})
    actual_tools = runtime.get("tool_versions", {})
    if (not isinstance(expected_tools, Mapping) or not isinstance(actual_tools, Mapping)
            or any(actual_tools.get(name) != value for name, value in expected_tools.items())):
        raise TypeScriptInstalledPending("installed client toolchain differs from admitted runtime")
    expected_runtime = job["runtime"]
    observed_client = {
        "os": platform["os"],
        "architecture": platform["architecture"],
        "native_or_emulated": expected["native_or_emulated"],
        "service_mode": expected["service_mode"],
        "tool_versions": dict(actual_tools),
    }
    runtime_actual = {
        "hub": dict(expected_runtime["hub"]),
        "client": observed_client,
        "browser_engines": list(expected_runtime["browser_engines"]),
        "client_transports": list(expected_runtime["client_transports"]),
    }
    return {"schema_version": 1, "runtime_ref": runtime["runtime_ref"], "runtime_kind": runtime.get("runtime_kind"), "identity_sha256": runtime.get("identity_sha256"), "binding": binding, "runtime": runtime, "runtime_actual": runtime_actual}


def _controller_context(module, job, cell, session_input, normalized, actor_evidence, raw, runtime, admission_views):
    actor_claim = actor_evidence["actors"][0]
    actor = module.AdmittedActor(
        id=actor_claim["id"], kind=actor_claim["kind"], runtime_ref=actor_claim["runtime_ref"],
        entrypoint_ref=actor_claim["entrypoint_ref"], artifact_roles=tuple(actor_claim["artifact_roles"]),
        source_roles=tuple(actor_claim["source_roles"]), installed_manifest=actor_claim["installed_manifest"],
        runtime=runtime,
    )
    invocations = tuple(module.AdmittedInvocation(**item) for item in actor_evidence["invocations"])
    raw_by_id = {item.evidence_id: raw[item.evidence_id] for item in invocations}
    observations = admission_views["controller_observations"]["observations"]
    return module.AdmissionContext(
        adapter_id=cell["client_id"], cell_id=cell["id"], session_id=session_input["session_id"],
        header=_read_staged_json(session_input["header"]["local"], "evidence header"),
        scenario=_read_staged_json(session_input["inputs"]["scenario"]["local"], "scenario"),
        actors={actor.id: actor}, invocations=invocations, raw=raw_by_id,
        controller_observations=observations,
    )


def admit(job, cell, config, matrix, contract, normalized, actors, *, admission_views=None, runtime_context=None, deadline=None):
    if admission_views is None:
        raise TypeScriptInstalledPending("runner-owned controller admission view is required")
    if not isinstance(actors, Mapping) or not isinstance(actors.get("actors"), list) or len(actors["actors"]) != 1:
        raise TypeScriptInstalledPending("installed actor evidence is invalid")
    actor_id = "sdk_browser" if cell["client_id"] == BROWSER else "sdk_node"
    if not isinstance(runtime_context, Mapping) or not isinstance(runtime_context.get("runtime_view"), Mapping):
        raise TypeScriptInstalledPending("runner-owned runtime inventory is unavailable")
    runtime_view = runtime_context["runtime_view"]
    runtime = runtime_view.get("runtime")
    if not isinstance(runtime, Mapping):
        raise TypeScriptInstalledPending("installed runtime document is unavailable")
    raw = {}
    for item in actors["actors"][0].get("raw_evidence", []):
        if not isinstance(item, Mapping) or not isinstance(item.get("binding"), Mapping):
            raise TypeScriptInstalledPending("installed raw evidence binding is invalid")
        raw[item["id"]] = _read_staged_json(item["binding"], "installed raw evidence")
    context = _controller_context(
        contract.module, job, cell, runtime_context["session_input"], normalized,
        actors, raw, runtime, admission_views,
    )
    by_id = {item.get("id"): item for item in contract.manifest.get("cases", []) if isinstance(item, Mapping)}
    admitted_cases = []
    for case_id in contract.manifest["required_cases"]:
        declaration = by_id.get(case_id)
        if declaration is None:
            raise TypeScriptInstalledPending("installed case declaration is missing")
        invocations = [item for item in actors["invocations"] if item.get("case_id") == case_id]
        if len(invocations) != 1:
            raise TypeScriptInstalledPending("installed case invocation coverage is incomplete")
        invocation = invocations[0]
        evidence = raw.get(invocation["evidence_id"])
        if not isinstance(evidence, Mapping):
            raise TypeScriptInstalledPending("installed case raw evidence is incomplete")
        case = {
            "id": case_id,
            "status": "pending" if case_id == "installed_service_runtime" else "passed",
            "expected": evidence.get("facts"), "actual": evidence.get("facts"),
            "evidence_kind": declaration.get("evidence_kind"),
            "request_transcript": evidence.get("requests"),
        }
        decision = contract.module.admit_case(case, context)
        if case_id == "installed_service_runtime":
            if decision.status != "pending" or decision.code != "runner_owned_service_runtime":
                raise TypeScriptInstalledPending("installed service-runtime case was not runner-owned")
        elif decision.status != "passed" or decision.code != "accepted":
            raise TypeScriptInstalledPending("installed TypeScript case admission failed")
        # The service-runtime predicate is deliberately runner-owned.  Leave
        # it pending while the adapter callback runs; the shared supervisor
        # may promote it only after its independent close/stop proof exists.
        admitted_cases.append(case)
    return {"schema_version": 1, "adapter_id": cell["client_id"], "cases": admitted_cases}


def build_supplement(job, cell, config, matrix, contract, result, *, runtime_context=None):
    """Bind runner/controller/actor evidence into supplement-v2."""
    if not isinstance(runtime_context, Mapping) or not isinstance(runtime_context.get("session_input"), Mapping):
        raise TypeScriptInstalledPending("runner session input is unavailable for supplement")
    session_input = runtime_context["session_input"]
    completion = result.completion
    coordination = Path(session_input["outputs"]["coordination_dir"])
    actor_evidence = _read_staged_json(completion["actor_evidence"], "actor evidence")
    runtime_binding, runtime = _runtime_document(session_input, actor_evidence["actors"][0]["id"])
    claims = [dict(actor, runtime_evidence=runtime_binding) for actor in actor_evidence["actors"]]
    controller_view = runtime_context.get("controller_view")
    observations = controller_view.get("observations") if isinstance(controller_view, Mapping) else None
    if not isinstance(observations, Mapping):
        raise TypeScriptInstalledPending("controller observations are unavailable")
    observation_value = {"schema_version": 1, "session_id": session_input["session_id"], "observations": [_plain(observations[key]) for key in sorted(observations)]}
    observation_binding = write_exclusive_json(coordination / "controller-observations.json", observation_value)
    final_binding = write_exclusive_json(coordination / "final-stopped.json", _plain(result.session_evidence.final_stopped))
    transport_value = {"schema_version": 1, "session_id": session_input["session_id"], "status": "passed", "resources": _plain(result.session_evidence.local_transport) if isinstance(result.session_evidence.local_transport, list) else [{"evidence": _plain(result.session_evidence.local_transport)}]}
    transport_binding = write_exclusive_json(coordination / "transport-cleanup.json", transport_value)
    command_value = {"schema_version": 1, "session_id": session_input["session_id"], "cell_id": cell["id"], "exit_code": result.exit_code, "outcome": "passed", "logs": []}
    command_binding = write_exclusive_json(coordination / "command-outcome.json", command_value)
    # The adapter's Ready.evidence already names this exact completion file;
    # the supplement must repeat that binding byte-for-byte.
    adapter_completion = file_binding(coordination / "adapter-completion.json", maximum=1_048_576)
    ready = file_binding(coordination / "ready-000001.json", maximum=1_048_576)
    ack = file_binding(coordination / "ack-000001.json", maximum=1_048_576)
    case_bindings = []
    for invocation in actor_evidence["invocations"]:
        case_bindings.append({"case_id": invocation["case_id"], "actor_ids": [invocation["actor_id"]], "invocations": [invocation]})
    supplement = {
        "schema_version": 2, "cell_id": cell["id"], "session_id": session_input["session_id"],
        "actors": claims, "case_bindings": case_bindings,
        "controller_evidence": {
            "registration_sha256": session_input["host_session"]["registration_sha256"],
            "session_config_sha256": job["installed_session"]["config"]["sha256"],
            "observations": observation_binding, "journal": {"path": result.session_evidence.journal_path, "sha256": result.session_evidence.journal_sha256},
            "final_stopped": final_binding, "transport_cleanup": transport_binding,
        },
        "completion": {
            "adapter_completion": adapter_completion, "ready": ready, "ack": ack,
            "normalized": completion["normalized"], "actor_evidence": completion["actor_evidence"],
            "command_outcome": command_binding, "status": "passed",
        },
    }
    output = coordination / "installed-supplement.json"
    return write_exclusive_json(output, supplement)


def execution_logs(job, cell, config, contract, result):
    framework = Path(result.completion["normalized"]["path"]).parent / "framework.log"
    stderr = Path(str(framework) + ".stderr.log")
    command = {"argv": [cell["client_id"], "fixed-reviewed-entrypoint"], "cwd": str(WORKSPACE), "started_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"), "ended_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"), "exit_code": result.exit_code, "outcome": "exited_zero" if result.exit_code == 0 else "failed"}
    command_binding = write_exclusive_json(Path(result.completion["normalized"]["path"]).parent / "command-record.json", command)
    def metadata(path):
        raw = _read_regular(path, "execution log", 8_388_608)
        return {"path": str(path), "sha256": _digest(raw), "bytes": len(raw), "truncated": False}
    return {"stdout": metadata(framework), "stderr": metadata(stderr), "command_record": command_binding, "duration_ms": 0}


def source_entry(adapter_id: str):
    contract = CONTRACTS[adapter_id]
    return contract
