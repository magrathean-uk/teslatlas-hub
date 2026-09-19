# SPDX-License-Identifier: AGPL-3.0-only
"""Runner-owned installed completion and cleanup ordering.

The installed adapter is an untrusted local process.  The runner therefore
owns its process generation from the instant it is launched, keeps one cell
deadline across startup and completion, and uses a separate bounded cleanup
obligation after a failure.
"""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import inspect
import json
from pathlib import Path
import subprocess
import time
from types import MappingProxyType
from typing import Any, Callable, Mapping

from .adapter_wire import (
    file_binding,
    read_bound_file,
    sha256_bytes,
    strict_json,
    validate_ready,
    write_ack,
    write_exclusive_json,
)
from .process_generation import OwnedProcess
try:  # ``run.py`` also loads this package as top-level ``matrix_runner``.
    from ..installed_hosts.bounded import Deadline
except ImportError:  # pragma: no cover - exercised by the real run.py package path
    from installed_hosts.bounded import Deadline


DEFAULT_CELL_TIMEOUT_MS = 3_600_000
DEFAULT_CLEANUP_TIMEOUT_MS = 45_000


class InstalledExecutionError(RuntimeError):
    """A permanent failure of one installed adapter cell."""

    def __init__(self, message, *, cleanup_errors=()):
        super().__init__(message)
        self.cleanup_errors = tuple(cleanup_errors)


@dataclass(frozen=True)
class InstalledExecutionResult:
    exit_code: int
    session_evidence: Any
    ready: Mapping[str, Any]
    ack: Mapping[str, Any]
    completion: Mapping[str, Any]
    # Adapter-specific semantic admission is returned only by reviewed fixed
    # registry callbacks.  The runner still owns lifecycle and cleanup; this
    # value is merely the closed predicate result used to normalize v2 rows.
    admission: Any = None


def canonical_json_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def proof_sha256(value: Mapping[str, Any]) -> str:
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def _freeze(value: Any) -> Any:
    """Deep-freeze runner-owned admission facts before handing them to a child."""
    if isinstance(value, Mapping):
        return MappingProxyType({key: _freeze(item) for key, item in value.items()})
    if isinstance(value, (list, tuple)):
        return tuple(_freeze(item) for item in value)
    return value


def _digest(value: Any, label: str) -> str:
    if not isinstance(value, str) or len(value) != 64 or any(char not in "0123456789abcdef" for char in value):
        raise InstalledExecutionError("runner-owned admission {} digest is invalid".format(label))
    return value


def build_controller_admission_view(
    session: Any, session_input: Mapping[str, Any], *, deadline=None,
) -> Mapping[str, Any]:
    """Construct the closed controller view from the retained session records.

    The view is deliberately made from the controller's detached snapshots,
    never from adapter JSON.  A reviewed adapter receives this value before
    close and cannot mutate it.
    """
    observations_snapshot = getattr(session, "observations_snapshot", None)
    records_snapshot = getattr(session, "operation_records_snapshot", None)
    if not callable(observations_snapshot) or not callable(records_snapshot):
        raise InstalledExecutionError("installed session lacks runner-owned admission snapshots")
    observations = tuple(observations_snapshot(deadline))
    records = tuple(records_snapshot(deadline))
    session_id = session_input.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        raise InstalledExecutionError("session input lacks admission session identity")
    by_sequence = {}
    for record in records:
        if not isinstance(record, Mapping) or record.get("status") != "admitted":
            continue
        binding = record.get("processed_binding")
        proof = record.get("proof")
        if not isinstance(binding, Mapping) or type(binding.get("sequence")) is not int:
            continue
        if not isinstance(proof, Mapping):
            continue
        if binding["sequence"] in by_sequence:
            raise InstalledExecutionError("controller operation sequence is duplicated")
        by_sequence[binding["sequence"]] = record
    view = {}
    previous_sequence = None
    for proof in observations:
        if not isinstance(proof, Mapping):
            raise InstalledExecutionError("controller observation is not an object")
        sequence = proof.get("sequence")
        if type(sequence) is not int or sequence <= 0 or sequence in view:
            raise InstalledExecutionError("controller observation sequence is invalid")
        if previous_sequence is not None and sequence <= previous_sequence:
            raise InstalledExecutionError("controller observation sequence is not increasing")
        if proof.get("session_id") != session_id:
            raise InstalledExecutionError("controller observation belongs to a foreign session")
        record = by_sequence.get(sequence)
        if record is None or record.get("proof") != proof:
            raise InstalledExecutionError("controller observation lacks its retained operation record")
        started = record.get("started_monotonic_ns")
        finished = record.get("finished_monotonic_ns")
        observed_at = record.get("observed_at_ms")
        if (type(started) is not int or started < 0 or type(finished) is not int
                or finished < started or type(observed_at) is not int or observed_at <= 0):
            raise InstalledExecutionError("controller operation timing is invalid")
        result_hash = _digest(record.get("result_sha256"), "result")
        config = proof.get("config")
        discovery = proof.get("discovery")
        service = proof.get("service")
        tls = proof.get("tls")
        if not all(isinstance(item, Mapping) for item in (config, discovery, service, tls)):
            raise InstalledExecutionError("controller observation provenance is incomplete")
        generation = service.get("generation")
        if not isinstance(generation, str) or not generation:
            raise InstalledExecutionError("controller service generation is invalid")

        def invitation(value):
            if value is None:
                return None
            if not isinstance(value, Mapping):
                raise InstalledExecutionError("controller invitation summary is invalid")
            pairing_id, expires_at = value.get("pairing_id"), value.get("expires_at_ms")
            if not isinstance(pairing_id, str) or type(expires_at) is not int or expires_at <= 0:
                raise InstalledExecutionError("controller invitation summary is invalid")
            return {"pairing_id": pairing_id, "expires_at_ms": expires_at}

        operation = record.get("operation")
        if operation not in {"verify", "advance-once", "pair", "revoke", "start"}:
            raise InstalledExecutionError("controller running operation is invalid")
        if operation == "verify":
            transition = None
        else:
            if previous_sequence is None:
                raise InstalledExecutionError("controller transition lacks a prior observation")
            transition = {"kind": operation, "from_sequence": previous_sequence}
            if operation == "advance-once":
                advance = record.get("advance")
                if not isinstance(advance, Mapping) or set(advance) != {
                    "before_store_sha256", "after_store_sha256", "scenario_sha256", "seed_sha256"
                }:
                    raise InstalledExecutionError("controller advance transition is incomplete")
                for key in advance:
                    _digest(advance[key], "advance." + key)
                transition.update(advance)
                transition["pre_advance_verify_sequence"] = previous_sequence
            elif operation == "revoke":
                request = record.get("request")
                device_id = request.get("device_id") if isinstance(request, Mapping) else None
                if not isinstance(device_id, str) or not device_id:
                    raise InstalledExecutionError("controller revoke transition lacks device identity")
                transition["device_id"] = device_id
            elif operation == "start":
                stopped = [
                    item for item in records
                    if isinstance(item, Mapping) and item.get("status") == "admitted"
                    and item.get("operation") == "stop"
                    and isinstance(item.get("processed_binding"), Mapping)
                ]
                if not stopped or type(stopped[-1]["processed_binding"].get("sequence")) is not int:
                    raise InstalledExecutionError("controller start transition lacks its stopped sequence")
                transition["stopped_sequence"] = stopped[-1]["processed_binding"]["sequence"]
        active_invitation = invitation(record.get("invitation"))
        expired_invitation = invitation(record.get("expired_invitation"))
        if active_invitation is None or expired_invitation is None:
            raise InstalledExecutionError("running controller observation lacks invitation identities")
        row = {
            "schema_version": 1,
            "session_id": session_id,
            "sequence": sequence,
            "operation": operation,
            "state": "running",
            "started_monotonic_ns": started,
            "finished_monotonic_ns": finished,
            "observed_at_ms": observed_at,
            "result_sha256": result_hash,
            "proof_sha256": proof_sha256(proof),
            "scenario_sha256": _digest(config.get("scenario_sha256"), "scenario"),
            "seed_sha256": _digest(config.get("seed_sha256"), "seed"),
            "store_id": config.get("store_id"),
            "store_schema_version": config.get("store_schema_version"),
            "hub_id": discovery.get("hub_id"),
            "service_generation": generation,
            "invitations": {
                "active": active_invitation,
                "expired": expired_invitation,
            },
            "transition": transition,
        }
        if not isinstance(row["store_id"], str) or not row["store_id"] or type(row["store_schema_version"]) is not int:
            raise InstalledExecutionError("controller store identity is invalid")
        view[sequence] = row
        previous_sequence = sequence
    if not view:
        raise InstalledExecutionError("controller admission view is empty")
    operation_facts = []
    for record in records:
        if not isinstance(record, Mapping) or record.get("status") != "admitted":
            continue
        binding = record.get("processed_binding")
        if not isinstance(binding, Mapping) or type(binding.get("sequence")) is not int:
            raise InstalledExecutionError("controller operation binding is invalid")
        request = record.get("request")
        fact = {
            "operation": record.get("operation"),
            "state": record.get("state"),
            "sequence": binding["sequence"],
            "request": {
                key: request.get(key)
                for key in ("op", "device_id")
                if isinstance(request, Mapping) and key in request
            },
            "started_monotonic_ns": record.get("started_monotonic_ns"),
            "finished_monotonic_ns": record.get("finished_monotonic_ns"),
            "observed_at_ms": record.get("observed_at_ms"),
            "result_sha256": record.get("result_sha256"),
        }
        if fact["operation"] not in {"verify", "stop", "start", "pair", "revoke", "advance-once"}:
            raise InstalledExecutionError("controller operation kind is invalid")
        _digest(fact["result_sha256"], "operation result")
        operation_facts.append(fact)
    journal = getattr(session, "journal", None)
    journal_binding = None
    journal_path = getattr(journal, "path", None)
    journal_digest = getattr(journal, "digest", None)
    if journal_path is not None and callable(journal_digest):
        try:
            try:
                journal_parameters = inspect.signature(journal_digest).parameters
            except (TypeError, ValueError):
                journal_parameters = {"deadline": None}
            journal_value = (
                journal_digest(deadline)
                if "deadline" in journal_parameters
                else journal_digest()
            )
            journal_binding = {
                "path": str(journal_path),
                "sha256": _digest(journal_value, "journal"),
            }
        except Exception as error:
            raise InstalledExecutionError("controller journal binding is unavailable") from error
    if journal_binding is None:
        raise InstalledExecutionError("installed session lacks a controller journal binding")
    return _freeze({
        "schema_version": 1,
        "session_id": session_id,
        "observations": view,
        "operation_records": operation_facts,
        "journal": journal_binding,
    })


def _accepts_keyword(callback, name: str, *, include_var_kwargs: bool = True) -> bool:
    try:
        parameters = inspect.signature(callback).parameters.values()
        return any(
            parameter.name == name
            or (include_var_kwargs and parameter.kind is inspect.Parameter.VAR_KEYWORD)
            for parameter in parameters
        )
    except (TypeError, ValueError):
        return True


def _deadline_from_value(value, *, default_seconds=DEFAULT_CELL_TIMEOUT_MS / 1000):
    """Normalize the public test hook without changing the production API."""
    if value is None:
        return Deadline(default_seconds)
    if isinstance(value, Deadline):
        return value
    # Existing direct supervisor tests pass an absolute monotonic timestamp.
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        remaining = float(value) - time.monotonic()
        return Deadline(max(0.001, remaining))
    if callable(getattr(value, "remaining", None)):
        return value
    raise TypeError("installed cell deadline is invalid")


def _call_with_deadline(callback, *args, deadline, **extra_keywords):
    """Call old source-fixed callbacks while requiring deadline-aware ones.

    Existing adapters are source-registered callables.  A callback that has
    not yet adopted the keyword remains usable for compatibility; reviewed
    callbacks can receive the enclosing Deadline and cannot silently create a
    new one.
    """
    try:
        parameters = inspect.signature(callback).parameters.values()
        parameter_list = tuple(parameters)
        accepts_kwargs = any(parameter.kind is inspect.Parameter.VAR_KEYWORD for parameter in parameter_list)
        accepted_names = {parameter.name for parameter in parameter_list}
    except (TypeError, ValueError):
        accepts_kwargs = True
        accepted_names = set()
    keywords = {}
    if accepts_kwargs or "deadline" in accepted_names:
        keywords["deadline"] = deadline
    for name, value in extra_keywords.items():
        if accepts_kwargs or name in accepted_names:
            keywords[name] = value
    return callback(*args, **keywords)


def _validate_bounds(session_input: Mapping[str, Any], supplied_timeout_ms):
    bounds = session_input.get("bounds") if isinstance(session_input, Mapping) else None
    if not isinstance(bounds, Mapping):
        raise InstalledExecutionError("session input lacks completion bounds")
    cell_timeout_ms = bounds.get("cell_timeout_ms")
    cleanup_timeout_ms = bounds.get("cleanup_timeout_ms")
    if (type(cell_timeout_ms) is not int or cell_timeout_ms <= 0
            or type(cleanup_timeout_ms) is not int
            or cleanup_timeout_ms != DEFAULT_CLEANUP_TIMEOUT_MS):
        raise InstalledExecutionError("session input has invalid completion bounds")
    if supplied_timeout_ms is not None and cell_timeout_ms != supplied_timeout_ms:
        raise InstalledExecutionError("session input cell deadline differs from the fixed job deadline")
    return cell_timeout_ms, cleanup_timeout_ms


def _clamp_to_declared_timeout(deadline, started, cell_timeout_ms):
    """Adopt a staged SessionInput timeout without restarting the clock."""
    end = started + cell_timeout_ms / 1000
    if hasattr(deadline, "end"):
        deadline.end = min(deadline.end, end)
    deadline.remaining()


def execute_installed(
    session_config: Mapping[str, Any], registration_inventory: Mapping[str, Any], *,
    build_session_input: Callable[..., tuple[Path | str, Mapping[str, Any]]],
    launch_adapter: Callable[..., subprocess.Popen],
    admit: Callable[..., None],
    session_factory: Any = None,
    cell_timeout_ms: int | None = None,
) -> InstalledExecutionResult:
    """Open, verify, and supervise one installed target.

    The enclosing Deadline starts before opening the host session.  It is
    passed through every bounded controller and adapter callback; cleanup on a
    failure receives a fresh fixed 45-second obligation.
    """
    if cell_timeout_ms is not None and (
        type(cell_timeout_ms) is not int or cell_timeout_ms <= 0
    ):
        raise InstalledExecutionError("fixed job cell deadline is invalid")
    if session_factory is None:
        try:
            from ..installed_hosts.session import InstalledSession
        except ImportError:  # top-level matrix_runner when invoked by run.py
            from installed_hosts.session import InstalledSession
        session_factory = InstalledSession
    started = time.monotonic()
    cell_deadline = Deadline(
        (cell_timeout_ms if cell_timeout_ms is not None else DEFAULT_CELL_TIMEOUT_MS) / 1000
    )
    session = None
    process = None
    owner = None
    handed_to_supervisor = False
    try:
        # This call is deliberately first: startup time is part of the cell.
        session = session_factory.open(session_config, registration_inventory, deadline=cell_deadline)
        cell_deadline.remaining()
        running = session.request({"op": "verify"}, deadline=cell_deadline)
        if not isinstance(running, dict) or not isinstance(running.get("proof"), dict):
            raise InstalledExecutionError("initial installed verification is invalid")
        proof = running["proof"]
        sequence = proof.get("sequence")
        if type(sequence) is not int or sequence <= 0:
            raise InstalledExecutionError("initial installed proof sequence is invalid")
        session_input_path, session_input = _call_with_deadline(
            build_session_input, session.descriptor, running, deadline=cell_deadline
        )
        if not isinstance(session_input, dict) or session_input.get("host_session") != session.descriptor:
            raise InstalledExecutionError("session input host descriptor is stale")
        declared_timeout_ms, _ = _validate_bounds(session_input, cell_timeout_ms)
        if cell_timeout_ms is None:
            _clamp_to_declared_timeout(cell_deadline, started, declared_timeout_ms)
        else:
            cell_deadline.remaining()
        # Binding the staged input is a bounded read before the child starts.
        file_binding(session_input_path, maximum=1_048_576, deadline=cell_deadline)
        process = _call_with_deadline(
            launch_adapter, session_input_path, session_input, deadline=cell_deadline,
            session=session, running=running,
        )
        cell_deadline.remaining()
        # No adapter bytes or completion records are read before ownership.
        owner = OwnedProcess(process, cell_deadline)
        handed_to_supervisor = True
        return supervise_completion(
            session, process, session_input_path, session_input,
            initial_observation={"session_sequence": sequence, "proof_sha256": proof_sha256(proof)},
            admit=admit, owner=owner, cell_deadline=cell_deadline,
        )
    except BaseException as error:
        if isinstance(error, InstalledExecutionError):
            failure = error
        elif isinstance(error, TimeoutError):
            failure = InstalledExecutionError("startup timed out")
        else:
            failure = InstalledExecutionError(str(error) or type(error).__name__)
        if not handed_to_supervisor:
            cleanup_errors = []
            if session is not None:
                try:
                    session.close(deadline=Deadline(DEFAULT_CLEANUP_TIMEOUT_MS / 1000))
                except BaseException as cleanup_error:
                    cleanup_errors.append(cleanup_error)
            if process is not None:
                try:
                    if owner is not None:
                        owner.cleanup(Deadline(DEFAULT_CLEANUP_TIMEOUT_MS / 1000))
                    else:
                        _terminate(process)
                except BaseException as cleanup_error:
                    cleanup_errors.append(cleanup_error)
            if cleanup_errors:
                raise InstalledExecutionError(
                    str(failure), cleanup_errors=cleanup_errors
                ) from failure
        raise failure


def _terminate(process: subprocess.Popen, deadline=None) -> None:
    """Best-effort fallback with no unscoped process-group signalling.

    Normal paths use an already retained OwnedProcess.  This fallback is
    only for a launcher/observation failure; it signals the exact Popen leader
    and never killpgs a PID that may already have been recycled.
    """
    if not isinstance(process, subprocess.Popen):
        return
    limit = _deadline_from_value(deadline, default_seconds=DEFAULT_CLEANUP_TIMEOUT_MS / 1000)
    try:
        owner = OwnedProcess(process, limit)
    except BaseException:
        try:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=limit.remaining())
                except (subprocess.TimeoutExpired, TimeoutError):
                    process.kill()
                    process.wait(timeout=max(0.001, limit.remaining()))
            else:
                process.wait(timeout=limit.remaining())
        except BaseException as error:
            raise error
        return
    owner.cleanup(limit)


def _wait_for_ready(path: Path, process: subprocess.Popen, deadline) -> dict[str, Any]:
    while True:
        deadline.remaining()
        if path.exists():
            value = strict_json(read_bound_file(
                file_binding(path, maximum=65_536, deadline=deadline),
                label="ready record", maximum=65_536, deadline=deadline,
            ))
            if not isinstance(value, dict):
                raise InstalledExecutionError("ready record is not an object")
            return value
        if process.poll() is not None:
            raise InstalledExecutionError("adapter exited before evidence readiness")
        time.sleep(min(0.01, deadline.remaining()))


def _validate_completion(value: Any, session_input: Mapping[str, Any], session_hash: str, deadline=None) -> tuple[bytes, bytes]:
    fields = {"schema_version", "session_id", "cell_id", "session_input_sha256", "normalized", "actor_evidence"}
    if not isinstance(value, dict) or set(value) != fields:
        raise InstalledExecutionError("adapter completion shape is invalid")
    if (
        type(value.get("schema_version")) is not int
        or value["schema_version"] != 1
        or value["session_id"] != session_input["session_id"]
        or value["cell_id"] != session_input["cell_id"]
        or value["session_input_sha256"] != session_hash
    ):
        raise InstalledExecutionError("adapter completion identity is invalid")
    outputs = session_input.get("outputs")
    if not isinstance(outputs, Mapping):
        raise InstalledExecutionError("session input outputs are invalid")
    normalized_binding, actor_binding = value.get("normalized"), value.get("actor_evidence")
    if (
        not isinstance(normalized_binding, Mapping)
        or not isinstance(actor_binding, Mapping)
        or normalized_binding.get("path") != outputs.get("normalized")
        or actor_binding.get("path") != outputs.get("actor_evidence")
    ):
        raise InstalledExecutionError("adapter completion output path is invalid")
    normalized = read_bound_file(normalized_binding, label="normalized evidence", maximum=8_388_608, deadline=deadline)
    actors = read_bound_file(actor_binding, label="actor evidence", maximum=8_388_608, deadline=deadline)
    return normalized, actors


def _validate_ha_initial_witness(admitted, session_input: Mapping[str, Any],
                                 observation: Mapping[str, Any]) -> None:
    """Require the HA initial barrier to name the root-owned observation.

    The adapter may retain richer scenario facts in its witness, but it cannot
    choose the observation anchor.  The controller proof and sequence below
    are always supplied by the runner's initial verify.
    """
    try:
        witness = strict_json(admitted.evidence_bytes)
    except Exception as error:
        raise InstalledExecutionError("HA initial observation witness is not strict JSON") from error
    if not isinstance(witness, dict):
        raise InstalledExecutionError("HA initial observation witness is not an object")
    if witness.get("session_id", session_input["session_id"]) != session_input["session_id"]:
        raise InstalledExecutionError("HA initial observation witness has a foreign session")
    claimed = witness.get("observation")
    if claimed is not None and claimed != dict(observation):
        raise InstalledExecutionError("HA initial observation witness is not bound to the initial proof")


def _closed_session_evidence(evidence: Any) -> bool:
    final_stopped = getattr(evidence, "final_stopped", None)
    return (
        getattr(evidence, "state", None) == "closed"
        and not getattr(evidence, "cleanup_errors", ("missing",))
        and isinstance(final_stopped, dict)
        and final_stopped.get("status") == "stopped"
        and isinstance(final_stopped.get("service"), dict)
        and final_stopped["service"].get("state") == "stopped"
    )


def _cleanup_process(owner, process):
    cleanup_errors = []
    try:
        cleanup_deadline = Deadline(DEFAULT_CLEANUP_TIMEOUT_MS / 1000)
        if owner is not None:
            owner.cleanup(cleanup_deadline)
        elif process is not None:
            _terminate(process, cleanup_deadline)
    except BaseException as error:
        cleanup_errors.append(error)
    return cleanup_errors


def supervise_completion(
    session: Any, process: subprocess.Popen, session_input_path: Path | str,
    session_input: Mapping[str, Any], *, initial_observation: Mapping[str, Any],
    admit: Callable[..., None], owner=None, cell_deadline=None,
) -> InstalledExecutionResult:
    """Run the close-before-ack protocol under one enclosing deadline."""
    supervision_started = time.monotonic()
    deadline = _deadline_from_value(cell_deadline)
    session_evidence = None
    failure = None
    ready = None
    ready_binding = None
    ack = None
    owner_acquired = owner is not None
    cleanup_timeout_ms = DEFAULT_CLEANUP_TIMEOUT_MS
    ack_path = None
    pending_ack_path = None
    pending_ready_binding = None
    pending_ready = None
    pending_sequence = 1
    admission_result = None
    try:
        # Direct callers enter here without execute_installed; acquire before
        # the first input read so early admission failures still own cleanup.
        if owner is None:
            owner = OwnedProcess(process, deadline)
            owner_acquired = True
        input_path = Path(session_input_path)
        input_raw = read_bound_file(
            file_binding(input_path, maximum=1_048_576, deadline=deadline),
            label="session input", maximum=1_048_576, deadline=deadline,
        )
        if strict_json(input_raw) != session_input:
            raise InstalledExecutionError("session input bytes differ from the staged contract")
        session_hash = sha256_bytes(input_raw)
        outputs, bounds = session_input.get("outputs"), session_input.get("bounds")
        if not isinstance(outputs, Mapping) or not isinstance(bounds, Mapping):
            raise InstalledExecutionError("session input lacks completion contract")
        declared_timeout_ms, cleanup_timeout_ms = _validate_bounds(session_input, None)
        if cell_deadline is None:
            _clamp_to_declared_timeout(deadline, supervision_started, declared_timeout_ms)
        else:
            deadline.remaining()
        coordination_value = outputs.get("coordination_dir")
        if not isinstance(coordination_value, str):
            raise InstalledExecutionError("session input coordination directory is invalid")
        coordination = Path(coordination_value)
        # Home Assistant has an additional runner-owned initial barrier.  It
        # keeps the adapter attached while the runner performs the one private
        # advance-once, then uses a distinct final Ready/Ack sequence.  Other
        # adapters retain the original single evidence-ready barrier.
        is_home_assistant = session_input.get("adapter_id") == "home_assistant"
        initial_observation = {
            "session_sequence": initial_observation["session_sequence"],
            "proof_sha256": initial_observation["proof_sha256"],
        }
        if is_home_assistant:
            initial_ready_path = coordination / "ready-000001.json"
            initial_ack_path = coordination / "ack-000001.json"
            pending_sequence = 1
            pending_ready = _wait_for_ready(initial_ready_path, process, deadline)
            pending_ready_binding = file_binding(initial_ready_path, maximum=65_536, deadline=deadline)
            pending_ack_path = initial_ack_path
            initial_admitted = validate_ready(
                pending_ready, session_id=session_input["session_id"], cell_id=session_input["cell_id"],
                session_input_sha256=session_hash, instance_nonce=session_input["instance_nonce"],
                sequence=1, allowed_phases={"ha_initial_observation_ready"},
                observation=initial_observation, deadline=deadline,
            )
            _validate_ha_initial_witness(initial_admitted, session_input, initial_observation)
            advance_result = session.advance_once_for_ha(deadline=deadline)
            if not isinstance(advance_result, dict) or not isinstance(advance_result.get("proof"), dict):
                raise InstalledExecutionError("HA initial advance result is invalid")
            advance_proof = advance_result["proof"]
            advance_sequence = advance_proof.get("sequence")
            if type(advance_sequence) is not int or advance_sequence <= initial_observation["session_sequence"]:
                raise InstalledExecutionError("HA initial advance did not advance the root observation")
            advance_value = {
                "schema_version": 1,
                "session_id": session_input["session_id"],
                "cell_id": session_input["cell_id"],
                "operation": "advance-once",
                "from_observation": initial_observation,
                "observation": {"session_sequence": advance_sequence, "proof_sha256": proof_sha256(advance_proof)},
                "advance": advance_result.get("advance"),
            }
            advance_binding = write_exclusive_json(coordination / "ha-advance-evidence.json", advance_value)
            write_ack(
                initial_ack_path, session_id=session_input["session_id"], cell_id=session_input["cell_id"],
                session_input_sha256=session_hash, instance_nonce=session_input["instance_nonce"],
                sequence=1, ready_binding=pending_ready_binding,
                phase="ha_initial_observation_ready", status="accepted", action="advance_once",
                result=advance_binding, deadline=deadline,
            )
            pending_ack_path = None
            pending_ready_binding = None
            pending_ready = None
            pending_sequence = 2
        ready_path = coordination / f"ready-{pending_sequence:06d}.json"
        ack_path = coordination / f"ack-{pending_sequence:06d}.json"
        ready = _wait_for_ready(ready_path, process, deadline)
        ready_binding = file_binding(ready_path, maximum=65_536, deadline=deadline)
        pending_ack_path = ack_path
        pending_ready_binding = ready_binding
        pending_ready = ready
        latest = session.latest_observation(deadline)
        if (
            not isinstance(latest, dict)
            or type(latest.get("sequence")) is not int
            or latest["sequence"] <= initial_observation.get("session_sequence", 0)
        ):
            raise InstalledExecutionError("adapter did not advance beyond its initial installed observation")
        final_observation = {
            "session_sequence": latest["sequence"],
            "proof_sha256": proof_sha256(latest),
        }
        admitted = validate_ready(
            ready, session_id=session_input["session_id"], cell_id=session_input["cell_id"],
            session_input_sha256=session_hash, instance_nonce=session_input["instance_nonce"],
            sequence=pending_sequence, allowed_phases={"evidence_ready"}, observation=final_observation,
            deadline=deadline,
        )
        completion = strict_json(admitted.evidence_bytes)
        normalized_raw, actors_raw = _validate_completion(completion, session_input, session_hash, deadline)
        normalized, actors = strict_json(normalized_raw), strict_json(actors_raw)
        if not isinstance(normalized, dict) or not isinstance(actors, dict):
            raise InstalledExecutionError("adapter evidence is not an object")
        if _accepts_keyword(admit, "admission_views", include_var_kwargs=False):
            admission_views = {
                "controller_observations": build_controller_admission_view(
                    session, session_input, deadline=deadline,
                ),
            }
            admission_result = _call_with_deadline(
                admit, normalized, actors, admission_views=admission_views,
                deadline=deadline,
            )
        else:
            # Legacy callbacks remain source-compatible, but reviewed fixed
            # registry entries must accept the runner-owned view keyword.
            admission_result = _call_with_deadline(admit, normalized, actors, deadline=deadline)
        deadline.remaining()
        if process.poll() is not None:
            raise InstalledExecutionError("adapter exited before runner close")
        session_evidence = session.close(deadline=deadline)
        deadline.remaining()
        if not _closed_session_evidence(session_evidence):
            raise InstalledExecutionError("installed session lacks clean independent stopped proof")
        close_binding = write_exclusive_json(coordination / "close-evidence.json", {
            "schema_version": 1, "session_id": session_input["session_id"],
            "state": session_evidence.state,
            "journal": {"path": session_evidence.journal_path, "sha256": session_evidence.journal_sha256},
            "final_stopped": session_evidence.final_stopped,
            "cleanup_errors": list(session_evidence.cleanup_errors),
            "local_transport": session_evidence.local_transport,
        })
        deadline.remaining()
        ack = write_ack(
            ack_path, session_id=session_input["session_id"], cell_id=session_input["cell_id"],
            session_input_sha256=session_hash, instance_nonce=session_input["instance_nonce"],
            sequence=pending_sequence, ready_binding=ready_binding, phase="evidence_ready", status="accepted",
            action="close_completed", result=close_binding, deadline=deadline,
        )
        pending_ack_path = None
        pending_ready_binding = None
        pending_ready = None
        deadline.remaining()
        # The final output read is before the process-generation absence check;
        # accepted completion still fails if any output mutates afterward.
        if (
            read_bound_file(completion["normalized"], label="normalized evidence", maximum=8_388_608, deadline=deadline) != normalized_raw
            or read_bound_file(completion["actor_evidence"], label="actor evidence", maximum=8_388_608, deadline=deadline) != actors_raw
        ):
            raise InstalledExecutionError("adapter evidence changed after readiness")
        owner.require_absent(deadline)
        exit_code = process.returncode
        if exit_code != 0:
            raise InstalledExecutionError("adapter exited nonzero after completion acknowledgement")
        return InstalledExecutionResult(exit_code, session_evidence, ready, ack, completion, admission_result)
    except BaseException as error:
        if isinstance(error, InstalledExecutionError):
            failure = error
        else:
            failure = InstalledExecutionError(str(error) or type(error).__name__)
    finally:
        if failure is not None:
            # A rejection is emitted only when no accepted acknowledgement was
            # durably written.  It gives a waiting adapter a bounded exit path.
            if pending_ready_binding is not None and pending_ack_path is not None and not pending_ack_path.exists() and isinstance(pending_ready, dict):
                try:
                    write_ack(
                        pending_ack_path, session_id=session_input["session_id"], cell_id=session_input["cell_id"],
                        session_input_sha256=locals().get("session_hash", ""),
                        instance_nonce=session_input.get("instance_nonce", ""), sequence=pending_sequence,
                        ready_binding=pending_ready_binding, phase=pending_ready.get("phase", "evidence_ready"),
                        status="rejected", action="abort", result=None,
                        deadline=Deadline(cleanup_timeout_ms / 1000),
                    )
                except BaseException:
                    pass
            cleanup_errors = []
            if session_evidence is None and session is not None:
                try:
                    session_evidence = session.close(deadline=Deadline(cleanup_timeout_ms / 1000))
                except BaseException as cleanup_error:
                    cleanup_errors.append(cleanup_error)
            if owner_acquired or process is not None:
                cleanup_errors.extend(_cleanup_process(owner, process))
            if cleanup_errors:
                failure = InstalledExecutionError(str(failure), cleanup_errors=cleanup_errors)
        if failure is not None:
            raise failure
