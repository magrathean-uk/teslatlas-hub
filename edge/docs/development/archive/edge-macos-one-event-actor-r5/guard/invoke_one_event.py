#!/usr/bin/env python3
"""Fail-closed one-shot wrapper for the future Edge actor.

The wrapper owns no runtime or credential setup. It validates source-bound
inputs, rejects every pre-existing or aliased output path before reading the
clock, claims one exclusive guard, performs one child invocation, and writes
one final result without retry or replay.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence


EXPECTED_SCHEMA_VERSION = 1
EXPECTED_PAYLOAD_KEYS = {
    "topic",
    "transaction_type",
    "field",
    "value",
    "value_type",
    "timestamp_source",
}
EXPECTED_AUTH_KEYS = {
    "schema_version",
    "event_authorized",
    "cohort_id",
    "binding_sha256",
    "helper_sha256",
    "transaction_id",
    "payload_sha256",
    "endpoint",
    "certificate_sha256",
    "key_sha256",
    "server_ca_sha256",
    "invocation_count",
    "retry_or_replay",
    "timeout_seconds",
    "expires_at_ms",
}
HEX_DIGEST = re.compile(r"\A[0-9a-f]{64}\Z")


class ContractError(RuntimeError):
    """Raised when a source-bound one-shot contract fails closed."""


class AlreadyClaimed(ContractError):
    """Raised when the exclusive guard already exists."""


def _duplicate_key_rejector(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _read_json(path: Path, label: str, expected_mode: int) -> dict[str, Any]:
    _check_private_regular(path, label, expected_mode=expected_mode)
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=_duplicate_key_rejector
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ContractError(f"cannot read {label}: {exc}") from exc
    if not isinstance(value, dict):
        raise ContractError(f"{label} must be a JSON object")
    return value


def _check_private_regular(
    path: Path, label: str, expected_mode: Optional[int] = None, executable: bool = False
) -> os.stat_result:
    if not path.is_absolute():
        raise ContractError(f"{label} must be an absolute path")
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise ContractError(f"cannot stat {label}: {exc}") from exc
    if stat.S_ISLNK(metadata.st_mode):
        raise ContractError(f"{label} must not be a symlink")
    if not stat.S_ISREG(metadata.st_mode):
        raise ContractError(f"{label} must be a regular file")
    if metadata.st_nlink != 1:
        raise ContractError(f"{label} must have one hard link")
    if metadata.st_uid != os.getuid():
        raise ContractError(f"{label} is not owned by the invoking user")
    actual_mode = stat.S_IMODE(metadata.st_mode)
    if actual_mode & 0o077:
        raise ContractError(f"{label} must not be group/world accessible")
    if expected_mode is not None and actual_mode != expected_mode:
        raise ContractError(
            f"{label} mode {actual_mode:04o} does not match {expected_mode:04o}"
        )
    if executable and not actual_mode & 0o100:
        raise ContractError(f"{label} must be owner-executable")
    return metadata


def _check_private_parent(path: Path, label: str) -> None:
    parent = path.parent
    if not parent.is_absolute():
        raise ContractError(f"{label} parent must be absolute")
    try:
        metadata = parent.lstat()
    except OSError as exc:
        raise ContractError(f"cannot stat {label} parent: {exc}") from exc
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise ContractError(f"{label} parent must be a non-symlink directory")
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
        raise ContractError(f"{label} parent must be owner-only")


def _parse_mode(value: Any, label: str) -> int:
    if not isinstance(value, str) or not re.fullmatch(r"0[0-7]{3}", value):
        raise ContractError(f"{label} mode must be an octal string")
    return int(value, 8)


def _digest(path: Path, label: str) -> str:
    _check_private_regular(path, label)
    hasher = hashlib.sha256()
    try:
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                hasher.update(chunk)
    except OSError as exc:
        raise ContractError(f"cannot hash {label}: {exc}") from exc
    return hasher.hexdigest()


def _require_digest(value: Any, label: str) -> str:
    if not isinstance(value, str) or not HEX_DIGEST.fullmatch(value):
        raise ContractError(f"{label} must be a lowercase SHA-256 digest")
    return value


def _require_path_reference(
    contract: Mapping[str, Any], section: str, supplied: Path, executable: bool = False
) -> tuple[dict[str, Any], str]:
    reference = contract.get(section)
    if not isinstance(reference, dict):
        raise ContractError(f"contract.{section} must be an object")
    expected_path = reference.get("path")
    if not isinstance(expected_path, str) or expected_path != str(supplied):
        raise ContractError(f"{section} path does not match the contract")
    expected_mode = _parse_mode(reference.get("mode"), f"contract.{section}")
    _check_private_regular(supplied, section, expected_mode=expected_mode, executable=executable)
    expected_hash = _require_digest(reference.get("sha256"), f"contract.{section}")
    actual_hash = _digest(supplied, section)
    if actual_hash != expected_hash:
        raise ContractError(f"{section} hash does not match the contract")
    return reference, actual_hash


def _canonical_json(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def _validate_authorization(
    contract: Mapping[str, Any], authorization: Mapping[str, Any]
) -> dict[str, Any]:
    expected = contract.get("authorization")
    if not isinstance(expected, dict) or dict(authorization) != expected:
        raise ContractError("authorization is not the exact contract object")
    if set(authorization) != EXPECTED_AUTH_KEYS:
        raise ContractError("authorization fields are not exact")
    if authorization.get("schema_version") != EXPECTED_SCHEMA_VERSION:
        raise ContractError("unsupported authorization schema")
    if authorization.get("event_authorized") is not True:
        raise ContractError("event is not explicitly authorized")
    if authorization.get("invocation_count") != 1:
        raise ContractError("invocation_count must be one")
    if authorization.get("retry_or_replay") is not False:
        raise ContractError("retry or replay is forbidden")
    transaction_id = authorization.get("transaction_id")
    if not isinstance(transaction_id, str) or not transaction_id:
        raise ContractError("transaction_id must be a non-empty string")
    endpoint = authorization.get("endpoint")
    if not isinstance(endpoint, str) or not endpoint.startswith("wss://"):
        raise ContractError("authorization endpoint must use wss")
    payload_hash = _require_digest(authorization.get("payload_sha256"), "payload_sha256")
    _require_digest(authorization.get("binding_sha256"), "binding_sha256")
    _require_digest(authorization.get("helper_sha256"), "helper_sha256")
    for field in ("certificate_sha256", "key_sha256", "server_ca_sha256"):
        _require_digest(authorization.get(field), field)
    timeout = authorization.get("timeout_seconds")
    if isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or not 0 < timeout <= 30:
        raise ContractError("timeout_seconds must be in (0, 30]")
    expiry = authorization.get("expires_at_ms")
    if isinstance(expiry, bool) or not isinstance(expiry, int) or expiry <= 0:
        raise ContractError("expires_at_ms must be a positive integer")
    return {
        "transaction_id": transaction_id,
        "endpoint": endpoint,
        "payload_sha256": payload_hash,
        "timeout_seconds": timeout,
        "expires_at_ms": expiry,
    }


def _validate_binding(
    binding: Mapping[str, Any], authorization: Mapping[str, Any], binding_hash: str
) -> dict[str, Any]:
    if binding.get("schema_version") != EXPECTED_SCHEMA_VERSION:
        raise ContractError("unsupported binding schema")
    if binding.get("event_authorized") is not False:
        raise ContractError("binding must not authorize its own event")
    if authorization.get("binding_sha256") != binding_hash:
        raise ContractError("authorization binding hash mismatch")
    vin = binding.get("vin")
    if not isinstance(vin, str) or not vin:
        raise ContractError("binding VIN must be a sealed non-empty string")
    endpoint = binding.get("endpoint")
    transaction_id = binding.get("transaction_id")
    if endpoint != authorization.get("endpoint"):
        raise ContractError("binding endpoint mismatch")
    if transaction_id != authorization.get("transaction_id"):
        raise ContractError("binding transaction mismatch")
    if binding.get("transaction_type") != "V":
        raise ContractError("binding transaction type must be V")
    payload = binding.get("payload")
    if not isinstance(payload, dict) or set(payload) != EXPECTED_PAYLOAD_KEYS:
        raise ContractError("binding payload shape is not exact")
    if (
        payload.get("topic") != "V"
        or payload.get("transaction_type") != "V"
        or payload.get("field") != "VehicleName"
        or payload.get("value_type") != "string"
        or not isinstance(payload.get("value"), str)
        or payload.get("timestamp_source") != "single-wrapper-created-at-ms"
    ):
        raise ContractError("binding payload is not the VehicleName-only contract")
    if hashlib.sha256(_canonical_json(payload)).hexdigest() != authorization.get("payload_sha256"):
        raise ContractError("authorization payload hash mismatch")
    return {
        "vin": vin,
        "endpoint": endpoint,
        "transaction_id": transaction_id,
        "payload": payload,
    }


def _validate_tls_inputs(
    contract: Mapping[str, Any],
    authorization: Mapping[str, Any],
    certificate_path: Path,
    key_path: Path,
    server_ca_path: Path,
) -> dict[str, str]:
    tls_inputs = contract.get("tls_inputs")
    if not isinstance(tls_inputs, dict):
        raise ContractError("contract.tls_inputs must be an object")
    hashes: dict[str, str] = {}
    for field, supplied, auth_field in (
        ("certificate", certificate_path, "certificate_sha256"),
        ("key", key_path, "key_sha256"),
        ("server_ca", server_ca_path, "server_ca_sha256"),
    ):
        reference = tls_inputs.get(field)
        if not isinstance(reference, dict):
            raise ContractError(f"contract.tls_inputs.{field} must be an object")
        if reference.get("path") != str(supplied):
            raise ContractError(f"{field} path does not match the contract")
        expected_mode = _parse_mode(reference.get("mode"), f"contract.tls_inputs.{field}")
        _check_private_regular(supplied, field, expected_mode=expected_mode)
        expected_hash = _require_digest(reference.get("sha256"), f"{field} sha256")
        actual_hash = _digest(supplied, field)
        if actual_hash != expected_hash or actual_hash != authorization.get(auth_field):
            raise ContractError(f"{field} hash does not match the authorization")
        hashes[field] = actual_hash
    if len({str(certificate_path), str(key_path), str(server_ca_path)}) != 3:
        raise ContractError("TLS input paths must be distinct")
    return hashes


def _normal_path(path: Path) -> str:
    return os.path.normcase(os.path.abspath(os.path.normpath(str(path))))


def _reject_existing_output(path: Path, label: str) -> None:
    try:
        path.lstat()
    except FileNotFoundError:
        return
    except OSError as exc:
        raise ContractError(f"cannot stat {label}: {exc}") from exc
    if label == "guard":
        _check_private_regular(path, label, expected_mode=0o600)
        raise AlreadyClaimed("exclusive producer guard already exists")
    if label == "result_temp":
        raise ContractError("result temporary file already exists")
    raise ContractError(f"{label} already exists")


def _validate_output_paths(
    contract: Mapping[str, Any],
    guard_path: Path,
    result_path: Path,
    output_path: Path,
    input_paths: Sequence[Path],
) -> Path:
    output_reference = contract.get("outputs")
    if not isinstance(output_reference, dict):
        raise ContractError("contract.outputs must be an object")
    outputs = {
        "guard": guard_path,
        "result": result_path,
        "output": output_path,
    }
    for field, supplied in outputs.items():
        reference = output_reference.get(field)
        if not isinstance(reference, dict) or reference.get("path") != str(supplied):
            raise ContractError(f"output path does not match contract.{field}")
        if _parse_mode(reference.get("mode"), f"contract.outputs.{field}") != 0o600:
            raise ContractError(f"contract.outputs.{field} must request mode 0600")
        _check_private_parent(supplied, field)

    result_temp = result_path.parent / f".{result_path.name}.tmp"
    output_paths = {**outputs, "result_temp": result_temp}
    normalized_outputs = [_normal_path(path) for path in output_paths.values()]
    if len(set(normalized_outputs)) != len(normalized_outputs):
        raise ContractError("output paths must be pairwise distinct")
    normalized_inputs = {_normal_path(path) for path in input_paths}
    for path in output_paths.values():
        if _normal_path(path) in normalized_inputs:
            raise ContractError("output path aliases an input")
    _check_private_parent(result_temp, "result_temp")

    # This entire absent-path check occurs before the sole clock read. It also
    # rejects dangling symlinks and other non-regular occupants via lstat.
    for label, path in output_paths.items():
        _reject_existing_output(path, label)
    return result_temp


def _claim_exclusive_json(path: Path, label: str, value: Mapping[str, Any]) -> None:
    _check_private_parent(path, label)
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as exc:
        if label == "guard":
            raise AlreadyClaimed("exclusive producer guard already exists") from exc
        raise ContractError(f"{label} already exists") from exc
    except OSError as exc:
        raise ContractError(f"cannot create {label}: {exc}") from exc
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(value, stream, sort_keys=True, separators=(",", ":"))
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
    except OSError as exc:
        raise ContractError(f"cannot persist {label}: {exc}") from exc
    _check_private_regular(path, label, expected_mode=0o600)


def _open_exclusive_output(path: Path) -> Any:
    _check_private_parent(path, "output")
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as exc:
        raise ContractError("output already exists") from exc
    except OSError as exc:
        raise ContractError(f"cannot create output: {exc}") from exc
    return os.fdopen(descriptor, "w", encoding="utf-8")


def _write_atomic_result(path: Path, temporary: Path, value: Mapping[str, Any]) -> None:
    _check_private_parent(path, "result")
    try:
        descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as exc:
        raise ContractError("result temporary file already exists") from exc
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(value, stream, sort_keys=True, separators=(",", ":"))
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.link(temporary, path, follow_symlinks=False)
        except FileExistsError as exc:
            raise ContractError("result already exists") from exc
    except OSError as exc:
        raise ContractError(f"cannot persist result: {exc}") from exc
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
        except OSError as exc:
            raise ContractError(f"cannot remove result temporary file: {exc}") from exc
    _check_private_regular(path, "result", expected_mode=0o600)


def _redacted_argv(
    helper_hash: str,
    endpoint: str,
    transaction_id: str,
    created_at_ms: int,
) -> list[str]:
    return [
        f"helper-sha256:{helper_hash}",
        "-endpoint",
        endpoint,
        "-cert",
        "<certificate-path>",
        "-key",
        "<key-path>",
        "-server-ca",
        "<server-ca-path>",
        "-vin",
        "<sealed-vin>",
        "-txid",
        transaction_id,
        "-created-at-ms",
        str(created_at_ms),
    ]


def run_once(
    *,
    contract_path: Path,
    authorization_path: Path,
    binding_path: Path,
    helper_path: Path,
    certificate_path: Path,
    key_path: Path,
    server_ca_path: Path,
    guard_path: Path,
    result_path: Path,
    output_path: Path,
    clock_ms: Optional[Callable[[], int]] = None,
) -> dict[str, Any]:
    contract = _read_json(contract_path, "contract", expected_mode=0o600)
    if contract.get("schema_version") != EXPECTED_SCHEMA_VERSION:
        raise ContractError("unsupported contract schema")

    authorization = _read_json(authorization_path, "authorization", expected_mode=0o600)
    authorization_context = _validate_authorization(contract, authorization)
    _, binding_hash = _require_path_reference(contract, "binding", binding_path)
    binding = _read_json(binding_path, "binding", expected_mode=0o600)
    binding_context = _validate_binding(binding, authorization, binding_hash)

    helper_reference, helper_hash = _require_path_reference(
        contract, "helper", helper_path, executable=True
    )
    marker = helper_reference.get("identity_marker")
    if not isinstance(marker, str) or marker.encode() not in helper_path.read_bytes():
        raise ContractError("helper identity marker is absent")
    forbidden_literals = helper_reference.get("forbidden_literals", [])
    if not isinstance(forbidden_literals, list) or any(
        not isinstance(literal, str) for literal in forbidden_literals
    ):
        raise ContractError("helper forbidden_literals must be strings")
    helper_bytes = helper_path.read_bytes()
    if any(literal.encode() in helper_bytes for literal in forbidden_literals):
        raise ContractError("helper contains a forbidden historical literal")
    if helper_hash != authorization.get("helper_sha256"):
        raise ContractError("authorization helper hash mismatch")

    tls_hashes = _validate_tls_inputs(
        contract, authorization, certificate_path, key_path, server_ca_path
    )
    result_temp = _validate_output_paths(
        contract,
        guard_path,
        result_path,
        output_path,
        input_paths=(
            contract_path,
            authorization_path,
            binding_path,
            helper_path,
            certificate_path,
            key_path,
            server_ca_path,
        ),
    )

    if clock_ms is None:
        clock_ms = lambda: time.time_ns() // 1_000_000
    created_at_ms = clock_ms()
    if isinstance(created_at_ms, bool) or not isinstance(created_at_ms, int) or created_at_ms <= 0:
        raise ContractError("clock must return a positive integer Unix millisecond")
    if created_at_ms > authorization_context["expires_at_ms"]:
        raise ContractError("authorization has expired")

    guard_record = {
        "schema_version": EXPECTED_SCHEMA_VERSION,
        "state": "claimed",
        "binding_sha256": authorization["binding_sha256"],
        "helper_sha256": helper_hash,
        "certificate_sha256": tls_hashes["certificate"],
        "server_ca_sha256": tls_hashes["server_ca"],
        "transaction_id": binding_context["transaction_id"],
        "payload_sha256": authorization["payload_sha256"],
        "created_at_ms": created_at_ms,
        "invocation_count": 1,
        "retry_or_replay": False,
        "argv_shape": _redacted_argv(
            helper_hash,
            binding_context["endpoint"],
            binding_context["transaction_id"],
            created_at_ms,
        ),
    }
    _claim_exclusive_json(guard_path, "guard", guard_record)

    argv = [
        str(helper_path),
        "-endpoint",
        binding_context["endpoint"],
        "-cert",
        str(certificate_path),
        "-key",
        str(key_path),
        "-server-ca",
        str(server_ca_path),
        "-vin",
        binding_context["vin"],
        "-txid",
        binding_context["transaction_id"],
        "-created-at-ms",
        str(created_at_ms),
    ]

    exit_code: Optional[int] = None
    error: Optional[str] = None
    state = "failed_no_retry"
    output = _open_exclusive_output(output_path)
    try:
        try:
            completed = subprocess.run(
                argv,
                check=False,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
                shell=False,
                timeout=authorization_context["timeout_seconds"],
            )
            exit_code = completed.returncode
            state = "completed" if exit_code == 0 else "failed_no_retry"
        except subprocess.TimeoutExpired as exc:
            state = "timed_out_no_retry"
            error = f"timeout after {authorization_context['timeout_seconds']} seconds"
            if exc.output:
                output.write("\n[wrapper timeout]\n")
        except OSError as exc:
            state = "failed_no_retry"
            error = f"spawn failed: {exc}"
    finally:
        output.flush()
        os.fsync(output.fileno())
        output.close()
    result_record: dict[str, Any] = {
        "schema_version": EXPECTED_SCHEMA_VERSION,
        "state": state,
        "exit_code": exit_code,
        "error": error,
        "binding_sha256": authorization["binding_sha256"],
        "helper_sha256": helper_hash,
        "transaction_id": binding_context["transaction_id"],
        "payload_sha256": authorization["payload_sha256"],
        "created_at_ms": created_at_ms,
        "invocation_count": 1,
        "retry_or_replay": False,
        "timeout_seconds": authorization_context["timeout_seconds"],
    }
    _write_atomic_result(result_path, result_temp, result_record)
    return result_record


def _parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in (
        "contract",
        "authorization",
        "binding",
        "helper",
        "certificate",
        "key",
        "server-ca",
        "guard",
        "result",
        "output",
    ):
        parser.add_argument(f"--{name}", required=True, type=Path)
    return parser.parse_args(argv)


def main(argv: Optional[Sequence[str]] = None) -> int:
    arguments = _parse_args(argv)
    try:
        result = run_once(
            contract_path=arguments.contract,
            authorization_path=arguments.authorization,
            binding_path=arguments.binding,
            helper_path=arguments.helper,
            certificate_path=arguments.certificate,
            key_path=arguments.key,
            server_ca_path=arguments.server_ca,
            guard_path=arguments.guard,
            result_path=arguments.result,
            output_path=arguments.output,
        )
    except AlreadyClaimed as exc:
        print(f"final one-shot rejection: {exc}", file=sys.stderr)
        return 2
    except ContractError as exc:
        print(f"contract rejection: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0 if result["state"] == "completed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
