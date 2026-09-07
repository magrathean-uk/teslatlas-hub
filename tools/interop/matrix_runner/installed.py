# SPDX-License-Identifier: AGPL-3.0-only
"""Runner-owned installed completion and cleanup ordering."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time
from typing import Any, Callable, Mapping

from .adapter_wire import WireError, file_binding, read_bound_file, sha256_bytes, strict_json, validate_ready, write_ack, write_exclusive_json
from ..installed_hosts.bounded import Deadline


class InstalledExecutionError(RuntimeError):
    pass


@dataclass(frozen=True)
class InstalledExecutionResult:
    exit_code: int
    session_evidence: Any
    ready: Mapping[str, Any]
    ack: Mapping[str, Any]
    completion: Mapping[str, Any]


def canonical_json_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def proof_sha256(value: Mapping[str, Any]) -> str:
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def execute_installed(
    session_config: Mapping[str, Any], registration_inventory: Mapping[str, Any], *,
    build_session_input: Callable[[Mapping[str, Any], Mapping[str, Any]], tuple[Path | str, Mapping[str, Any]]],
    launch_adapter: Callable[[Path | str, Mapping[str, Any]], subprocess.Popen],
    admit: Callable[[Mapping[str, Any], Mapping[str, Any]], None],
    session_factory: Any = None,
) -> InstalledExecutionResult:
    """Open and verify the installed target before exposing a broker attachment."""
    if session_factory is None:
        from ..installed_hosts.session import InstalledSession
        session_factory = InstalledSession
    session = session_factory.open(session_config, registration_inventory)
    process = None
    handed_to_supervisor = False
    try:
        running = session.request({"op": "verify"})
        if not isinstance(running, dict) or not isinstance(running.get("proof"), dict):
            raise InstalledExecutionError("initial installed verification is invalid")
        proof = running["proof"]
        sequence = proof.get("sequence")
        if type(sequence) is not int or sequence <= 0:
            raise InstalledExecutionError("initial installed proof sequence is invalid")
        session_input_path, session_input = build_session_input(session.descriptor, running)
        if not isinstance(session_input, dict) or session_input.get("host_session") != session.descriptor:
            raise InstalledExecutionError("session input host descriptor is stale")
        process = launch_adapter(session_input_path, session_input)
        if not all(callable(getattr(process, name, None)) for name in ("poll", "wait", "terminate", "kill")):
            raise InstalledExecutionError("adapter launcher returned no supervised process")
        handed_to_supervisor = True
        return supervise_completion(
            session, process, session_input_path, session_input,
            initial_observation={"session_sequence": sequence, "proof_sha256": proof_sha256(proof)},
            admit=admit,
        )
    finally:
        if not handed_to_supervisor:
            if process is not None and process.poll() is None:
                _terminate(process)
            try:
                session.close()
            except Exception:
                pass


def _terminate(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        process.wait()
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        process.terminate()
    try:
        process.wait(timeout=0.75)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            process.kill()
        process.wait(timeout=1)


def _wait_for_ready(path: Path, process: subprocess.Popen, deadline: float) -> dict[str, Any]:
    while True:
        if path.exists():
            value = strict_json(read_bound_file(file_binding(path), label="ready record", maximum=65_536))
            if not isinstance(value, dict):
                raise InstalledExecutionError("ready record is not an object")
            return value
        if process.poll() is not None:
            raise InstalledExecutionError("adapter exited before evidence readiness")
        if time.monotonic() >= deadline:
            raise InstalledExecutionError("adapter evidence readiness timed out")
        time.sleep(0.01)


def _validate_completion(value: Any, session_input: Mapping[str, Any], session_hash: str) -> tuple[bytes, bytes]:
    fields = {"schema_version", "session_id", "cell_id", "session_input_sha256", "normalized", "actor_evidence"}
    if not isinstance(value, dict) or set(value) != fields:
        raise InstalledExecutionError("adapter completion shape is invalid")
    if value["schema_version"] != 1 or value["session_id"] != session_input["session_id"] or value["cell_id"] != session_input["cell_id"] or value["session_input_sha256"] != session_hash:
        raise InstalledExecutionError("adapter completion identity is invalid")
    outputs = session_input["outputs"]
    if value["normalized"].get("path") != outputs["normalized"] or value["actor_evidence"].get("path") != outputs["actor_evidence"]:
        raise InstalledExecutionError("adapter completion output path is invalid")
    normalized = read_bound_file(value["normalized"], label="normalized evidence", maximum=8_388_608)
    actors = read_bound_file(value["actor_evidence"], label="actor evidence", maximum=8_388_608)
    return normalized, actors


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


def supervise_completion(
    session: Any, process: subprocess.Popen, session_input_path: Path | str,
    session_input: Mapping[str, Any], *, initial_observation: Mapping[str, Any],
    admit: Callable[[Mapping[str, Any], Mapping[str, Any]], None],
) -> InstalledExecutionResult:
    input_path = Path(session_input_path)
    input_raw = read_bound_file(file_binding(input_path), label="session input", maximum=1_048_576)
    session_hash = sha256_bytes(input_raw)
    outputs, bounds = session_input.get("outputs"), session_input.get("bounds")
    if not isinstance(outputs, dict) or not isinstance(bounds, dict):
        raise InstalledExecutionError("session input lacks completion contract")
    coordination = Path(outputs["coordination_dir"])
    ready_path, ack_path = coordination / "ready-000001.json", coordination / "ack-000001.json"
    session_evidence = None
    failure = None
    ready = None
    ready_binding = None
    try:
        deadline = time.monotonic() + bounds["cell_timeout_ms"] / 1000
        ready = _wait_for_ready(ready_path, process, deadline)
        ready_binding = file_binding(ready_path)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise InstalledExecutionError("adapter evidence readiness timed out")
        latest = session.latest_observation(Deadline(remaining))
        if not isinstance(latest, dict) or type(latest.get("sequence")) is not int or latest["sequence"] <= initial_observation["session_sequence"]:
            raise InstalledExecutionError("adapter did not advance beyond its initial installed observation")
        final_observation = {
            "session_sequence": latest["sequence"], "proof_sha256": proof_sha256(latest)
        }
        admitted = validate_ready(ready, session_id=session_input["session_id"], cell_id=session_input["cell_id"], session_input_sha256=session_hash, instance_nonce=session_input["instance_nonce"], sequence=1, allowed_phases={"evidence_ready"}, observation=final_observation)
        completion = strict_json(admitted.evidence_bytes)
        normalized_raw, actors_raw = _validate_completion(completion, session_input, session_hash)
        normalized, actors = strict_json(normalized_raw), strict_json(actors_raw)
        if not isinstance(normalized, dict) or not isinstance(actors, dict):
            raise InstalledExecutionError("adapter evidence is not an object")
        admit(normalized, actors)
        if process.poll() is not None:
            raise InstalledExecutionError("adapter exited before runner close")
        session_evidence = session.close()
        if not _closed_session_evidence(session_evidence):
            raise InstalledExecutionError("installed session lacks clean independent stopped proof")
        close_binding = write_exclusive_json(coordination / "close-evidence.json", {
            "schema_version": 1, "session_id": session_input["session_id"], "state": session_evidence.state,
            "journal": {"path": session_evidence.journal_path, "sha256": session_evidence.journal_sha256},
            "final_stopped": session_evidence.final_stopped,
            "cleanup_errors": list(session_evidence.cleanup_errors),
            "local_transport": session_evidence.local_transport,
        })
        ack = write_ack(ack_path, session_id=session_input["session_id"], cell_id=session_input["cell_id"], session_input_sha256=session_hash, instance_nonce=session_input["instance_nonce"], sequence=1, ready_binding=ready_binding, phase="evidence_ready", status="accepted", action="close_completed", result=close_binding)
        try:
            exit_code = process.wait(timeout=max(0.001, deadline - time.monotonic()))
        except subprocess.TimeoutExpired as error:
            raise InstalledExecutionError("adapter did not exit after completion acknowledgement") from error
        if exit_code != 0:
            raise InstalledExecutionError("adapter exited nonzero after completion acknowledgement")
        if read_bound_file(completion["normalized"], label="normalized evidence", maximum=8_388_608) != normalized_raw or read_bound_file(completion["actor_evidence"], label="actor evidence", maximum=8_388_608) != actors_raw:
            raise InstalledExecutionError("adapter evidence changed after readiness")
        return InstalledExecutionResult(exit_code, session_evidence, ready, ack, completion)
    except Exception as error:
        failure = error if isinstance(error, InstalledExecutionError) else InstalledExecutionError(str(error))
    finally:
        if failure is not None and ready_binding is not None and not ack_path.exists() and isinstance(ready, dict):
            try:
                write_ack(
                    ack_path, session_id=session_input["session_id"], cell_id=session_input["cell_id"],
                    session_input_sha256=session_hash, instance_nonce=session_input["instance_nonce"],
                    sequence=1, ready_binding=ready_binding, phase=ready.get("phase", "evidence_ready"),
                    status="rejected", action="abort", result=None,
                )
            except Exception:
                pass
        if session_evidence is None:
            try:
                session_evidence = session.close()
            except Exception:
                pass
        if process.poll() is None:
            _terminate(process)
    raise failure or InstalledExecutionError("installed execution failed")
