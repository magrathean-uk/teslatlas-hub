# SPDX-License-Identifier: AGPL-3.0-only
"""Source-fixed installed adapter dispatch; job JSON never selects code."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import inspect
import os
from pathlib import Path
import re
import ssl
from types import MappingProxyType
from typing import Any, Callable, Mapping

from . import installed
from .adapter_wire import (
    LoadedContract,
    ReviewedContract,
    WireError,
    file_binding,
    load_reviewed_contract,
    read_bound_file,
    strict_json,
    validate_profile_inputs,
)


class InstalledRegistryPending(RuntimeError):
    pass


class InstalledRegistryError(RuntimeError):
    pass


@dataclass(frozen=True)
class FixedInstalledAdapter:
    adapter_id: str
    client_id: str
    contract: ReviewedContract
    required_actor_ids: tuple[str, ...]
    execution_by_target: tuple[tuple[str, str], ...]
    build_session_input: Callable[..., tuple[Path | str, Mapping[str, Any]]]
    launch_adapter: Callable[..., Any]
    admit: Callable[..., None]
    build_supplement: Callable[..., Mapping[str, str]]
    execution_logs: Callable[..., Mapping[str, Any]]
    runtime_inventory: Callable[..., Mapping[str, Any]] | None = None
    # The default follows the target execution transport.  A reviewed adapter
    # can bind a different broker only in this source-fixed field.
    broker_kind_by_target: tuple[tuple[str, str], ...] = ()


@dataclass(frozen=True)
class InstalledDispatch:
    result: installed.InstalledExecutionResult
    supplement: Mapping[str, str]
    logs: Mapping[str, Any]
    admission: Any = None
    runtime_actual: Mapping[str, Any] | None = None


# Entries are added only after their contract, validator, launch inventory, and
# source hashes have an accepted independent review.  Runtime JSON cannot
# mutate or extend this mapping.
from . import protocol_installed as _protocol_installed
from . import swift_installed as _swift_installed
from . import typescript_installed as _typescript_installed

FIXED_INSTALLED_REGISTRY: Mapping[str, FixedInstalledAdapter] = MappingProxyType({
    "protocol_actual_hub": FixedInstalledAdapter(
        adapter_id="protocol_actual_hub", client_id="protocol_actual_hub",
        contract=_protocol_installed.source_entry("protocol_actual_hub"),
        required_actor_ids=("protocol_http",),
        execution_by_target=_protocol_installed.execution_by_target(),
        broker_kind_by_target=_protocol_installed.broker_kind_by_target(),
        build_session_input=_protocol_installed.build_session_input,
        launch_adapter=_protocol_installed.launch_adapter,
        admit=_protocol_installed.admit,
        build_supplement=_protocol_installed.build_supplement,
        execution_logs=_protocol_installed.execution_logs,
        runtime_inventory=_protocol_installed.runtime_inventory,
    ),
    "typescript_node": FixedInstalledAdapter(
        adapter_id="typescript_node", client_id="typescript_node",
        contract=_typescript_installed.CONTRACTS["typescript_node"],
        required_actor_ids=("sdk_node",),
        execution_by_target=_typescript_installed._execution_by_target("typescript_node"),
        build_session_input=_typescript_installed.build_session_input,
        launch_adapter=_typescript_installed.launch_adapter,
        admit=_typescript_installed.admit,
        build_supplement=_typescript_installed.build_supplement,
        execution_logs=_typescript_installed.execution_logs,
        runtime_inventory=_typescript_installed.runtime_inventory,
    ),
    "typescript_browser": FixedInstalledAdapter(
        adapter_id="typescript_browser", client_id="typescript_browser",
        contract=_typescript_installed.CONTRACTS["typescript_browser"],
        required_actor_ids=("sdk_browser",),
        execution_by_target=_typescript_installed._execution_by_target("typescript_browser"),
        build_session_input=_typescript_installed.build_session_input,
        launch_adapter=_typescript_installed.launch_adapter,
        admit=_typescript_installed.admit,
        build_supplement=_typescript_installed.build_supplement,
        execution_logs=_typescript_installed.execution_logs,
        runtime_inventory=_typescript_installed.runtime_inventory,
    ),
    "swift": FixedInstalledAdapter(
        adapter_id="swift", client_id="swift",
        contract=_swift_installed.CONTRACT,
        required_actor_ids=("swift_macos", "swift_linux"),
        execution_by_target=_swift_installed.execution_by_target(),
        broker_kind_by_target=_swift_installed.broker_kind_by_target(),
        build_session_input=_swift_installed.build_session_input,
        launch_adapter=_swift_installed.launch_adapter,
        admit=_swift_installed.admit,
        build_supplement=_swift_installed.build_supplement,
        execution_logs=_swift_installed.execution_logs,
        runtime_inventory=_swift_installed.runtime_inventory,
    ),
})


def _private_json(binding: Mapping[str, str], label: str) -> Mapping[str, Any]:
    try:
        raw = read_bound_file(binding, label=label, maximum=1_048_576)
        value = strict_json(raw)
    except (WireError, OSError) as error:
        raise InstalledRegistryError(label + " is invalid") from error
    if not isinstance(value, dict):
        raise InstalledRegistryError(label + " is not an object")
    return value


def _entry(adapter_id: str, client_id: str,
           registry: Mapping[str, FixedInstalledAdapter]) -> FixedInstalledAdapter:
    entry = registry.get(adapter_id)
    if entry is None:
        raise InstalledRegistryPending("pending: reviewed installed adapter launch registry is unavailable")
    if (not isinstance(entry, FixedInstalledAdapter)
            or entry.adapter_id != adapter_id or entry.client_id != client_id
            or not entry.required_actor_ids
            or len(entry.required_actor_ids) != len(set(entry.required_actor_ids))
            or len(entry.execution_by_target) != len(dict(entry.execution_by_target))
            or (entry.broker_kind_by_target and (
                len(entry.broker_kind_by_target) != len(dict(entry.broker_kind_by_target))
                or {target for target, _kind in entry.broker_kind_by_target}
                   != {target for target, _kind in entry.execution_by_target}
                or any(kind not in {"unix", "stdio"}
                       for _target, kind in entry.broker_kind_by_target)
            ))
            or any(target not in {"macos_arm64", "debian13_amd64", "debian13_arm64"}
                   or kind not in {"local", "ssh_linux", "docker_exec_pipe"}
                   for target, kind in entry.execution_by_target)
            or not all(callable(getattr(entry, name)) for name in (
                "build_session_input", "launch_adapter", "admit",
                "build_supplement", "execution_logs", "runtime_inventory",
            ))):
        raise InstalledRegistryError("fixed installed adapter registry entry is invalid")
    return entry


def _canonical_staged_path(value: Any, label: str) -> Path:
    if not isinstance(value, str) or not value.startswith("/"):
        raise InstalledRegistryError(label + " path is invalid")
    path = Path(value)
    if str(path.resolve(strict=False)) != value:
        raise InstalledRegistryError(label + " path is not canonical")
    return path


def _staged_pair(value: Any, label: str, *, deadline=None) -> tuple[bytes, bytes]:
    """Read both root and local staged bytes and require exact parity."""
    if not isinstance(value, Mapping) or set(value) != {"id", "root", "local"}:
        raise InstalledRegistryError(label + " staging is invalid")
    root = value["root"]
    local = value["local"]
    try:
        root_raw = read_bound_file(root, label=label + " root", maximum=1_048_576, deadline=deadline)
        local_raw = read_bound_file(local, label=label + " local", maximum=1_048_576, deadline=deadline)
    except (WireError, OSError) as error:
        raise InstalledRegistryError(label + " staging cannot be read") from error
    if root_raw != local_raw or root.get("sha256") != local.get("sha256"):
        raise InstalledRegistryError(label + " root/local staging differs")
    return root_raw, local_raw


def _validate_member_manifest(raw: bytes, label: str, *, artifact_sha256: str | None = None) -> Mapping[str, Any]:
    try:
        value = strict_json(raw)
    except WireError as error:
        raise InstalledRegistryError(label + " is not strict JSON") from error
    if not isinstance(value, Mapping):
        raise InstalledRegistryError(label + " is not an object")
    artifact_manifest = artifact_sha256 is not None or "artifact_sha256" in value
    if artifact_manifest:
        required = {"schema_version", "artifact_sha256", "files"}
    else:
        required = {"schema_version", "build_record", "files"}
    if set(value) != required or value.get("schema_version") != 1:
        raise InstalledRegistryError(label + " has an invalid closed shape")
    if artifact_sha256 is not None and value.get("artifact_sha256") != artifact_sha256:
        raise InstalledRegistryError(label + " is bound to a foreign artifact")
    if not artifact_manifest:
        build = value.get("build_record")
        if not isinstance(build, Mapping) or set(build) != {"path", "sha256"} or not re.fullmatch(r"[0-9a-f]{64}", str(build.get("sha256"))):
            raise InstalledRegistryError(label + " build record binding is invalid")
    files = value.get("files")
    if not isinstance(files, list) or not files:
        raise InstalledRegistryError(label + " file inventory is empty")
    paths = []
    for item in files:
        if not isinstance(item, Mapping) or set(item) != {"path", "bytes", "mode", "sha256"}:
            raise InstalledRegistryError(label + " file inventory record is invalid")
        path = item["path"]
        if (not isinstance(path, str) or path.startswith("/") or ".." in Path(path).parts
                or "\x00" in path or "\n" in path or type(item["bytes"]) is not int
                or item["bytes"] < 0 or type(item["mode"]) is not int
                or not re.fullmatch(r"[0-9a-f]{64}", str(item["sha256"]))):
            raise InstalledRegistryError(label + " file inventory record is invalid")
        paths.append(path)
    if paths != sorted(set(paths)):
        raise InstalledRegistryError(label + " file inventory is not sorted and unique")
    return value


def _validate_output_reservations(outputs: Mapping[str, Any], *, protected_roots=(), deadline=None) -> None:
    if not isinstance(outputs, Mapping) or set(outputs) != {
        "normalized", "actor_evidence", "coordination_dir", "framework_log"
    }:
        raise InstalledRegistryError("matrix outputs are incomplete")
    paths = []
    for name, raw in outputs.items():
        path = _canonical_staged_path(raw, "matrix output " + name)
        # All private outputs must be external to the source checkout.  A
        # pre-existing output/control path is stale evidence and is rejected
        # before the child gets an opportunity to read or overwrite it.
        if any(path == root or root in path.parents for root in protected_roots):
            raise InstalledRegistryError("matrix output is inside the source checkout")
        if os.path.lexists(path):
            raise InstalledRegistryError("matrix output path is not fresh")
        paths.append(path)
    for index, path in enumerate(paths):
        for other in paths[index + 1:]:
            if path == other or path in other.parents or other in path.parents:
                raise InstalledRegistryError("matrix output paths overlap")
        if deadline is not None:
            deadline.remaining()


def _certificate_der_bytes(value: Any) -> bytes:
    if type(value) is bytes and value:
        return value
    if (type(value) is str and value and len(value) % 2 == 0
            and re.fullmatch(r"[0-9a-fA-F]+", value)):
        return bytes.fromhex(value)
    raise InstalledRegistryError("matrix certificate DER encoding is invalid")


def _session_input(path: Path | str, supplied: Mapping[str, Any], entry: FixedInstalledAdapter,
                   cell: Mapping[str, Any], descriptor: Mapping[str, Any], deadline=None,
                   job: Mapping[str, Any] | None = None,
                   config: Mapping[str, Any] | None = None,
                   contract: LoadedContract | None = None,
                   running: Mapping[str, Any] | None = None) -> tuple[Path | str, Mapping[str, Any]]:
    try:
        import json
        from jsonschema import Draft202012Validator
        raw = read_bound_file(
            file_binding(path, maximum=1_048_576, deadline=deadline),
            label="matrix session input", maximum=1_048_576, deadline=deadline,
        )
        actual = strict_json(raw)
        schema = json.loads(Path(__file__).with_name("adapter_wire.schema.json").read_text(encoding="utf-8"))
    except Exception as error:
        raise InstalledRegistryError("matrix session input cannot be read") from error
    if actual != supplied or next(Draft202012Validator(schema).iter_errors(actual), None):
        raise InstalledRegistryError("matrix session input violates the closed schema")
    if (actual["adapter_id"] != entry.adapter_id or actual["client_id"] != entry.client_id
            or actual["cell_id"] != cell["id"] or actual["host_session"] != descriptor):
        raise InstalledRegistryError("matrix session input has a foreign identity")
    expected_execution = dict(entry.execution_by_target).get(cell["hub_target"])
    if expected_execution is None:
        raise InstalledRegistryError("fixed installed entry lacks this target")
    expected_broker = dict(entry.broker_kind_by_target).get(
        cell["hub_target"],
        "stdio" if expected_execution == "docker_exec_pipe" else "unix",
    )
    if actual["broker"]["kind"] != expected_broker:
        raise InstalledRegistryError("matrix session broker differs from fixed execution topology")
    if tuple(actor["id"] for actor in actual["actors"]) != entry.required_actor_ids:
        raise InstalledRegistryError("matrix session changes the fixed actor order")
    if actual["broker"].get("socket_path") != descriptor.get("broker_socket"):
        raise InstalledRegistryError("matrix session broker socket is foreign")
    contract_stage = actual["case_contract"]
    if (contract_stage["root"]["sha256"] != entry.contract.manifest_sha256
            or contract_stage["local"]["sha256"] != entry.contract.manifest_sha256):
        raise InstalledRegistryError("matrix session contract staging is foreign")
    header = strict_json(read_bound_file(
        actual["header"]["local"], label="matrix evidence header", maximum=1_048_576, deadline=deadline
    ))
    if not isinstance(header, dict) or header.get("adapter") != entry.adapter_id or header.get("cell_id") != cell["id"]:
        raise InstalledRegistryError("matrix evidence header identity is invalid")
    if job is not None:
        expected_header = {
            "schema_version": 1, "execution_kind": "actual_hub_acceptance",
            "adapter": job["adapter"], "cell_id": job["cell_id"],
            "product_version": config.get("product_version") if config else None,
            "profile_id": config.get("profile", {}).get("id") if config else None,
            "profile_revision": config.get("profile", {}).get("revision") if config else None,
            "profile_sha256": config.get("profile", {}).get("sha256") if config else None,
            "source_identities": job.get("source_identities"),
            "artifacts": job.get("artifacts"), "runtime": job.get("runtime"),
        }
        if set(header) != set(expected_header):
            raise InstalledRegistryError("matrix evidence header fields are invalid")
        for key, expected in expected_header.items():
            if expected is not None and header.get(key) != expected:
                raise InstalledRegistryError("matrix evidence header provenance is invalid")
    if contract is not None:
        declared = contract.manifest.get("actors")
        if isinstance(declared, list):
            declared_by_id = {actor.get("id"): actor for actor in declared if isinstance(actor, dict)}
            for actor in actual["actors"]:
                fixed = declared_by_id.get(actor["id"])
                if (fixed is None or actor.get("kind") != fixed.get("kind")
                        or fixed.get("required") is not True):
                    raise InstalledRegistryError("matrix session actor provenance is invalid")
                if actor.get("artifact_roles") != sorted(set(actor.get("artifact_roles", []))) or actor.get("source_roles") != sorted(set(actor.get("source_roles", []))):
                    raise InstalledRegistryError("matrix session actor roles are not canonical")
                if actor.get("phase_contract") is not None:
                    _staged_pair(actor["phase_contract"], "matrix actor phase contract", deadline=deadline)
                input_manifest_raw, _ = _staged_pair(actor["input_manifest"], "matrix actor input manifest", deadline=deadline)
                _validate_member_manifest(input_manifest_raw, "matrix actor input manifest")
                known_roles = {item.get("role") for item in (job or {}).get("artifacts", [])}
                known_sources = {item.get("role") for item in (job or {}).get("source_identities", [])}
                if (not set(actor.get("artifact_roles", [])).issubset(known_roles)
                        or not set(actor.get("source_roles", [])).issubset(known_sources)):
                    raise InstalledRegistryError("matrix session actor role provenance is foreign")
    for name in ("case_contract", "header", "inputs", "actors"):
        if deadline is not None:
            deadline.remaining()
    _staged_pair(actual["header"], "matrix evidence header", deadline=deadline)
    _staged_pair(actual["case_contract"], "matrix case contract", deadline=deadline)
    protected_roots = [Path(__file__).resolve().parents[3]]
    for name, binding in actual["inputs"].items():
        if name == "certificate_der_sha256":
            continue
        if name == "product_inputs":
            if not isinstance(binding, list) or len(binding) > 4:
                raise InstalledRegistryError("matrix product inputs are invalid")
            for index, product in enumerate(binding):
                if not isinstance(product, dict) or set(product) != {"artifact_role", "staged", "installed_manifest", "local_root"}:
                    raise InstalledRegistryError("matrix product input is invalid")
                _staged_pair(product["staged"], f"matrix product input {index}", deadline=deadline)
                if product["installed_manifest"] is not None:
                    manifest_raw, _ = _staged_pair(product["installed_manifest"], f"matrix product manifest {index}", deadline=deadline)
                    expected_manifest_hash = next(
                        (artifact.get("sha256") for artifact in (job or {}).get("artifacts", [])
                         if artifact.get("role") == product.get("artifact_role")),
                        None,
                    )
                    _validate_member_manifest(
                        manifest_raw, f"matrix product manifest {index}",
                        artifact_sha256=expected_manifest_hash,
                    )
                if product["local_root"] is not None:
                    protected_roots.append(_canonical_staged_path(product["local_root"], "matrix product local root"))
        else:
            _staged_pair(binding, "matrix input " + name, deadline=deadline)
    scenario_digest = descriptor.get("scenario_sha256")
    if isinstance(scenario_digest, str) and actual["inputs"]["scenario"]["local"]["sha256"] != scenario_digest:
        raise InstalledRegistryError("matrix scenario staging is foreign")
    certificate_raw, _ = _staged_pair(actual["inputs"]["certificate"], "matrix certificate", deadline=deadline)
    try:
        certificate_der = ssl.PEM_cert_to_DER_cert(certificate_raw.decode("ascii"))
    except (UnicodeDecodeError, ValueError, ssl.SSLError) as error:
        raise InstalledRegistryError("matrix certificate is not a PEM certificate") from error
    certificate_digest = hashlib.sha256(_certificate_der_bytes(certificate_der)).hexdigest()
    if actual["inputs"]["certificate_der_sha256"] != certificate_digest:
        raise InstalledRegistryError("matrix certificate DER digest is foreign")
    if isinstance(running, Mapping) and isinstance(running.get("proof"), Mapping):
        proof = running["proof"]
        proof_config = proof.get("config") if isinstance(proof.get("config"), Mapping) else {}
        proof_tls = proof.get("tls") if isinstance(proof.get("tls"), Mapping) else {}
        if (proof_config.get("scenario_sha256") is not None
                and actual["inputs"]["scenario"]["local"]["sha256"] != proof_config["scenario_sha256"]):
            raise InstalledRegistryError("matrix scenario is outside the root observation")
        if (proof_tls.get("certificate_der_sha256") is not None
                and actual["inputs"]["certificate_der_sha256"] != proof_tls["certificate_der_sha256"]):
            raise InstalledRegistryError("matrix certificate is outside the root observation")
    if job is not None:
        product_inputs = actual["inputs"]["product_inputs"]
        expected_products = {
            artifact["role"]: artifact for artifact in job.get("artifacts", [])
            if artifact.get("role") != "hub_executable"
        }
        if entry.client_id == "protocol_actual_hub" and product_inputs:
            raise InstalledRegistryError("Protocol session unexpectedly stages client products")
        if entry.client_id != "protocol_actual_hub" and set(item["artifact_role"] for item in product_inputs) != set(expected_products):
            raise InstalledRegistryError("matrix product input roles are incomplete")
        for product in product_inputs:
            expected = expected_products.get(product["artifact_role"])
            if expected is None or product["staged"]["local"]["sha256"] != expected["sha256"]:
                raise InstalledRegistryError("matrix product input is outside the selected artifact set")
            if (product["local_root"] is None) != (product["installed_manifest"] is None):
                raise InstalledRegistryError("matrix product install provenance is incomplete")
    _validate_output_reservations(actual["outputs"], protected_roots=protected_roots, deadline=deadline)
    validate_profile_inputs(
        actual["inputs"]["profile_manifest"], actual["inputs"]["profile_members"],
        header.get("profile_sha256"), deadline=deadline,
    )
    return path, actual


def _launch_without_deadline(callback, *args, session=None, running=None):
    """Keep direct unit callers compatible while preserving root authority."""
    try:
        parameters = inspect.signature(callback).parameters.values()
        accepts_kwargs = any(item.kind is inspect.Parameter.VAR_KEYWORD for item in parameters)
        names = {item.name for item in inspect.signature(callback).parameters.values()}
    except (TypeError, ValueError):
        accepts_kwargs, names = True, set()
    keywords = {}
    if accepts_kwargs or "session" in names:
        keywords["session"] = session
    if accepts_kwargs or "running" in names:
        keywords["running"] = running
    return callback(*args, **keywords)


def dispatch(job: Mapping[str, Any], cell: Mapping[str, Any], config: Mapping[str, Any],
             matrix: Mapping[str, Any], *,
             registry: Mapping[str, FixedInstalledAdapter] = FIXED_INSTALLED_REGISTRY,
             session_factory: Any = None) -> InstalledDispatch:
    """Run one source-registered adapter through the shared close-before-Ack path."""
    adapter_id = job.get("adapter")
    client_id = cell.get("client_id")
    entry = _entry(adapter_id, client_id, registry)
    try:
        # These contracts are immutable hash-bound source artifacts in sibling
        # checkouts, rather than private runtime JSON. Other adapters keep the
        # stricter owner-only default until their review supplies that boundary.
        contract = load_reviewed_contract(
            adapter_id, {adapter_id: entry.contract},
            private=adapter_id not in {
                "protocol_actual_hub", "typescript_node", "typescript_browser",
            },
        )
    except WireError as error:
        raise InstalledRegistryError("reviewed adapter contract is invalid") from error
    required_cases = matrix["clients"][client_id]["required_cases"]
    if list(contract.manifest["required_cases"]) != list(required_cases):
        raise InstalledRegistryPending("pending: reviewed installed adapter contract and matrix case sets differ")
    actors = contract.manifest.get("actors")
    if (not isinstance(actors, list)
            or tuple(actor.get("id") for actor in actors if isinstance(actor, dict)) != entry.required_actor_ids
            or any(not isinstance(actor, dict) or set(actor) != {"id", "kind", "required"}
                   or actor["required"] is not True for actor in actors)):
        raise InstalledRegistryError("reviewed adapter contract changes fixed actors")
    expected_execution = dict(entry.execution_by_target).get(cell.get("hub_target"))
    if expected_execution != job.get("client_execution", {}).get("kind"):
        raise InstalledRegistryError("client execution differs from fixed target topology")
    session_config = _private_json(job["installed_session"]["config"], "installed session config")
    registration_inventory = _private_json(
        job["installed_session"]["registration_inventory"], "installed registration inventory"
    )

    def build(descriptor, running, deadline=None):
        path, value = installed._call_with_deadline(
            entry.build_session_input, job, cell, config, matrix, contract, descriptor, running,
            deadline=deadline,
        ) if deadline is not None else entry.build_session_input(
            job, cell, config, matrix, contract, descriptor, running
        )
        built = _session_input(
            path, value, entry, cell, descriptor, deadline,
            job, config, contract, running,
        )
        runtime_holder["session_input"] = built[1]
        return built

    def launch(session_input_path, session_input, deadline=None, session=None, running=None):
        return installed._call_with_deadline(
            entry.launch_adapter, job, cell, config, contract, session_input_path, session_input,
            deadline=deadline, session=session, running=running,
        ) if deadline is not None else _launch_without_deadline(
            entry.launch_adapter, job, cell, config, contract, session_input_path, session_input,
            session=session, running=running,
        )

    def admit(normalized, actors, deadline=None, admission_views=None):
        if admission_views is None:
            # Direct unit harnesses may call the callback without the shared
            # supervisor.  The production supervisor sees this callback's
            # keyword and always supplies the runner-owned views.
            return installed._call_with_deadline(
                entry.admit, job, cell, config, matrix, contract, normalized, actors,
                deadline=deadline,
            )
        if not isinstance(admission_views, Mapping):
            raise InstalledRegistryError("reviewed installed admission views are invalid")
        session_value = runtime_holder.get("session_input")
        if not isinstance(session_value, Mapping):
            raise InstalledRegistryError("runner-owned runtime inventory lacks SessionInput")
        runtime_view = installed._call_with_deadline(
            entry.runtime_inventory, job, cell, config, contract, session_value,
            deadline=deadline,
        )
        if not isinstance(runtime_view, Mapping):
            raise InstalledRegistryError("runner-owned runtime inventory is invalid")
        runtime_holder["runtime_view"] = installed._freeze(runtime_view)
        runtime_holder["controller_view"] = installed._freeze(admission_views["controller_observations"])
        views = dict(admission_views)
        views["runtime"] = runtime_holder["runtime_view"]
        runtime_holder["admission_views"] = installed._freeze(views)
        kwargs = {"admission_views": installed._freeze(views), "deadline": deadline}
        try:
            parameters = inspect.signature(entry.admit).parameters
        except (TypeError, ValueError):
            parameters = {}
        if "runtime_context" in parameters or any(
            item.kind is inspect.Parameter.VAR_KEYWORD for item in parameters.values()
        ):
            kwargs["runtime_context"] = runtime_holder
        return installed._call_with_deadline(
            entry.admit, job, cell, config, matrix, contract, normalized, actors, **kwargs,
        )

    try:
        runtime_holder = {}
        execute_kwargs = {}
        timeout_seconds = job.get("timeout_seconds")
        if type(timeout_seconds) is int and timeout_seconds > 0:
            execute_kwargs["cell_timeout_ms"] = timeout_seconds * 1000
        result = installed.execute_installed(
            session_config, registration_inventory,
            build_session_input=build, launch_adapter=launch, admit=admit,
            session_factory=session_factory, **execute_kwargs,
        )
        try:
            supplement_parameters = inspect.signature(entry.build_supplement).parameters
        except (TypeError, ValueError):
            supplement_parameters = {}
        supplement_kwargs = {}
        if "runtime_context" in supplement_parameters or any(
            item.kind is inspect.Parameter.VAR_KEYWORD for item in supplement_parameters.values()
        ):
            supplement_kwargs["runtime_context"] = runtime_holder
        supplement = entry.build_supplement(job, cell, config, matrix, contract, result, **supplement_kwargs)
    except Exception as error:
        raise InstalledRegistryError("installed adapter supervision failed") from error
    try:
        read_bound_file(supplement, label="installed supplement", maximum=8_388_608)
    except (WireError, OSError) as error:
        raise InstalledRegistryError("installed supplement binding is invalid") from error
    try:
        logs = entry.execution_logs(job, cell, config, contract, result)
    except Exception as error:
        raise InstalledRegistryError("installed execution log collection failed") from error
    if not isinstance(logs, dict):
        raise InstalledRegistryError("installed execution log record is invalid")
    return InstalledDispatch(
        result=result, supplement=dict(supplement), logs=dict(logs),
        admission=getattr(result, "admission", None),
        # The matrix receipt must expose the normalized runtime returned by the
        # fixed adapter's runner-owned inventory callback.  Copying job.runtime
        # here would turn expected metadata into an asserted observation.
        runtime_actual=(
            dict(runtime_holder["runtime_view"].get("runtime_actual"))
            if isinstance(runtime_holder.get("runtime_view"), Mapping)
            and isinstance(runtime_holder["runtime_view"].get("runtime_actual"), Mapping)
            else None
        ),
    )
