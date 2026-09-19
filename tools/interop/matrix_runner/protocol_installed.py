# SPDX-License-Identifier: AGPL-3.0-only
"""Source-fixed construction and launch seam for Protocol installed sessions.

This module deliberately stops before registry admission.  It owns the exact
Protocol sources, topology, private SessionInput construction, and child
launcher.  Job JSON supplies evidence inputs; it cannot select executable
code, validators, brokers, or output paths.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import platform
import ssl
import stat
import subprocess
import sys
from types import MappingProxyType
from typing import Any, Mapping

from .adapter_wire import (
    LoadedContract,
    ReviewedContract,
    file_binding,
    read_bound_file,
    strict_json,
    write_exclusive_json,
)


WORKSPACE = Path(__file__).resolve().parents[4]
PROTOCOL_ROOT = WORKSPACE / "teslatlas-protocol"

CONTRACT = ReviewedContract(
    str(PROTOCOL_ROOT / "tools/matrix-contract.json"),
    "75bdb6380f70dc30b0f91abf92edbe7024fe0617402c67025bd5ec5dca609ca8",
    str(PROTOCOL_ROOT / "tools/matrix_contract.py"),
    "eef926e5d732a4c7ad46a9fe02a6089ea28d5c36b4e2523607212603b4994b24",
)

# The coordinator imports these files directly.  Staging the exact bytes into
# a private tree prevents the child from resolving a later checkout change.
REVIEWED_SOURCES = MappingProxyType({
    "conformance/hub_control.py":
        "907065ee7b0a0665533f6ca674bee577bf70c6004fc58dbf0bd866c7dd674430",
    "conformance/hub_http.py":
        "d1b31ce65550837b032044ebb8b54cecdc69044fa0df6e488c7d59321a70f5d1",
    "conformance/hub_matrix.py":
        "c3b9ee8f17e59ab70e27aca21faada550eb50b2979439cca9f36e7bcd2fa75b4",
    "conformance/hub_native_evidence.py":
        "09fdd31510470577c1e22da0969c6762f933aa06b087b6959a7d520b29159bd0",
})

PROFILE_MEMBERS = (
    "SHA256SUMS", "auth.schema.json", "cases.json", "discovery.schema.json",
    "errors.schema.json", "examples/claim.json", "examples/current.json",
    "examples/discovery.json", "examples/drives.json", "examples/health.json",
    "examples/invitation.json", "examples/ready.json", "examples/vehicles.json",
    "field-semantics.json", "openapi.json", "profile.json",
    "resources.schema.json", "sync-regression.json",
)

_FORBIDDEN_JOB_AUTHORITY = frozenset({
    "executable", "validator_path", "output_path", "broker_socket",
})


class ProtocolInstalledPending(RuntimeError):
    """The fixed Protocol source seam cannot admit the supplied inputs."""


def _reviewed_python_runtime() -> Mapping[str, Any]:
    """Observe the fixed coordinator interpreter selected by this process."""
    if tuple(sys.version_info[:2]) < (3, 11):
        raise ProtocolInstalledPending("Protocol coordinator requires Python 3.11 or newer")
    executable = Path(sys.executable).resolve()
    executable_raw = _read_regular(
        executable, "coordinator Python executable", 64 * 1024 * 1024,
        private=False,
    )
    system = platform.system()
    return {
        "os": "macOS" if system == "Darwin" else system,
        "architecture": {"aarch64": "arm64", "x86_64": "amd64"}.get(
            platform.machine(), platform.machine(),
        ),
        "native_or_emulated": "native",
        "service_mode": "protocol-conformance-process",
        "tool_versions": {"python": platform.python_version()},
        "executable": str(executable),
        "identity_sha256": _digest(executable_raw),
    }


def _validated_python_runtime(job: Mapping[str, Any]) -> Mapping[str, Any]:
    observed = _reviewed_python_runtime()
    public = {
        key: observed[key] for key in (
            "os", "architecture", "native_or_emulated", "service_mode", "tool_versions",
        )
    }
    expected = job.get("runtime", {}).get("client")
    if expected != public:
        raise ProtocolInstalledPending(
            "installed Protocol coordinator runtime differs from observation"
        )
    return observed


def execution_by_target():
    """The Protocol coordinator always runs beside the Hub matrix runner."""
    return (
        ("macos_arm64", "local"),
        ("debian13_amd64", "local"),
        ("debian13_arm64", "local"),
    )


def broker_kind_by_target():
    """All Protocol targets use the runner-owned Unix controller broker."""
    return (
        ("macos_arm64", "unix"),
        ("debian13_amd64", "unix"),
        ("debian13_arm64", "unix"),
    )


def _digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _canonical(path: Path, label: str) -> Path:
    if not path.is_absolute() or path.resolve(strict=False) != path:
        raise ProtocolInstalledPending(label + " path is not canonical")
    return path


def _read_regular(
    path: Path, label: str, maximum: int = 8_388_608, *, private: bool = True,
) -> bytes:
    path = _canonical(path, label)
    try:
        before = path.lstat()
    except OSError as error:
        raise ProtocolInstalledPending(label + " is unavailable") from error
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_nlink != 1
        or before.st_uid != os.getuid()
        or before.st_size > maximum
        or (private and stat.S_IMODE(before.st_mode) & 0o077)
    ):
        raise ProtocolInstalledPending(label + " is not an admissible regular file")
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            opened = os.fstat(descriptor)
            if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                raise ProtocolInstalledPending(label + " changed before reading")
            chunks = bytearray()
            while len(chunks) <= maximum:
                part = os.read(descriptor, min(65_536, maximum + 1 - len(chunks)))
                if not part:
                    break
                chunks.extend(part)
            after = os.fstat(descriptor)
        finally:
            os.close(descriptor)
        final = path.lstat()
    except ProtocolInstalledPending:
        raise
    except OSError as error:
        raise ProtocolInstalledPending(label + " cannot be read") from error
    identity = (before.st_dev, before.st_ino, before.st_mtime_ns, before.st_size)
    if (
        len(chunks) > maximum
        or len(chunks) != before.st_size
        or identity
        != (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_size)
        or identity
        != (final.st_dev, final.st_ino, final.st_mtime_ns, final.st_size)
    ):
        raise ProtocolInstalledPending(label + " changed while reading")
    return bytes(chunks)


def _json_bytes(value: Mapping[str, Any]) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"),
                   ensure_ascii=False, allow_nan=False) + "\n"
    ).encode("utf-8")


def _plain(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {key: _plain(item) for key, item in value.items()}
    if isinstance(value, (tuple, list)):
        return [_plain(item) for item in value]
    return value


def _write_bytes(path: Path, raw: bytes, *, mode: int = 0o600) -> Mapping[str, str]:
    path = _canonical(path, "staged file")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    except OSError as error:
        raise ProtocolInstalledPending("staged file path is not fresh") from error
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        path.chmod(mode)
    except BaseException:
        try:
            path.unlink()
        except OSError:
            pass
        raise
    return {"path": str(path), "sha256": _digest(raw)}


def _stage(
    root: Path, identifier: str, raw: bytes, *, suffix: str = ".bin",
    relative: Path = None, mode: int = 0o600,
) -> Mapping[str, Any]:
    leaf = relative if relative is not None else Path(identifier.replace("/", "_") + suffix)
    return {
        "id": identifier,
        "root": _write_bytes(root / "root" / leaf, raw, mode=mode),
        "local": _write_bytes(root / "local" / leaf, raw, mode=mode),
    }


def _bound_json(
    binding: Mapping[str, Any], label: str, maximum: int = 1_048_576,
) -> Mapping[str, Any]:
    try:
        value = strict_json(read_bound_file(binding, label=label, maximum=maximum))
    except Exception as error:
        raise ProtocolInstalledPending(label + " is invalid") from error
    if not isinstance(value, Mapping):
        raise ProtocolInstalledPending(label + " is not an object")
    return value


def _private_root(descriptor: Mapping[str, Any], cell_id: str) -> Path:
    broker = _canonical(Path(descriptor.get("broker_socket", "")), "broker socket")
    session_id = descriptor.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        raise ProtocolInstalledPending("installed session identity is unavailable")
    root = broker.parent / ("protocol-" + cell_id + "-" + session_id)
    root.mkdir(mode=0o700, exist_ok=False)
    return root


def _profile_inputs(profile: Mapping[str, Any], stage: Path):
    root = _canonical(Path(profile.get("path", "")), "profile root")
    manifest_raw = _read_regular(root / "SHA256SUMS", "profile manifest", 1_048_576,
                                 private=False)
    if _digest(manifest_raw) != profile.get("sha256"):
        raise ProtocolInstalledPending("profile manifest digest changed")
    try:
        checksums = {
            name: value
            for value, name in (
                line.split("  ", 1)
                for line in manifest_raw.decode("ascii").splitlines()
            )
        }
    except (UnicodeError, ValueError) as error:
        raise ProtocolInstalledPending("profile manifest syntax is invalid") from error
    if set(checksums) != set(PROFILE_MEMBERS) - {"SHA256SUMS"}:
        raise ProtocolInstalledPending("profile member set is incomplete")
    members = []
    for name in PROFILE_MEMBERS:
        raw = manifest_raw if name == "SHA256SUMS" else _read_regular(
            root / name, "profile member", 1_048_576, private=False,
        )
        if name != "SHA256SUMS" and _digest(raw) != checksums[name]:
            raise ProtocolInstalledPending("profile member digest differs from manifest")
        members.append(_stage(
            stage, "profile_" + name.replace("/", "_"), raw,
            relative=Path("hub-http-v1") / "1.0.0" / name,
        ))
    return members[0], members


def _validate_job_inventory(job: Mapping[str, Any], product_version: str) -> None:
    if _FORBIDDEN_JOB_AUTHORITY.intersection(job):
        raise ProtocolInstalledPending("job attempts to select Protocol adapter authority")
    sources = job.get("source_identities")
    if not isinstance(sources, list) or len(sources) != 2:
        raise ProtocolInstalledPending("Protocol source inventory is incomplete")
    by_role = {
        item.get("role"): item for item in sources if isinstance(item, Mapping)
    }
    if set(by_role) != {"hub_source", "protocol_source"}:
        raise ProtocolInstalledPending("Protocol source inventory is foreign")
    if Path(str(by_role["protocol_source"].get("repo", ""))).resolve() != PROTOCOL_ROOT:
        raise ProtocolInstalledPending("Protocol source checkout is foreign")
    if Path(str(by_role["hub_source"].get("repo", ""))).resolve() != WORKSPACE / "hub":
        raise ProtocolInstalledPending("Hub source checkout is foreign")

    artifacts = job.get("artifacts")
    if not isinstance(artifacts, list) or len(artifacts) != 2:
        raise ProtocolInstalledPending("Protocol artifact inventory is incomplete")
    by_artifact = {
        item.get("role"): item for item in artifacts if isinstance(item, Mapping)
    }
    if set(by_artifact) != {"hub_executable", "protocol_fixture_seed"}:
        raise ProtocolInstalledPending("Protocol artifact inventory is foreign")
    hub = by_artifact["hub_executable"]
    seed = by_artifact["protocol_fixture_seed"]
    if hub.get("embedded_version") != product_version:
        raise ProtocolInstalledPending("Hub artifact version is foreign")
    if seed.get("name") != "interop_fixture" or seed.get("embedded_version") != "tooling":
        raise ProtocolInstalledPending("Protocol fixture artifact is foreign")
    for artifact in (hub, seed):
        raw = _read_regular(Path(str(artifact.get("path", ""))), "Protocol artifact",
                            1_073_741_824, private=False)
        if _digest(raw) != artifact.get("sha256"):
            raise ProtocolInstalledPending("Protocol artifact digest changed")


def _stage_reviewed_sources(stage: Path, source_identity: Mapping[str, Any]):
    records = []
    for relative, expected in REVIEWED_SOURCES.items():
        raw = _read_regular(PROTOCOL_ROOT / relative, "reviewed Protocol source",
                            1_048_576, private=False)
        if _digest(raw) != expected:
            raise ProtocolInstalledPending("pending: reviewed Protocol source changed")
        staged = _stage(stage, relative.replace("/", "_"), raw,
                        relative=Path(relative), mode=0o400)
        records.append({
            "path": relative, "bytes": len(raw), "mode": 0o400,
            "sha256": expected,
        })
        if staged["root"]["sha256"] != staged["local"]["sha256"]:
            raise ProtocolInstalledPending("reviewed Protocol source staging differs")
    build = _stage(
        stage, "protocol_source_build",
        _json_bytes({
            "schema_version": 1, "kind": "reviewed-protocol-source",
            "source_identity": source_identity, "files": records,
        }),
        suffix=".json",
    )
    manifest = _stage(
        stage, "protocol_http_manifest",
        _json_bytes({
            "schema_version": 1, "build_record": build["local"],
            "files": records,
        }),
        suffix=".json", relative=Path("actor-manifest.json"),
    )
    return manifest


def build_session_input(
    job, cell, config, matrix, contract: LoadedContract, descriptor, running,
    deadline=None,
):
    """Build a closed Protocol SessionInput from fixed source and private inputs."""
    del matrix
    if deadline is not None:
        deadline.remaining()
    _validated_python_runtime(job)
    if (
        job.get("adapter") != "protocol_actual_hub"
        or cell.get("client_id") != "protocol_actual_hub"
        or cell.get("id") != job.get("cell_id")
        or contract.manifest.get("adapter_id") != "protocol_actual_hub"
        or not isinstance(running, Mapping)
        or not isinstance(running.get("descriptor"), Mapping)
    ):
        raise ProtocolInstalledPending("installed Protocol descriptor is unavailable")
    product_version = config.get("product_version")
    if product_version != "2026.36.2":
        raise ProtocolInstalledPending("Protocol product version is unavailable")
    _validate_job_inventory(job, product_version)

    session_config = _bound_json(
        job.get("installed_session", {}).get("config"), "installed session config",
    )
    run_id = session_config.get("run_id")
    scenario_binding = session_config.get("scenario")
    if not isinstance(run_id, str) or not run_id:
        raise ProtocolInstalledPending("installed run identity is unavailable")
    try:
        scenario_raw = read_bound_file(
            scenario_binding, label="Protocol scenario", maximum=1_048_576,
        )
    except Exception as error:
        raise ProtocolInstalledPending("Protocol scenario is unavailable") from error

    stage = _private_root(descriptor, cell["id"])
    profile_manifest, profile_members = _profile_inputs(config["profile"], stage / "profile")
    scenario = _stage(stage / "inputs", "scenario", scenario_raw, suffix=".json")

    certificate_path = _canonical(
        Path(running["descriptor"].get("certificate_path", "")), "certificate",
    )
    certificate_raw = _read_regular(certificate_path, "certificate", 1_048_576)
    try:
        certificate_der = ssl.PEM_cert_to_DER_cert(certificate_raw.decode("ascii"))
    except (UnicodeError, ValueError, ssl.SSLError) as error:
        raise ProtocolInstalledPending("certificate is not PEM") from error
    certificate = _stage(stage / "inputs", "certificate", certificate_raw, suffix=".pem")

    protocol_source = next(
        item for item in job["source_identities"] if item["role"] == "protocol_source"
    )
    actor_manifest = _stage_reviewed_sources(stage / "protocol", protocol_source)

    header = {
        "schema_version": 1,
        "execution_kind": "actual_hub_acceptance",
        "adapter": "protocol_actual_hub",
        "cell_id": cell["id"],
        "product_version": product_version,
        "profile_id": config["profile"]["id"],
        "profile_revision": config["profile"]["revision"],
        "profile_sha256": config["profile"]["sha256"],
        "source_identities": job["source_identities"],
        "artifacts": job["artifacts"],
        "runtime": job["runtime"],
    }
    header_stage = _stage(stage / "inputs", "header", _json_bytes(header), suffix=".json")
    contract_raw = _read_regular(
        Path(CONTRACT.manifest_path), "reviewed Protocol contract", 1_048_576,
        private=False,
    )
    if _digest(contract_raw) != CONTRACT.manifest_sha256:
        raise ProtocolInstalledPending("pending: reviewed Protocol contract changed")
    contract_stage = _stage(stage / "inputs", "case_contract", contract_raw, suffix=".json")

    output_root = stage / "outputs"
    output_root.mkdir(mode=0o700)
    outputs = {
        "normalized": str(output_root / "normalized.json"),
        "actor_evidence": str(output_root / "actor-evidence.json"),
        "coordination_dir": str(output_root / "coordination"),
        "framework_log": str(output_root / "framework.log"),
    }
    session = {
        "schema_version": 1,
        "kind": "matrix-adapter-session",
        "run_id": run_id,
        "cell_id": cell["id"],
        "adapter_id": "protocol_actual_hub",
        "client_id": "protocol_actual_hub",
        "session_id": descriptor["session_id"],
        "instance_nonce": os.urandom(32).hex(),
        "header": header_stage,
        "case_contract": contract_stage,
        "host_session": descriptor,
        "broker": {"kind": "unix", "socket_path": descriptor["broker_socket"]},
        "inputs": {
            "profile_manifest": profile_manifest,
            "profile_members": profile_members,
            "scenario": scenario,
            "certificate": certificate,
            "certificate_der_sha256": _digest(certificate_der),
            "product_inputs": [],
        },
        "actors": [{
            "id": "protocol_http",
            "kind": "protocol_http",
            "execution": "coordinator",
            "runtime_ref": "root_python",
            "artifact_roles": [],
            "source_roles": ["protocol_source"],
            "entrypoint_ref": "protocol_actual_hub",
            "input_manifest": actor_manifest,
            "phase_contract": None,
        }],
        "outputs": outputs,
        "bounds": {
            "cell_timeout_ms": job["timeout_seconds"] * 1000,
            "cleanup_timeout_ms": 45_000,
            "frame_bytes": 1_048_576,
            "evidence_bytes": 8_388_608,
            "framework_log_bytes": 8_388_608,
        },
    }
    session_path = stage / "session-input.json"
    _write_bytes(session_path, _json_bytes(session))
    if deadline is not None:
        deadline.remaining()
    return session_path, session


def _validate_staged_sources(session_input: Mapping[str, Any]) -> Path:
    actors = session_input.get("actors")
    if not isinstance(actors, list) or len(actors) != 1:
        raise ProtocolInstalledPending("Protocol actor inventory is unavailable")
    manifest_binding = actors[0].get("input_manifest", {}).get("local")
    manifest = _bound_json(manifest_binding, "Protocol actor source manifest")
    if set(manifest) != {"schema_version", "build_record", "files"}:
        raise ProtocolInstalledPending("Protocol actor source manifest is invalid")
    files = manifest.get("files")
    if not isinstance(files, list) or [item.get("path") for item in files] != list(REVIEWED_SOURCES):
        raise ProtocolInstalledPending("Protocol actor source inventory is foreign")
    source_root = Path(manifest_binding["path"]).parent
    for record, (relative, expected) in zip(files, REVIEWED_SOURCES.items()):
        raw = _read_regular(source_root / relative, "staged Protocol source", 1_048_576)
        if record != {
            "path": relative,
            "bytes": len(raw),
            "mode": 0o400,
            "sha256": expected,
        }:
            raise ProtocolInstalledPending("Protocol actor source record is foreign")
        if stat.S_IMODE((source_root / relative).stat().st_mode) != 0o400 or _digest(raw) != expected:
            raise ProtocolInstalledPending("staged Protocol source changed")
    return source_root


def launch_adapter(
    job, cell, config, contract, session_input_path, session_input,
    *, deadline=None, session=None, running=None,
):
    """Launch only the private, hash-bound Protocol coordinator source tree."""
    del cell, config, contract, session, running
    if _FORBIDDEN_JOB_AUTHORITY.intersection(job):
        raise ProtocolInstalledPending("job attempts to select Protocol adapter authority")
    if deadline is not None:
        deadline.remaining()
    python_runtime = _validated_python_runtime(job)
    source_root = _validate_staged_sources(session_input)
    entrypoint = source_root / "conformance" / "hub_matrix.py"
    session_path = _canonical(Path(session_input_path), "Protocol SessionInput")
    _read_regular(session_path, "Protocol SessionInput", 1_048_576)

    framework = _canonical(Path(session_input["outputs"]["framework_log"]),
                           "Protocol framework log")
    stderr_path = Path(str(framework) + ".stderr.log")
    framework.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    stdout_descriptor = os.open(framework, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        stderr_descriptor = os.open(
            stderr_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600,
        )
    except BaseException:
        os.close(stdout_descriptor)
        framework.unlink(missing_ok=True)
        raise
    stdout_handle = os.fdopen(stdout_descriptor, "wb")
    stderr_handle = os.fdopen(stderr_descriptor, "wb")
    try:
        process = subprocess.Popen(
            [python_runtime["executable"], str(entrypoint), str(session_path)],
            cwd=str(source_root),
            stdin=subprocess.DEVNULL,
            stdout=stdout_handle,
            stderr=stderr_handle,
            env={
                "PATH": os.defpath,
                "PYTHONDONTWRITEBYTECODE": "1",
                "PYTHONHASHSEED": "0",
                "PYTHONNOUSERSITE": "1",
            },
            start_new_session=True,
            close_fds=True,
        )
    finally:
        stdout_handle.close()
        stderr_handle.close()
    if deadline is not None:
        deadline.remaining()
    return process


def runtime_inventory(job, cell, config, contract, session_input, deadline=None):
    """Record the coordinator runtime from the running interpreter itself."""
    del config, contract
    if deadline is not None:
        deadline.remaining()
    if cell.get("client_id") != "protocol_actual_hub":
        raise ProtocolInstalledPending("Protocol runtime cell is foreign")
    header = _bound_json(session_input.get("header", {}).get("local"), "evidence header")
    expected_runtime = header.get("runtime")
    if expected_runtime != job.get("runtime") or not isinstance(expected_runtime, Mapping):
        raise ProtocolInstalledPending("Protocol runtime expectation changed after staging")
    expected_client = expected_runtime.get("client")
    observed = _validated_python_runtime(job)
    observed_client = {key: observed[key] for key in (
        "os", "architecture", "native_or_emulated", "service_mode", "tool_versions",
    )}
    if expected_client != observed_client:
        raise ProtocolInstalledPending("installed Protocol coordinator runtime differs from observation")
    runtime_document = {
        "schema_version": 1,
        "runtime_ref": "root_python",
        "runtime_kind": "coordinator_python",
        "identity_sha256": observed["identity_sha256"],
        "platform": {"os": observed["os"], "architecture": observed["architecture"]},
        "tool_versions": observed["tool_versions"],
        "executable": observed["executable"],
    }
    coordination = _canonical(
        Path(session_input["outputs"]["coordination_dir"]), "Protocol coordination directory",
    )
    binding = _write_bytes(
        coordination / "runtime-protocol_http.json", _json_bytes(runtime_document),
    )
    if deadline is not None:
        deadline.remaining()
    return {
        **runtime_document,
        "binding": binding,
        "runtime": runtime_document,
        "runtime_actual": {
            "hub": dict(expected_runtime["hub"]),
            "client": observed_client,
            "browser_engines": list(expected_runtime["browser_engines"]),
            "client_transports": list(expected_runtime["client_transports"]),
        },
    }


def _controller_context(
    module, cell, session_input, actors, raw, runtime, admission_views,
):
    actor_claim = actors["actors"][0]
    actor = module.AdmittedActor(
        id=actor_claim["id"], kind=actor_claim["kind"],
        runtime_ref=actor_claim["runtime_ref"],
        entrypoint_ref=actor_claim["entrypoint_ref"],
        artifact_roles=tuple(actor_claim["artifact_roles"]),
        source_roles=tuple(actor_claim["source_roles"]),
        installed_manifest=actor_claim["installed_manifest"], runtime=runtime,
    )
    try:
        invocations = tuple(module.AdmittedInvocation(
            **{**item, "request_ids": tuple(item["request_ids"])}
        ) for item in actors["invocations"])
    except (KeyError, TypeError) as error:
        raise ProtocolInstalledPending("Protocol invocation inventory is invalid") from error
    observations = admission_views.get("controller_observations", {}).get("observations")
    if not isinstance(observations, Mapping):
        raise ProtocolInstalledPending("runner-owned controller observations are unavailable")
    return module.AdmissionContext(
        adapter_id=cell["client_id"], cell_id=cell["id"],
        session_id=session_input["session_id"],
        header=_bound_json(session_input["header"]["local"], "evidence header"),
        scenario=_bound_json(session_input["inputs"]["scenario"]["local"], "scenario"),
        actors={actor.id: actor}, invocations=invocations, raw=raw,
        controller_observations=observations,
    )


def admit(
    job, cell, config, matrix, contract, normalized, actors, *,
    admission_views=None, runtime_context=None, deadline=None,
):
    """Admit Protocol cases through the reviewed pure validator."""
    del job, config, matrix
    if deadline is not None:
        deadline.remaining()
    if not isinstance(admission_views, Mapping):
        raise ProtocolInstalledPending("runner-owned controller admission view is required")
    if (
        not isinstance(runtime_context, Mapping)
        or not isinstance(runtime_context.get("session_input"), Mapping)
        or not isinstance(runtime_context.get("runtime_view"), Mapping)
    ):
        raise ProtocolInstalledPending("runner-owned Protocol runtime inventory is unavailable")
    session_input = runtime_context["session_input"]
    runtime_view = runtime_context["runtime_view"]
    runtime = runtime_view.get("runtime")
    if not isinstance(runtime, Mapping):
        raise ProtocolInstalledPending("Protocol coordinator runtime document is unavailable")
    if _bound_json(runtime_view.get("binding"), "Protocol coordinator runtime") != runtime:
        raise ProtocolInstalledPending("Protocol coordinator runtime binding differs")
    actor_fields = {
        "id", "kind", "runtime_ref", "entrypoint_ref", "artifact_roles",
        "source_roles", "installed_manifest", "raw_evidence",
    }
    outer_fields = {
        "schema_version", "session_id", "cell_id", "session_input_sha256",
        "actors", "invocations",
    }
    if (
        not isinstance(actors, Mapping) or set(actors) != outer_fields
        or actors.get("schema_version") != 1
        or actors.get("session_id") != session_input.get("session_id")
        or actors.get("cell_id") != cell.get("id")
        or not isinstance(actors.get("actors"), list) or len(actors["actors"]) != 1
        or not isinstance(actors["actors"][0], Mapping)
        or set(actors["actors"][0]) != actor_fields
        or not isinstance(actors.get("invocations"), list)
    ):
        raise ProtocolInstalledPending("installed Protocol actor evidence is invalid")
    raw = {}
    for item in actors["actors"][0]["raw_evidence"]:
        if (
            not isinstance(item, Mapping) or set(item) != {"id", "schema_id", "binding"}
            or item.get("schema_id") != "protocol-http-v1"
            or not isinstance(item.get("id"), str) or item["id"] in raw
        ):
            raise ProtocolInstalledPending("installed Protocol raw evidence inventory is invalid")
        raw[item["id"]] = _bound_json(item["binding"], "installed Protocol raw evidence")
    context = _controller_context(
        contract.module, cell, session_input, actors, raw, runtime, admission_views,
    )
    header = dict(context.header)
    if (
        not isinstance(normalized, Mapping)
        or set(normalized) != set(header) | {"cases"}
        or any(normalized.get(key) != value for key, value in header.items())
        or not isinstance(normalized.get("cases"), list)
    ):
        raise ProtocolInstalledPending("normalized Protocol evidence identity is invalid")
    cases_by_id = {
        item.get("id"): item for item in normalized["cases"] if isinstance(item, Mapping)
    }
    if (
        len(cases_by_id) != len(normalized["cases"])
        or list(cases_by_id) != list(contract.manifest["required_cases"])
    ):
        raise ProtocolInstalledPending("normalized Protocol case coverage is incomplete")
    admitted_cases = []
    for case_id in contract.manifest["required_cases"]:
        case = cases_by_id[case_id]
        decision = contract.module.admit_case(dict(case), context)
        if case_id == "installed_service_runtime":
            accepted = decision.status == "pending" and decision.code == "runner_owned_service_runtime"
        else:
            accepted = decision.status == "passed" and decision.code == "accepted"
        if not accepted:
            raise ProtocolInstalledPending("installed Protocol case admission failed: " + case_id)
        admitted_cases.append(dict(case))
    if deadline is not None:
        deadline.remaining()
    return {
        "schema_version": 1, "adapter_id": cell["client_id"],
        "cases": admitted_cases,
    }


def build_supplement(
    job, cell, config, matrix, contract, result, *, runtime_context=None,
):
    """Bind the retained Protocol outputs into the installed supplement."""
    del config, matrix, contract
    if (
        not isinstance(runtime_context, Mapping)
        or not isinstance(runtime_context.get("session_input"), Mapping)
        or not isinstance(runtime_context.get("runtime_view"), Mapping)
    ):
        raise ProtocolInstalledPending("runner Protocol context is unavailable for supplement")
    session_input = runtime_context["session_input"]
    completion = result.completion
    if (
        not isinstance(completion, Mapping)
        or set(completion) != {
            "schema_version", "session_id", "cell_id", "session_input_sha256",
            "normalized", "actor_evidence",
        }
        or completion.get("schema_version") != 1
        or completion.get("session_id") != session_input.get("session_id")
        or completion.get("cell_id") != cell.get("id")
        or result.exit_code != 0
    ):
        raise ProtocolInstalledPending("retained Protocol completion is invalid")
    _bound_json(completion["normalized"], "retained normalized Protocol evidence", 8_388_608)
    actor_evidence = _bound_json(
        completion["actor_evidence"], "retained Protocol actor evidence", 8_388_608,
    )
    if (
        actor_evidence.get("session_id") != session_input.get("session_id")
        or actor_evidence.get("cell_id") != cell.get("id")
        or not isinstance(actor_evidence.get("actors"), list)
        or len(actor_evidence["actors"]) != 1
        or not isinstance(actor_evidence.get("invocations"), list)
    ):
        raise ProtocolInstalledPending("retained Protocol actor evidence is invalid")

    runtime_view = runtime_context["runtime_view"]
    runtime_binding = runtime_view.get("binding")
    runtime = runtime_view.get("runtime")
    if (
        not isinstance(runtime_binding, Mapping)
        or not isinstance(runtime, Mapping)
        or _bound_json(runtime_binding, "retained Protocol runtime", 8_388_608) != runtime
    ):
        raise ProtocolInstalledPending("retained Protocol runtime binding differs")
    actors = [dict(actor_evidence["actors"][0], runtime_evidence=dict(runtime_binding))]

    controller_view = runtime_context.get("controller_view")
    observations = (
        controller_view.get("observations")
        if isinstance(controller_view, Mapping) else None
    )
    if not isinstance(observations, Mapping):
        raise ProtocolInstalledPending("runner-owned controller observations are unavailable")
    coordination = _canonical(
        Path(session_input["outputs"]["coordination_dir"]),
        "Protocol coordination directory",
    )
    observation_binding = write_exclusive_json(
        coordination / "controller-observations.json",
        {
            "schema_version": 1,
            "session_id": session_input["session_id"],
            "observations": [_plain(observations[key]) for key in sorted(observations)],
        },
    )
    evidence = result.session_evidence
    if (
        evidence is None
        or not isinstance(getattr(evidence, "final_stopped", None), Mapping)
        or tuple(getattr(evidence, "cleanup_errors", (None,))) != ()
        or not isinstance(getattr(evidence, "journal_path", None), str)
        or not isinstance(getattr(evidence, "journal_sha256", None), str)
        or getattr(evidence, "local_transport", None) is None
    ):
        raise ProtocolInstalledPending("runner-owned Protocol close evidence is unavailable")
    try:
        read_bound_file(
            {"path": evidence.journal_path, "sha256": evidence.journal_sha256},
            label="retained Protocol controller journal", maximum=8_388_608,
        )
    except Exception as error:
        raise ProtocolInstalledPending("retained Protocol controller journal differs") from error
    final_binding = write_exclusive_json(
        coordination / "final-stopped.json", _plain(evidence.final_stopped),
    )
    transport_resources = (
        _plain(evidence.local_transport)
        if isinstance(evidence.local_transport, (tuple, list))
        else [{"evidence": _plain(evidence.local_transport)}]
    )
    if not transport_resources:
        raise ProtocolInstalledPending("Protocol transport cleanup evidence is unavailable")
    transport_binding = write_exclusive_json(
        coordination / "transport-cleanup.json",
        {
            "schema_version": 1, "session_id": session_input["session_id"],
            "status": "passed", "resources": transport_resources,
        },
    )
    command_binding = write_exclusive_json(
        coordination / "command-outcome.json",
        {
            "schema_version": 1, "session_id": session_input["session_id"],
            "cell_id": cell["id"], "exit_code": result.exit_code,
            "outcome": "passed", "logs": [],
        },
    )

    invocations = actor_evidence["invocations"]
    case_bindings = []
    flattened = []
    contract_cases = tuple(
        item for item in _bound_json(
            session_input["case_contract"]["local"], "staged Protocol case contract",
        )["required_cases"]
    )
    for case_id in contract_cases:
        selected = [item for item in invocations if item.get("case_id") == case_id]
        if len(selected) != 1 or selected[0].get("actor_id") != "protocol_http":
            raise ProtocolInstalledPending("retained Protocol invocation coverage is incomplete")
        flattened.extend(selected)
        case_bindings.append({
            "case_id": case_id, "actor_ids": ["protocol_http"],
            "invocations": selected,
        })
    if flattened != invocations or len(contract_cases) != len(set(contract_cases)):
        raise ProtocolInstalledPending("retained Protocol invocation order is invalid")

    supplement = {
        "schema_version": 2, "cell_id": cell["id"],
        "session_id": session_input["session_id"], "actors": actors,
        "case_bindings": case_bindings,
        "controller_evidence": {
            "registration_sha256": session_input["host_session"]["registration_sha256"],
            "session_config_sha256": job["installed_session"]["config"]["sha256"],
            "observations": observation_binding,
            "journal": {
                "path": evidence.journal_path, "sha256": evidence.journal_sha256,
            },
            "final_stopped": final_binding, "transport_cleanup": transport_binding,
        },
        "completion": {
            "adapter_completion": file_binding(
                coordination / "adapter-completion.json", maximum=1_048_576,
            ),
            "ready": file_binding(
                coordination / "ready-000001.json", maximum=1_048_576,
            ),
            "ack": file_binding(
                coordination / "ack-000001.json", maximum=1_048_576,
            ),
            "normalized": dict(completion["normalized"]),
            "actor_evidence": dict(completion["actor_evidence"]),
            "command_outcome": command_binding,
            "status": "passed",
        },
    }
    adapter_completion = _bound_json(
        supplement["completion"]["adapter_completion"],
        "retained Protocol adapter completion",
    )
    if adapter_completion != completion:
        raise ProtocolInstalledPending("retained Protocol adapter completion differs")
    return write_exclusive_json(coordination / "installed-supplement.json", supplement)


def execution_logs(job, cell, config, contract, result):
    """Bind the two launcher streams and its source-fixed command."""
    del job, config, contract
    completion = result.completion
    normalized = completion.get("normalized") if isinstance(completion, Mapping) else None
    if not isinstance(normalized, Mapping):
        raise ProtocolInstalledPending("Protocol completion lacks normalized output")
    output_root = _canonical(
        Path(str(normalized.get("path", ""))).parent, "Protocol output root",
    )
    framework = output_root / "framework.log"
    stderr = Path(str(framework) + ".stderr.log")
    actor_evidence = _bound_json(
        completion["actor_evidence"], "retained Protocol actor evidence", 8_388_608,
    )
    try:
        source_root = Path(
            actor_evidence["actors"][0]["installed_manifest"]["path"]
        ).parent
    except (KeyError, IndexError, TypeError) as error:
        raise ProtocolInstalledPending("Protocol launch source binding is unavailable") from error
    entrypoint = source_root / "conformance" / "hub_matrix.py"
    session_path = output_root.parent / "session-input.json"
    _read_regular(entrypoint, "launched Protocol entrypoint", 1_048_576)
    _read_regular(session_path, "launched Protocol SessionInput", 1_048_576)

    command_binding = write_exclusive_json(
        output_root / "command-record.json",
        {
            "argv": [str(Path(sys.executable).resolve()), str(entrypoint), str(session_path)],
            "cwd": str(source_root), "exit_code": result.exit_code,
            "outcome": "passed" if result.exit_code == 0 else "failed",
        },
    )

    def metadata(path: Path, label: str):
        raw = _read_regular(path, label, 8_388_608)
        return {
            "path": str(path), "sha256": _digest(raw), "bytes": len(raw),
            "truncated": False,
        }

    return {
        "stdout": metadata(framework, "Protocol launcher stdout"),
        "stderr": metadata(stderr, "Protocol launcher stderr"),
        "command_record": command_binding, "duration_ms": 0,
    }


def source_entry(adapter_id: str):
    if adapter_id != "protocol_actual_hub":
        raise ProtocolInstalledPending("Protocol adapter identity is foreign")
    return CONTRACT
