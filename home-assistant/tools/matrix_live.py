#!/usr/bin/env python3
"""Installed Home Assistant matrix coordinator over isolated broker stdio."""

from __future__ import annotations

import asyncio
import hashlib
import os
import platform
import secrets
import stat
import sys
import threading
import time
from contextlib import suppress
from pathlib import Path, PurePosixPath
from types import MappingProxyType
from urllib.parse import urlsplit

# The isolated adapter owns interpreter policy before pytest or Home Assistant is
# imported. Exact installed-member admission therefore remains stable during the
# whole process lifetime.
sys.dont_write_bytecode = True

try:
    from .matrix_contract import (
        CASE_BINDINGS,
        REQUIRED_CASES,
        AdmissionContext,
        AdmittedActor,
        AdmittedInvocation,
        admit_case,
    )
    from .matrix_wire import (
        CoordinationChannel,
        MatrixWireError,
        StdioBroker,
        canonical_json_bytes,
        load_session_input,
        read_staged_json,
        write_exclusive_json,
    )
except ImportError:
    from matrix_contract import (  # type: ignore[no-redef]
        CASE_BINDINGS,
        REQUIRED_CASES,
        AdmissionContext,
        AdmittedActor,
        AdmittedInvocation,
        admit_case,
    )
    from matrix_wire import (  # type: ignore[no-redef]
        CoordinationChannel,
        MatrixWireError,
        StdioBroker,
        canonical_json_bytes,
        load_session_input,
        read_staged_json,
        write_exclusive_json,
    )


HEADER_FIELDS = {
    "schema_version",
    "execution_kind",
    "adapter",
    "cell_id",
    "product_version",
    "profile_id",
    "profile_revision",
    "profile_sha256",
    "source_identities",
    "artifacts",
    "runtime",
}
SCHEMA_FOR_OPERATION = {
    "observe_installed_runtime": "ha-runtime-v1",
    "initial_poll": "ha-initial-v1",
    "probe": "ha-client-v1",
    "bad_pair": "ha-client-v1",
    "expired_pair": "ha-client-v1",
    "replay_pair": "ha-client-v1",
    "unknown_current": "ha-client-v1",
    "unsupported_surface": "ha-client-v1",
    "config_flow_setup": "ha-flow-v1",
    "later_poll": "ha-flow-v1",
    "revocation_refresh": "ha-flow-v1",
    "reauthentication_flow": "ha-flow-v1",
    "endpoint_restart_poll": "ha-flow-v1",
    "outage_poll": "ha-flow-v1",
    "unload": "ha-flow-v1",
}


class FrameworkLog:
    """Drain redirected framework output into one bounded, separate log."""

    def __init__(self, path: str, maximum: int) -> None:
        self.path = Path(path)
        self.maximum = maximum
        self.overflowed = False
        self._read_fd, write_fd = os.pipe()
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
        self._log_fd = os.open(self.path, flags, 0o600)
        null_fd = os.open("/dev/null", os.O_RDONLY)
        try:
            os.dup2(null_fd, 0)
            os.dup2(write_fd, 1)
            os.dup2(write_fd, 2)
        finally:
            os.close(null_fd)
            os.close(write_fd)
        self._thread = threading.Thread(target=self._drain, daemon=True)
        self._thread.start()

    def _drain(self) -> None:
        written = 0
        try:
            while True:
                chunk = os.read(self._read_fd, 65_536)
                if not chunk:
                    break
                available = max(0, self.maximum - written)
                if available:
                    piece = chunk[:available]
                    offset = 0
                    while offset < len(piece):
                        count = os.write(self._log_fd, piece[offset:])
                        if count <= 0:
                            self.overflowed = True
                            break
                        offset += count
                    written += len(piece)
                if len(chunk) > available:
                    self.overflowed = True
        finally:
            try:
                os.fsync(self._log_fd)
            finally:
                os.close(self._log_fd)
                os.close(self._read_fd)

    def finish(self) -> None:
        """Flush the framework stream and wait for the bounded drain to finish."""
        sys.stdout.flush()
        sys.stderr.flush()
        null_fd = os.open("/dev/null", os.O_WRONLY)
        try:
            os.dup2(null_fd, 1)
            os.dup2(null_fd, 2)
        finally:
            os.close(null_fd)
        self._thread.join(timeout=5)
        if self._thread.is_alive():
            raise MatrixWireError("installed HA framework log did not close")


def _proof_sha256(proof: dict) -> str:
    return hashlib.sha256(canonical_json_bytes(proof)).hexdigest()


def _safe_relative(value: object) -> str:
    if not isinstance(value, str) or not value or "\x00" in value or "\n" in value:
        raise MatrixWireError("installed member path is invalid")
    path = PurePosixPath(value)
    if path.is_absolute() or str(path) != value or ".." in path.parts:
        raise MatrixWireError("installed member path is invalid")
    return value


class _MatrixRequestCensus:
    """Record bounded outgoing attempts and completed HTTP transcripts."""

    MAX_ATTEMPTS = 1024

    def __init__(self, original_request) -> None:
        self.original_request = original_request
        self.attempts: list[dict] = []
        self.completed: list[dict] = []
        self.authorization_headers = 0
        self.last_event_id_headers = 0

    def mark(self) -> int:
        return len(self.attempts)

    async def request(self, session, method: str, url: str, **kwargs):
        if len(self.attempts) >= self.MAX_ATTEMPTS:
            raise MatrixWireError("HA request census exceeded its fixed bound")
        headers = {
            str(key).lower(): str(value)
            for key, value in kwargs.get("headers", {}).items()
        }
        if "authorization" in headers:
            self.authorization_headers += 1
        if "last-event-id" in headers:
            self.last_event_id_headers += 1
        path = urlsplit(str(url)).path
        parts = path.split("/")
        resource_id = None
        if len(parts) == 5 and parts[1:3] == ["v1", "pairings"]:
            resource_id = parts[3]
            parts[3] = "{pairing_id}"
        if len(parts) == 5 and parts[1:3] == ["v1", "vehicles"]:
            resource_id = parts[3]
            parts[3] = "{vehicle_id}"
        attempt = {
            "method": method.upper(),
            "route": "/".join(parts),
            "resource_id": resource_id,
            "outcome": "pending",
        }
        self.attempts.append(attempt)
        try:
            response = await self.original_request(session, method, url, **kwargs)
        except asyncio.CancelledError:
            attempt["outcome"] = "cancelled"
            raise
        except BaseException:
            attempt["outcome"] = "failed"
            raise
        request_id = response.headers.get("X-Request-ID")
        if not isinstance(request_id, str) or not request_id:
            raise MatrixWireError("completed HA request has no request identity")
        attempt.update(
            outcome="completed", status=response.status, request_id=request_id
        )
        self.completed.append(
            {
                "method": attempt["method"],
                "route": attempt["route"],
                "status": response.status,
                "request_id": request_id,
            }
        )
        return response

    def since(self, mark: int) -> list[dict]:
        return [
            {
                key: item[key]
                for key in ("method", "route", "status", "request_id")
            }
            for item in self.attempts[mark:]
            if item["outcome"] == "completed"
        ]

    def attempts_since(self, mark: int) -> list[dict]:
        return [dict(item) for item in self.attempts[mark:]]

    def retained_attempts(
        self, mark: int = 0, *, unload_mark: int
    ) -> list[dict[str, str]]:
        """Return only bounded nonsecret dispatch facts for public evidence."""
        if not 0 <= mark <= unload_mark <= len(self.attempts):
            raise MatrixWireError("HA request census mark is invalid")
        return [
            {
                "method": item["method"],
                "route": item["route"],
                "outcome": item["outcome"],
                "phase": (
                    "before_unload" if index < unload_mark else "after_unload"
                ),
            }
            for index, item in enumerate(self.attempts[mark:], start=mark)
        ]


class MatrixRuntime:
    """State shared by the fixed pytest plugin and top-level coordinator."""

    def __init__(
        self,
        session: dict,
        session_path: Path,
        broker: StdioBroker,
    ) -> None:
        self.session = session
        self.session_path = session_path
        self.session_input_sha256 = hashlib.sha256(
            session_path.read_bytes()
        ).hexdigest()
        self.broker = broker
        self.header = read_staged_json(session["header"], label="evidence header")
        self.scenario = read_staged_json(
            session["inputs"]["scenario"], label="scenario"
        )
        self.contract = read_staged_json(
            session["case_contract"], label="HA matrix contract"
        )
        self._validate_static_inputs()
        output_root = Path(session["outputs"]["normalized"]).parent
        metadata = output_root.stat(follow_symlinks=False)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or metadata.st_mode & 0o077
        ):
            raise MatrixWireError("HA output root ownership is invalid")
        self.raw_root = output_root / "raw"
        self.raw_root.mkdir(mode=0o700)
        self.channel = CoordinationChannel(
            session["outputs"]["coordination_dir"],
            session_id=session["session_id"],
            cell_id=session["cell_id"],
            session_input_sha256=self.session_input_sha256,
            instance_nonce=session["instance_nonce"],
            cleanup_timeout_ms=session["bounds"]["cleanup_timeout_ms"],
        )
        self.running: dict | None = None
        self.sequence: int | None = None
        self.observations: dict[int, dict] = {}
        self.raw: dict[str, dict] = {}
        self.raw_bindings: dict[str, dict] = {}
        self.case_facts: dict[str, dict] | None = None
        self.expired_fixture_facts: dict | None = None

    def _validate_static_inputs(self) -> None:
        if set(self.header) != HEADER_FIELDS:
            raise MatrixWireError("evidence header shape is invalid")
        if (
            self.header["schema_version"] != 1
            or self.header["execution_kind"] != "actual_hub_acceptance"
            or self.header["adapter"] != "home_assistant"
            or self.header["cell_id"] != self.session["cell_id"]
            or self.header["product_version"] != "2026.36.2"
            or self.header["profile_id"] != "hub-http-v1"
            or self.header["profile_revision"] != "1.0.0"
            or self.header["profile_sha256"]
            != self.session["inputs"]["profile_manifest"]["local"]["sha256"]
        ):
            raise MatrixWireError("evidence header identity is invalid")
        if (
            self.contract.get("schema_version") != 1
            or self.contract.get("adapter_id") != "home_assistant"
            or self.contract.get("revision") != 1
            or self.contract.get("required_cases") != list(REQUIRED_CASES)
        ):
            raise MatrixWireError("HA matrix contract identity is invalid")
        if (
            self.scenario.get("schema_version") != 1
            or self.scenario.get("name") != "two-vehicles-five-drives"
            or self.scenario.get("provenance") != "synthetic-only"
            or self.scenario.get("observed_at_ms") != 1788566400000
            or self.scenario.get("vehicle_ids")
            != [
                "11111111-1111-4111-8111-111111111111",
                "22222222-2222-4222-8222-222222222222",
            ]
            or self.scenario.get("vehicles")
            != [
                {
                    "vehicle_id": "11111111-1111-4111-8111-111111111111",
                    "display_name": "Interop \N{EN DASH} Árvíztűrő 🚗",
                },
                {
                    "vehicle_id": "22222222-2222-4222-8222-222222222222",
                    "display_name": "Interop empty",
                },
            ]
            or self.scenario.get("empty_vehicle_observed_at_ms") is not None
            or self.scenario.get("current", {}).get("battery_level") != 0
            or self.scenario.get("current", {}).get("inside_temp") != 21.5
            or self.scenario.get("current", {}).get("outside_temp") is not None
            or self.scenario.get("later_current")
            != {
                "observed_at_ms": 1788566460000,
                "battery_level": 1,
                "inside_temp": 22.5,
                "outside_temp": None,
            }
        ):
            raise MatrixWireError("HA scenario contents are invalid")
        certificate_path = self.session["inputs"]["certificate"]["local"]["path"]
        if (
            os.environ.get("SSL_CERT_FILE") != certificate_path
            or os.environ.get("REQUESTS_CA_BUNDLE") != certificate_path
        ):
            raise MatrixWireError(
                "HA trusted CA environment is not bound to staged input"
            )
        artifacts = self.header["artifacts"]
        if not isinstance(artifacts, list):
            raise MatrixWireError("evidence header artifacts are invalid")
        archive = next(
            (
                item
                for item in artifacts
                if isinstance(item, dict)
                and item.get("role") == "home_assistant_integration_archive"
            ),
            None,
        )
        product = self.session["inputs"]["product_inputs"][0]
        if (
            archive is None
            or archive.get("sha256") != product["staged"]["local"]["sha256"]
        ):
            raise MatrixWireError("HA archive binding is invalid")
        self.installed_manifest = read_staged_json(
            product["installed_manifest"], label="installed integration manifest"
        )
        if set(self.installed_manifest) != {
            "schema_version",
            "artifact_sha256",
            "files",
        }:
            raise MatrixWireError("installed integration manifest shape is invalid")
        if (
            self.installed_manifest["schema_version"] != 1
            or self.installed_manifest["artifact_sha256"] != archive["sha256"]
            or not isinstance(self.installed_manifest["files"], list)
        ):
            raise MatrixWireError("installed integration manifest identity is invalid")
        expected = []
        seen: set[str] = set()
        for row in self.installed_manifest["files"]:
            if not isinstance(row, dict) or set(row) != {
                "path",
                "bytes",
                "mode",
                "sha256",
            }:
                raise MatrixWireError("installed integration member shape is invalid")
            path = _safe_relative(row["path"])
            if path in seen:
                raise MatrixWireError("installed integration member is duplicated")
            seen.add(path)
            if (
                type(row["bytes"]) is not int
                or row["bytes"] < 0
                or type(row["mode"]) is not int
                or not isinstance(row["sha256"], str)
                or len(row["sha256"]) != 64
            ):
                raise MatrixWireError("installed integration member is invalid")
            expected.append(row)
        if [row["path"] for row in expected] != sorted(seen):
            raise MatrixWireError("installed integration members are not sorted")
        if len(expected) != 31:
            raise MatrixWireError("installed integration must contain 31 members")
        installed_root = Path(product["local_root"])
        if (
            installed_root.name != "teslatlas_hub"
            or installed_root.parent.name != "custom_components"
        ):
            raise MatrixWireError("installed integration root layout is invalid")
        self.installed_root = installed_root
        self.import_root = installed_root.parent.parent
        self._validate_installed_members()

    def _validate_installed_members(self) -> None:
        expected = self.installed_manifest["files"]
        actual = []
        installed_root = self.installed_root
        for path in sorted(installed_root.rglob("*")):
            if path.is_symlink():
                raise MatrixWireError("installed integration contains a symlink")
            if not path.is_file():
                continue
            raw = path.read_bytes()
            metadata = path.stat(follow_symlinks=False)
            actual.append(
                {
                    "path": str(path.relative_to(installed_root)),
                    "bytes": len(raw),
                    "mode": stat.S_IMODE(metadata.st_mode),
                    "sha256": hashlib.sha256(raw).hexdigest(),
                }
            )
        if actual != expected:
            raise MatrixWireError("installed integration members differ from manifest")

    def validate_loaded_modules(self) -> dict[str, str]:
        """Bind every executed integration module to admitted source bytes."""
        package = "custom_components.teslatlas_hub"
        required = {
            package,
            f"{package}.client",
            f"{package}.config_flow",
            f"{package}.const",
            f"{package}.coordinator",
            f"{package}.current_hub_client",
            f"{package}.entity",
            f"{package}.models",
            f"{package}.sensor",
        }
        installed_by_path = {
            row["path"]: row for row in self.installed_manifest["files"]
        }
        loaded: dict[str, str] = {}
        for name, module in sorted(sys.modules.items()):
            if name != package and not name.startswith(package + "."):
                continue
            origin = getattr(module, "__file__", None)
            if not isinstance(origin, str):
                raise MatrixWireError("loaded integration module has no source origin")
            source = Path(origin).resolve()
            try:
                relative = str(source.relative_to(self.installed_root.resolve()))
            except ValueError as error:
                raise MatrixWireError(
                    "loaded integration module is outside installed root"
                ) from error
            row = installed_by_path.get(relative)
            if row is None or source.suffix != ".py":
                raise MatrixWireError(
                    "loaded integration module is not admitted source"
                )
            if hashlib.sha256(source.read_bytes()).hexdigest() != row["sha256"]:
                raise MatrixWireError("loaded integration module differs from manifest")
            cached = getattr(module, "__cached__", None)
            if isinstance(cached, str) and Path(cached).exists():
                raise MatrixWireError("loaded integration module has cached bytecode")
            loaded[name] = relative
        if not required.issubset(loaded):
            raise MatrixWireError("required integration modules were not executed")
        self._validate_installed_members()
        return loaded

    def _admit_running(
        self,
        result: dict,
        *,
        operation: str,
        started_monotonic_ns: int,
        prior_sequence: int | None,
        device_id: str | None = None,
    ) -> dict:
        if not isinstance(result, dict) or set(result) != {
            "descriptor",
            "proof",
            "invitation",
            "expired_invitation",
            "events",
        }:
            raise MatrixWireError("running broker result shape is invalid")
        descriptor = result["descriptor"]
        if not isinstance(descriptor, dict) or set(descriptor) != {
            "status",
            "provenance",
            "endpoint",
            "hub_id",
            "hub_pid",
            "hub_started_at",
            "service_generation",
            "binary_sha256",
            "seed_binary_sha256",
            "profile_id",
            "profile_path",
            "profile_sha256",
            "scenario_path",
            "scenario_sha256",
            "certificate_path",
        }:
            raise MatrixWireError("running descriptor shape is invalid")
        proof = result["proof"]
        if (
            descriptor["status"] != "ready"
            or descriptor["provenance"] != "installed-package-service"
            or descriptor["profile_id"] != "hub-http-v1@1.0.0"
            or descriptor["profile_sha256"] != self.header["profile_sha256"]
            or descriptor["scenario_sha256"]
            != self.session["inputs"]["scenario"]["local"]["sha256"]
            or not isinstance(proof, dict)
            or proof.get("status") != "verified"
            or proof.get("session_id") != self.session["session_id"]
            or type(proof.get("sequence")) is not int
            or proof["sequence"] <= 0
            or proof.get("discovery", {}).get("hub_id") != descriptor["hub_id"]
            or proof.get("discovery", {}).get("product_version") != "2026.36.2"
            or proof.get("tls", {}).get("endpoint") != descriptor["endpoint"]
            or proof.get("tls", {}).get("certificate_der_sha256")
            != self.session["inputs"]["certificate_der_sha256"]
        ):
            raise MatrixWireError("running descriptor and proof identity differ")
        if prior_sequence is None:
            if self.sequence is not None:
                raise MatrixWireError("running proof has no authentic prior anchor")
        elif self.sequence != prior_sequence or proof["sequence"] <= prior_sequence:
            raise MatrixWireError("running proof sequence did not advance")
        self.running = result
        if not hasattr(self, "initial_expired_invitation"):
            self.initial_expired_invitation = dict(result["expired_invitation"])
        self.sequence = proof["sequence"]
        if operation == "start":
            # Root alone can bind the intervening stopped guest operation from
            # its controller journal. Do not fabricate that missing view here.
            return result
        self.observations[self.sequence] = {
            "schema_version": 1,
            "state": "running",
            "session_id": self.session["session_id"],
            "sequence": self.sequence,
            "operation": operation,
            "started_monotonic_ns": started_monotonic_ns,
            "finished_monotonic_ns": time.monotonic_ns(),
            "observed_at_ms": int(time.time() * 1000),
            "proof_sha256": _proof_sha256(proof),
            "result_sha256": hashlib.sha256(
                canonical_json_bytes(result)
            ).hexdigest(),
            "scenario_sha256": proof["config"]["scenario_sha256"],
            "seed_sha256": proof["config"]["seed_sha256"],
            "store_id": proof["config"]["store_id"],
            "store_schema_version": proof["config"]["store_schema_version"],
            "service_generation": proof["service"]["generation"],
            "hub_id": proof["discovery"]["hub_id"],
            "invitations": {
                "active": {
                    "pairing_id": result["invitation"]["pairingId"],
                    "expires_at_ms": result["invitation"]["expiresAtMs"],
                },
                "expired": {
                    "pairing_id": result["expired_invitation"]["pairingId"],
                    "expires_at_ms": result["expired_invitation"]["expiresAtMs"],
                },
            },
            "transition": self._local_transition(
                operation,
                prior_sequence=prior_sequence,
                device_id=device_id,
            ),
        }
        return result

    @staticmethod
    def _local_transition(
        operation: str,
        *,
        prior_sequence: int | None,
        device_id: str | None,
    ) -> dict | None:
        if operation == "verify":
            return None
        if prior_sequence is None:
            raise MatrixWireError("running transition has no prior observation")
        if operation == "revoke":
            return {
                "kind": "revoke",
                "from_sequence": prior_sequence,
                "device_id": device_id,
            }
        if operation == "pair":
            return {"kind": "pair", "from_sequence": prior_sequence}
        raise MatrixWireError("running transition operation is invalid")

    async def request(self, operation: str, *, device_id: str | None = None) -> dict:
        """Perform one broker operation away from the HA event loop."""
        prior_sequence = self.sequence
        started_monotonic_ns = time.monotonic_ns()
        result = await asyncio.to_thread(
            self.broker.request, operation, device_id=device_id
        )
        if operation != "stop":
            self._admit_running(
                result,
                operation=operation,
                started_monotonic_ns=started_monotonic_ns,
                prior_sequence=prior_sequence,
                device_id=device_id,
            )
        return result

    async def verify(self) -> dict:
        """Refresh and anchor the current installed service proof."""
        return await self.request("verify")

    def record_operation(
        self,
        operation: str,
        *,
        actor_id: str,
        before: int,
        after: int,
        facts: dict,
        requests: list[dict],
    ) -> dict:
        """Seal one schema-bound raw operation witness exactly once."""
        if operation not in SCHEMA_FOR_OPERATION:
            raise MatrixWireError("HA evidence operation is unknown")
        expected_actor = (
            "ha_client"
            if SCHEMA_FOR_OPERATION[operation] == "ha-client-v1"
            else "ha_flow"
        )
        if actor_id != expected_actor:
            raise MatrixWireError("HA evidence actor is invalid")
        evidence_id = f"raw-{operation}"
        if evidence_id in self.raw:
            raise MatrixWireError("HA evidence operation is duplicated")
        value = {
            "schema_version": 1,
            "session_id": self.session["session_id"],
            "cell_id": self.session["cell_id"],
            "session_input_sha256": self.session_input_sha256,
            "actor_id": actor_id,
            "operation": operation,
            "session_sequence_before": before,
            "session_sequence_after": after,
            "facts": facts,
            "requests": requests,
        }
        path = self.raw_root / f"{operation}.json"
        binding = write_exclusive_json(path, value)
        self.raw[evidence_id] = value
        self.raw_bindings[evidence_id] = binding
        return binding

    async def initial_barrier(self) -> dict:
        """Hold initial zero/null/entity evidence until root advances once."""
        if self.sequence is None or "raw-initial_poll" not in self.raw_bindings:
            raise MatrixWireError("initial HA evidence is incomplete")
        observation = self.observations[self.sequence]
        ack = await asyncio.to_thread(
            self.channel.barrier,
            phase="ha_initial_observation",
            observation={
                "session_sequence": self.sequence,
                "proof_sha256": observation["proof_sha256"],
            },
            evidence=self.raw_bindings["raw-initial_poll"],
        )
        result = await asyncio.to_thread(self.channel.read_ack_result, ack)
        if not isinstance(result, dict) or set(result) != {
            "proof",
            "advance",
            "invitation",
            "expired_invitation",
            "events",
        }:
            raise MatrixWireError("HA advance result shape is invalid")
        proof = result["proof"]
        advance = result["advance"]
        if (
            not isinstance(proof, dict)
            or proof.get("status") != "verified"
            or proof.get("session_id") != self.session["session_id"]
            or type(proof.get("sequence")) is not int
            or self.sequence is None
            or proof["sequence"] <= self.sequence
        ):
            raise MatrixWireError("HA advance proof identity is invalid")
        if (
            not isinstance(advance, dict)
            or set(advance)
            != {
                "before_store_sha256",
                "after_store_sha256",
                "scenario_sha256",
                "seed_sha256",
            }
            or any(
                not isinstance(advance.get(key), str)
                or len(advance[key]) != 64
                or any(
                    character not in "0123456789abcdef"
                    for character in advance[key]
                )
                for key in advance
            )
            or advance["before_store_sha256"] == advance["after_store_sha256"]
            or advance["scenario_sha256"]
            != proof.get("config", {}).get("scenario_sha256")
            or advance["seed_sha256"] != proof.get("config", {}).get("seed_sha256")
        ):
            raise MatrixWireError("HA advance result identity is invalid")
        self.sequence = proof["sequence"]
        return result

    def set_case_facts(self, facts: dict[str, dict]) -> None:
        """Retain the complete independently asserted 18-case fact map."""
        if tuple(facts) != REQUIRED_CASES:
            raise MatrixWireError("HA case fact set is incomplete or reordered")
        self.case_facts = facts

    def set_case_facts_from_raw(self) -> None:
        """Project enriched sealed operation facts into the fixed case shapes."""
        def facts(operation: str) -> dict:
            value = self.raw.get(f"raw-{operation}")
            if not isinstance(value, dict) or not isinstance(value.get("facts"), dict):
                raise MatrixWireError("HA operation facts are incomplete")
            return value["facts"]

        def select(operation: str, *keys: str) -> dict:
            source = facts(operation)
            try:
                return {key: source[key] for key in keys}
            except KeyError as error:
                raise MatrixWireError("HA operation facts are incomplete") from error

        runtime = facts("observe_installed_runtime")
        later = facts("later_poll")
        self.set_case_facts(
            {
                "candidate_artifact_identity": select(
                    "observe_installed_runtime",
                    "archive_sha256",
                    "integration_version",
                    "installed_manifest_sha256",
                    "installed_members",
                ),
                "installed_service_runtime": {
                    "service_mode": runtime["service_mode"],
                    "status": "pending",
                },
                "discovery_identity_profile": select(
                    "probe",
                    "hub_id",
                    "api_versions",
                    "protocol",
                    "protocol_major",
                    "version",
                ),
                "unauthenticated_discovery": select(
                    "probe",
                    "discovery_status",
                    "authorization_headers",
                    "credential_absent",
                ),
                "bad_invitation": select(
                    "bad_pair", "claim_status", "typed_error", "credential_created"
                ),
                "expired_invitation": select(
                    "expired_pair",
                    "claim_status",
                    "typed_error",
                    "credential_created",
                ),
                "replayed_invitation": select(
                    "replay_pair",
                    "claim_status",
                    "typed_error",
                    "credential_created",
                ),
                "real_auth": select(
                    "config_flow_setup",
                    "claim_status",
                    "vehicles",
                    "config_entry_loaded",
                ),
                "credential_lifecycle_reauth": select(
                    "reauthentication_flow",
                    "fresh_claim_status",
                    "entry_id_preserved",
                    "registry_ids_preserved",
                    "later_poll_status",
                ),
                "revocation": select(
                    "revocation_refresh",
                    "old_credential_status",
                    "typed_error",
                    "reauth_started",
                ),
                "unknown_vehicle": select(
                    "unknown_current",
                    "http_status",
                    "vehicle_id",
                    "state_of_charge",
                    "inside_temperature_c",
                ),
                "exact_current_values": {
                    **select(
                        "initial_poll",
                        "vehicles",
                        "sensors_per_vehicle",
                        "initial_battery",
                        "initial_inside_temperature_c",
                        "empty_state_of_charge",
                        "unicode_name",
                    ),
                    "later_battery": later["later_battery"],
                    "later_inside_temperature_c": later[
                        "later_inside_temperature_c"
                    ],
                },
                "endpoint_restart": select(
                    "endpoint_restart_poll",
                    "same_hub",
                    "new_service_generation",
                    "poll_status",
                ),
                "outage_recovery": select(
                    "outage_poll",
                    "unavailable_observed",
                    "recovered",
                    "registry_ids_preserved",
                ),
                "unsupported_operation_zero_requests": select(
                    "unsupported_surface", "operations", "outgoing_requests"
                ),
                "credential_loss_reauthentication": {
                    "credential_loss_observed": facts("revocation_refresh")[
                        "credential_loss_observed"
                    ],
                    "reauth_started": facts("revocation_refresh")["reauth_started"],
                    "reauth_completed": facts("reauthentication_flow")[
                        "reauth_completed"
                    ],
                    "entry_id_preserved": facts("reauthentication_flow")[
                        "entry_id_preserved"
                    ],
                    "registry_ids_preserved": facts("reauthentication_flow")[
                        "registry_ids_preserved"
                    ],
                },
                "installed_home_assistant_runtime": select(
                    "observe_installed_runtime",
                    "home_assistant_version",
                    "python_version",
                    "integration_version",
                    "iot_class",
                    "actors",
                ),
                "polling_transport_zero_sse": select(
                    "unload",
                    "polling_requests_positive",
                    "vehicle_requests_positive",
                    "current_requests_positive",
                    "sse_requests",
                    "last_event_id_requests",
                    "clean_unload",
                    "post_unload_requests",
                ),
            }
        )

    @staticmethod
    def request_census(original_request):
        return _MatrixRequestCensus(original_request)

    def set_expired_fixture_facts(self, facts: dict) -> None:
        """Bind the expired-claim witness reused by runtime admission."""
        if set(facts) != {
            "fixture_expires_at_ms",
            "observed_after_expiry_ms",
            "fixture_kind",
            "pairing_id",
        }:
            raise MatrixWireError("expired invitation fixture facts are invalid")
        if (
            type(facts["fixture_expires_at_ms"]) is not int
            or type(facts["observed_after_expiry_ms"]) is not int
            or facts["fixture_expires_at_ms"] >= facts["observed_after_expiry_ms"]
            or facts["fixture_kind"] != "persisted-one-second"
            or not isinstance(facts["pairing_id"], str)
        ):
            raise MatrixWireError("expired invitation fixture facts are invalid")
        self.expired_fixture_facts = dict(facts)

    @staticmethod
    def require_secret_changed(before: str, after: str) -> None:
        """Fail with a constant diagnostic without exposing either credential."""
        unchanged = secrets.compare_digest(before, after)
        del before, after
        if unchanged:
            MatrixRuntime._raise_secret_rotation_failure()

    @staticmethod
    def _raise_secret_rotation_failure() -> None:
        raise MatrixWireError("Home Assistant credential did not rotate")

    def _actors(self) -> dict[str, AdmittedActor]:
        actors = {}
        product = self.session["inputs"]["product_inputs"][0]
        python_path = Path(sys.executable)
        installed_manifest_sha256 = hashlib.sha256(
            Path(product["installed_manifest"]["local"]["path"]).read_bytes()
        ).hexdigest()
        for spec in self.session["actors"]:
            runtime = {
                "schema_version": 1,
                "runtime_ref": "ha_container",
                "runtime_kind": "docker-container",
                "container_id": (
                    "2c46e1bdf4c13159683b3aaa8e7ea22063ddc2841715658ce47cae7ecc02d6bc"
                ),
                "python": {
                    "executable": sys.executable,
                    "sha256": hashlib.sha256(python_path.read_bytes()).hexdigest(),
                    "version": platform.python_version(),
                },
                "home_assistant": {"version": "2026.8.3"},
                "artifact": {
                    "role": "home_assistant_integration_archive",
                    "sha256": product["staged"]["local"]["sha256"],
                    "installed_root": str(self.installed_root),
                    "installed_manifest_sha256": installed_manifest_sha256,
                    "installed_members": [
                        dict(row) for row in self.installed_manifest["files"]
                    ],
                },
                "actor_input_manifest_sha256": spec["input_manifest"]["local"]
                ["sha256"],
            }
            actors[spec["id"]] = AdmittedActor(
                id=spec["id"],
                kind=spec["kind"],
                runtime_ref=spec["runtime_ref"],
                entrypoint_ref=spec["entrypoint_ref"],
                artifact_roles=tuple(spec["artifact_roles"]),
                source_roles=tuple(spec["source_roles"]),
                installed_manifest=MappingProxyType(
                    dict(spec["input_manifest"]["local"])
                ),
                runtime=MappingProxyType(runtime),
            )
        return actors

    def _invocations(self) -> tuple[AdmittedInvocation, ...]:
        invocations = []
        for case_id in REQUIRED_CASES:
            actor_id, operations = CASE_BINDINGS[case_id]
            for operation in operations:
                raw = self.raw[f"raw-{operation}"]
                invocations.append(
                    AdmittedInvocation(
                        id=f"invoke-{case_id}-{operation}",
                        case_id=case_id,
                        actor_id=actor_id,
                        operation=operation,
                        session_sequence_before=raw["session_sequence_before"],
                        session_sequence_after=raw["session_sequence_after"],
                        evidence_id=f"raw-{operation}",
                        request_ids=tuple(
                            item["request_id"] for item in raw["requests"]
                        ),
                    )
                )
        return tuple(invocations)

    def _cases(
        self, context: AdmissionContext
    ) -> list[dict]:
        if self.case_facts is None:
            raise MatrixWireError("HA case facts were not supplied by the live test")
        invocations = context.invocations
        cases = []
        for case_id in REQUIRED_CASES:
            requests = []
            for invocation in invocations:
                if invocation.case_id == case_id:
                    requests.extend(self.raw[invocation.evidence_id]["requests"])
            value = {
                "id": case_id,
                "status": "pending"
                if case_id == "installed_service_runtime"
                else "passed",
                "expected": self.case_facts[case_id],
                "actual": self.case_facts[case_id],
                "evidence_kind": "zero_request"
                if case_id == "unsupported_operation_zero_requests"
                else "identity"
                if case_id
                in {
                    "candidate_artifact_identity",
                    "installed_service_runtime",
                    "installed_home_assistant_runtime",
                }
                else "http",
                "request_transcript": requests,
            }
            decision = admit_case(value, context)
            expected_status = (
                "pending" if case_id == "installed_service_runtime" else "passed"
            )
            independently_pending = (
                decision.status == "pending"
                and decision.code == "independent_observation_pending"
            )
            if decision.status != expected_status and not independently_pending:
                raise MatrixWireError("HA semantic admission rejected live evidence")
            cases.append(value)
        return cases

    async def finalize(self) -> None:
        """Seal outputs, verify once, and hold the broker through close ack."""
        if self.case_facts is None:
            raise MatrixWireError("HA live test did not publish case facts")
        await self.verify()
        assert self.sequence is not None
        actors = self._actors()
        invocations = self._invocations()
        context = AdmissionContext(
            adapter_id="home_assistant",
            cell_id=self.session["cell_id"],
            session_id=self.session["session_id"],
            header=MappingProxyType(self.header),
            scenario=MappingProxyType(self.scenario),
            actors=MappingProxyType(actors),
            invocations=invocations,
            raw=MappingProxyType(self.raw),
            controller_observations=MappingProxyType(
                {
                    key: MappingProxyType(value)
                    for key, value in self.observations.items()
                }
            ),
        )
        cases = self._cases(context)
        normalized = dict(self.header)
        normalized["cases"] = cases
        normalized_binding = write_exclusive_json(
            self.session["outputs"]["normalized"], normalized
        )
        actor_claims = []
        for actor_id, actor in actors.items():
            raw_evidence = []
            for evidence_id, raw in self.raw.items():
                if raw["actor_id"] != actor_id:
                    continue
                raw_evidence.append(
                    {
                        "id": evidence_id,
                        "schema_id": SCHEMA_FOR_OPERATION[raw["operation"]],
                        "binding": self.raw_bindings[evidence_id],
                    }
                )
            actor_claims.append(
                {
                    "id": actor.id,
                    "kind": actor.kind,
                    "runtime_ref": actor.runtime_ref,
                    "entrypoint_ref": actor.entrypoint_ref,
                    "artifact_roles": list(actor.artifact_roles),
                    "source_roles": list(actor.source_roles),
                    "installed_manifest": dict(actor.installed_manifest),
                    "raw_evidence": raw_evidence,
                }
            )
        actor_evidence = {
            "schema_version": 1,
            "session_id": self.session["session_id"],
            "cell_id": self.session["cell_id"],
            "session_input_sha256": self.session_input_sha256,
            "actors": actor_claims,
            "invocations": [
                {
                    "id": item.id,
                    "case_id": item.case_id,
                    "actor_id": item.actor_id,
                    "operation": item.operation,
                    "session_sequence_before": item.session_sequence_before,
                    "session_sequence_after": item.session_sequence_after,
                    "evidence_id": item.evidence_id,
                    "request_ids": list(item.request_ids),
                }
                for item in invocations
            ],
        }
        actors_binding = write_exclusive_json(
            self.session["outputs"]["actor_evidence"], actor_evidence
        )
        completion = {
            "schema_version": 1,
            "session_id": self.session["session_id"],
            "cell_id": self.session["cell_id"],
            "session_input_sha256": self.session_input_sha256,
            "normalized": normalized_binding,
            "actor_evidence": actors_binding,
        }
        completion_binding = write_exclusive_json(
            Path(self.session["outputs"]["coordination_dir"])
            / "adapter-completion.json",
            completion,
        )
        self.validate_loaded_modules()
        self.broker.assert_attached()
        observation = self.observations[self.sequence]
        await asyncio.to_thread(
            self.channel.barrier,
            phase="evidence_ready",
            observation={
                "session_sequence": self.sequence,
                "proof_sha256": observation["proof_sha256"],
            },
            evidence=completion_binding,
        )
        self.channel.assert_frozen(
            [
                *self.raw_bindings.values(),
                normalized_binding,
                actors_binding,
                completion_binding,
            ]
        )
        self.validate_loaded_modules()

    def runtime_facts(
        self, *, hub_id: str, entry_id: str, registry_ids: list[str]
    ) -> dict:
        """Build exact installed identity facts from admitted local inputs."""
        import importlib.metadata
        import json

        product = self.session["inputs"]["product_inputs"][0]
        actor_hashes = {
            spec["id"]: spec["input_manifest"]["local"]["sha256"]
            for spec in self.session["actors"]
        }
        manifest_path = product["installed_manifest"]["local"]["path"]
        if self.expired_fixture_facts is None:
            raise MatrixWireError("expired invitation fixture evidence is unavailable")
        loaded_modules = self.validate_loaded_modules()
        manifest = json.loads(
            (self.installed_root / "manifest.json").read_text(encoding="utf-8")
        )
        if (
            importlib.metadata.version("homeassistant") != "2026.8.3"
            or platform.python_version() != "3.14.2"
            or manifest.get("version") != "2026.36.2"
            or manifest.get("iot_class") != "local_poll"
            or manifest.get("domain") != "teslatlas_hub"
            or len(registry_ids) != 26
        ):
            raise MatrixWireError(
                "installed Home Assistant runtime identity is invalid"
            )
        return {
            "archive_sha256": product["staged"]["local"]["sha256"],
            "integration_version": "2026.36.2",
            "installed_manifest_sha256": hashlib.sha256(
                Path(manifest_path).read_bytes()
            ).hexdigest(),
            "installed_members": len(self.installed_manifest["files"]),
            "home_assistant_version": "2026.8.3",
            "python_version": platform.python_version(),
            "iot_class": "local_poll",
            "actors": ["ha_client", "ha_flow"],
            "service_mode": self.header["runtime"]["hub"]["service_mode"],
            "hub_id": hub_id,
            "actor_manifest_sha256s": actor_hashes,
            "expired_fixture": dict(self.expired_fixture_facts),
            "entry_id": entry_id,
            "registry_ids": registry_ids,
            "python_executable": sys.executable,
            "module_path": loaded_modules["custom_components.teslatlas_hub"],
            "module_sha256": next(
                row["sha256"]
                for row in self.installed_manifest["files"]
                if row["path"]
                == loaded_modules["custom_components.teslatlas_hub"]
            ),
            "loaded_modules": loaded_modules,
            "manifest_domain": manifest["domain"],
            "config_entry_loaded": True,
            "entity_registry_count": len(registry_ids),
        }


def _save_broker_descriptors() -> tuple[int, int]:
    read_fd = os.dup(0)
    write_fd = os.dup(1)
    os.set_inheritable(read_fd, False)
    os.set_inheritable(write_fd, False)
    return read_fd, write_fd


def _pytest_arguments(source_root: Path) -> list[str]:
    """Return the complete fixed isolated pytest configuration."""
    target = source_root / "tests" / "integration" / "test_live_hub.py"
    return [
        "-q",
        "--no-showlocals",
        "-o",
        "asyncio_mode=auto",
        str(target) + "::test_installed_matrix_all_cases_against_real_hub",
    ]


def _require_framework_success(result: object, ok: object, overflowed: bool) -> None:
    if result != ok or overflowed:
        raise MatrixWireError("installed HA framework execution failed")


def main(argv: list[str] | None = None) -> int:
    """Run the one fixed installed HA pytest target and completion barrier."""
    arguments = list(sys.argv[1:] if argv is None else argv)
    if len(arguments) != 1:
        return 2
    session_path = Path(arguments[0])
    broker: StdioBroker | None = None
    framework_log: FrameworkLog | None = None
    try:
        session = load_session_input(session_path)
        read_fd, write_fd = _save_broker_descriptors()
        framework_log = FrameworkLog(
            session["outputs"]["framework_log"],
            session["bounds"]["framework_log_bytes"],
        )
        broker = StdioBroker.attach(read_fd, write_fd, session["host_session"])
        runtime = MatrixRuntime(session, session_path, broker)
        sys.path.insert(0, str(runtime.import_root))
        os.environ["PYTHONDONTWRITEBYTECODE"] = "1"

        # Import pytest and the complete Home Assistant graph only after fd0/1/2
        # are isolated from the saved broker descriptors.
        import pytest

        os.environ["TESLATLAS_HA_LIVE"] = "1"

        class AdapterPlugin:
            @pytest.fixture
            def matrix_runtime(self) -> MatrixRuntime:
                return runtime

        result = pytest.main(
            _pytest_arguments(Path(__file__).resolve().parents[1]),
            plugins=[AdapterPlugin()],
        )
        framework_log.finish()
        _require_framework_success(
            result, pytest.ExitCode.OK, framework_log.overflowed
        )
        asyncio.run(runtime.finalize())
        broker.close()
        return 0
    except BaseException:
        if framework_log is not None:
            with suppress(BaseException):
                framework_log.finish()
        if broker is not None:
            with suppress(BaseException):
                broker.close()
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
