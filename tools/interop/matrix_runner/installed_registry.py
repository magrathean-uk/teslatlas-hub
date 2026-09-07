# SPDX-License-Identifier: AGPL-3.0-only
"""Source-fixed installed adapter dispatch; job JSON never selects code."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
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


@dataclass(frozen=True)
class InstalledDispatch:
    result: installed.InstalledExecutionResult
    supplement: Mapping[str, str]
    logs: Mapping[str, Any]


# Entries are added only after their contract, validator, launch inventory, and
# source hashes have an accepted independent review.  Runtime JSON cannot
# mutate or extend this mapping.
FIXED_INSTALLED_REGISTRY: Mapping[str, FixedInstalledAdapter] = MappingProxyType({})


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
            or any(target not in {"macos_arm64", "debian13_amd64", "debian13_arm64"}
                   or kind not in {"local", "ssh_linux", "docker_exec_pipe"}
                   for target, kind in entry.execution_by_target)
            or not all(callable(getattr(entry, name)) for name in (
                "build_session_input", "launch_adapter", "admit",
                "build_supplement", "execution_logs",
            ))):
        raise InstalledRegistryError("fixed installed adapter registry entry is invalid")
    return entry


def _session_input(path: Path | str, supplied: Mapping[str, Any], entry: FixedInstalledAdapter,
                   cell: Mapping[str, Any], descriptor: Mapping[str, Any]) -> tuple[Path | str, Mapping[str, Any]]:
    try:
        import json
        from jsonschema import Draft202012Validator
        raw = read_bound_file(file_binding(path), label="matrix session input", maximum=1_048_576)
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
    expected_broker = "stdio" if expected_execution == "docker_exec_pipe" else "unix"
    if actual["broker"]["kind"] != expected_broker:
        raise InstalledRegistryError("matrix session broker differs from fixed execution topology")
    if tuple(actor["id"] for actor in actual["actors"]) != entry.required_actor_ids:
        raise InstalledRegistryError("matrix session changes the fixed actor order")
    contract_stage = actual["case_contract"]
    if (contract_stage["root"]["sha256"] != entry.contract.manifest_sha256
            or contract_stage["local"]["sha256"] != entry.contract.manifest_sha256):
        raise InstalledRegistryError("matrix session contract staging is foreign")
    header = strict_json(read_bound_file(actual["header"]["local"], label="matrix evidence header", maximum=1_048_576))
    if not isinstance(header, dict) or header.get("adapter") != entry.adapter_id or header.get("cell_id") != cell["id"]:
        raise InstalledRegistryError("matrix evidence header identity is invalid")
    validate_profile_inputs(
        actual["inputs"]["profile_manifest"], actual["inputs"]["profile_members"],
        header.get("profile_sha256"),
    )
    return path, actual


def dispatch(job: Mapping[str, Any], cell: Mapping[str, Any], config: Mapping[str, Any],
             matrix: Mapping[str, Any], *,
             registry: Mapping[str, FixedInstalledAdapter] = FIXED_INSTALLED_REGISTRY,
             session_factory: Any = None) -> InstalledDispatch:
    """Run one source-registered adapter through the shared close-before-Ack path."""
    adapter_id = job.get("adapter")
    client_id = cell.get("client_id")
    entry = _entry(adapter_id, client_id, registry)
    try:
        contract = load_reviewed_contract(adapter_id, {adapter_id: entry.contract})
    except WireError as error:
        raise InstalledRegistryError("reviewed adapter contract is invalid") from error
    required_cases = matrix["clients"][client_id]["required_cases"]
    if list(contract.manifest["required_cases"]) != list(required_cases):
        raise InstalledRegistryError("reviewed adapter contract changes required cases")
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

    def build(descriptor, running):
        path, value = entry.build_session_input(job, cell, config, matrix, contract, descriptor, running)
        return _session_input(path, value, entry, cell, descriptor)

    def launch(session_input_path, session_input):
        return entry.launch_adapter(job, cell, config, contract, session_input_path, session_input)

    def admit(normalized, actors):
        return entry.admit(job, cell, config, matrix, contract, normalized, actors)

    try:
        result = installed.execute_installed(
            session_config, registration_inventory,
            build_session_input=build, launch_adapter=launch, admit=admit,
            session_factory=session_factory,
        )
        supplement = entry.build_supplement(job, cell, config, matrix, contract, result)
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
    return InstalledDispatch(result=result, supplement=dict(supplement), logs=dict(logs))
