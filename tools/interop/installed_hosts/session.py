# SPDX-License-Identifier: AGPL-3.0-only
"""Runner-owned installed-host session, private broker, and durable journal."""
import hashlib
import copy
import ctypes
import json
import os
import secrets
import socket
import stat
import struct
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path

from .contract import (
    MAX_JSON_BYTES,
    ContractError,
    generation_changed,
    read_registered_config,
    validate_control_request,
    validate_descriptor,
    validate_invitation,
    validate_installed_observation,
    validate_package_manifest,
)
from .bounded import Deadline, IncrementalLineReader, write_all, durable_bytes, cleanup_error
from .lease import HostLease


OUTER_BUDGETS = {"greeting": 10, "verify": 30, "stop": 60, "start": 60, "pair": 160, "revoke": 160}
INNER_BUDGETS = {"greeting": 8, "verify": 25, "stop": 50, "start": 50, "pair": 145, "revoke": 145, "advance-once": 145}
CLEANUP_BUDGET = 45
CLEANUP_STAGE_BUDGETS = {"close_lock": 1, "operation_barrier": 5, "cancel": 2, "cancel_barrier": 3, "service": 9, "verify": 8, "transport": 9}
MUTATIONS = frozenset(("stop", "start", "pair", "revoke"))
MAX_OPERATION_RECORDS = 512


class SessionError(RuntimeError):
    pass


class CleanupError(SessionError):
    def __init__(self, evidence):
        super().__init__("installed-host cleanup failed")
        self.evidence = evidence


@dataclass(frozen=True)
class SessionEvidence:
    session_id: str
    state: str
    observations: tuple
    events: tuple
    journal_path: str
    journal_sha256: str
    final_stopped: object
    cleanup_errors: tuple
    local_transport: object


class _Journal:
    def __init__(self, path):
        self.path = Path(path)
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = self.path.parent.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_mode & 0o077:
            raise ContractError("local session root must be a private directory")
        os.chmod(str(self.path.parent), 0o700)
        self._file = self.path.open("xb", buffering=0)
        os.chmod(str(self.path), 0o600)
        self._lock = threading.Lock()

    def append(self, value, deadline=None):
        if deadline is not None:
            deadline.remaining()
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
        if len(raw) > MAX_JSON_BYTES:
            raise SessionError("journal entry exceeds bounded size")
        if deadline is not None:
            deadline.remaining()
        with self._lock:
            if deadline is not None:
                deadline.remaining()
            self._file.write(raw)
            os.fsync(self._file.fileno())
            if deadline is not None:
                deadline.remaining()

    def close(self):
        with self._lock:
            if not self._file.closed:
                self._file.flush()
                os.fsync(self._file.fileno())
                self._file.close()

    def digest(self, deadline=None):
        if deadline is not None:
            deadline.remaining()
        with self._lock:
            if not self._file.closed:
                self._file.flush()
                os.fsync(self._file.fileno())
                if deadline is not None:
                    deadline.remaining()
        digest = hashlib.sha256()
        with self.path.open("rb") as handle:
            while True:
                if deadline is not None:
                    deadline.remaining()
                block = handle.read(1024 * 1024)
                if not block:
                    break
                digest.update(block)
        if deadline is not None:
            deadline.remaining()
        return digest.hexdigest()


def _exact(value, keys, label):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise SessionError("{} has invalid fields".format(label))
    return value


def _validate_guest_greeting(value, session_id):
    value = _exact(value, ("schema_version", "type", "session_id", "sequence", "challenge"), "guest greeting")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1 or type(value["sequence"]) is not int or value["type"] != "challenge" or value["session_id"] != session_id or value["sequence"] != 0:
        raise SessionError("invalid guest greeting identity")
    if not isinstance(value["challenge"], str) or len(value["challenge"]) != 64:
        raise SessionError("invalid guest greeting challenge")
    try:
        bytes.fromhex(value["challenge"])
    except ValueError as error:
        raise SessionError("invalid guest greeting challenge") from error
    return value


def _validate_mac_stop_evidence(acquired, stop, session_id):
    """Validate the complete generation-bound launchd alternative."""
    full_keys = {
        "operation", "session_id", "wrapper", "hub", "app", "transient_helpers",
        "normal_exit", "normal_exit_code", "normal_exit_predicate",
        "stop_requested_monotonic_ns", "hub_absent_monotonic_ns",
        "elapsed_to_hub_absence_ns", "escalation_boundary_ns",
        "accepted_deadline_ns", "wrapper_helper_cleanup_cap_ns",
        "cleanup_absence_observations", "operation_sequence",
        "operation_challenge", "acquired_service_sha256",
    }
    app_only_keys = {
        "operation", "session_id", "hub", "app", "transient_helpers",
        "normal_exit", "normal_exit_code", "normal_exit_predicate",
        "stop_requested_monotonic_ns", "app_absent_monotonic_ns",
        "operation_sequence", "operation_challenge", "acquired_service_sha256",
    }
    common = (
        stop.get("operation") == "stop" and stop.get("session_id") == session_id
        and stop.get("normal_exit") is True and stop.get("normal_exit_code") == "unavailable"
    )
    if not common:
        raise SessionError("stopped macOS proof lacks the accepted unavailable-exit alternative")
    if stop.get("normal_exit_predicate") == "no-hub-generation-acquired-app-absence":
        if set(stop) != app_only_keys:
            raise SessionError("stopped macOS App-only proof has invalid fields")
        if acquired.get("acquisition_state") != "app-acquired" or acquired.get("hub") is not None or acquired.get("supervisor") is not None:
            raise SessionError("stopped macOS App-only proof follows a Hub acquisition")
        if stop["hub"] is not None or stop["app"] != acquired.get("app_process") or stop["transient_helpers"] != []:
            raise SessionError("stopped macOS App-only proof does not bind the acquired App")
        requested, absent = stop["stop_requested_monotonic_ns"], stop["app_absent_monotonic_ns"]
        if type(requested) is not int or type(absent) is not int or absent < requested:
            raise SessionError("stopped macOS App-only timestamp is invalid")
        return
    if stop.get("normal_exit_predicate") != "hub-child-pre-escalation-generation-absence" or set(stop) != full_keys:
        raise SessionError("stopped macOS proof has invalid fields")
    if stop["wrapper"] != acquired.get("supervisor") or stop["hub"] != acquired.get("hub") or stop["app"] != acquired.get("app_process") or stop["transient_helpers"] != acquired.get("transient_helpers", []):
        raise SessionError("stopped macOS proof does not bind the acquired process tree")
    integer_fields = (
        "stop_requested_monotonic_ns", "hub_absent_monotonic_ns",
        "elapsed_to_hub_absence_ns", "escalation_boundary_ns",
        "accepted_deadline_ns", "wrapper_helper_cleanup_cap_ns",
    )
    if any(type(stop[key]) is not int for key in integer_fields):
        raise SessionError("stopped macOS timestamp is not an exact JSON integer")
    requested = stop["stop_requested_monotonic_ns"]
    hub_absent = stop["hub_absent_monotonic_ns"]
    if requested < 0 or hub_absent < requested or stop["elapsed_to_hub_absence_ns"] != hub_absent - requested:
        raise SessionError("stopped macOS timestamp ordering is invalid")
    if stop["escalation_boundary_ns"] != 2_000_000_000 or stop["accepted_deadline_ns"] != 1_800_000_000 or stop["wrapper_helper_cleanup_cap_ns"] != 30_000_000_000 or hub_absent >= requested + stop["accepted_deadline_ns"]:
        raise SessionError("stopped macOS Hub absence missed the accepted deadline")
    expected = [("app", acquired.get("app_process")), ("wrapper", acquired.get("supervisor"))]
    expected.extend(("transient-helper", value) for value in acquired.get("transient_helpers", []))
    observations = stop["cleanup_absence_observations"]
    if not isinstance(observations, list) or len(observations) != len(expected):
        raise SessionError("stopped macOS cleanup observations are incomplete")
    cap = requested + stop["wrapper_helper_cleanup_cap_ns"]
    for row, (role, identity) in zip(observations, expected):
        if not isinstance(row, dict) or set(row) != {"role", "identity", "present", "observed_monotonic_ns"} or row["role"] != role or row["identity"] != identity or row["present"] is not False:
            raise SessionError("stopped macOS cleanup observation is invalid")
        observed = row["observed_monotonic_ns"]
        if type(observed) is not int or observed < requested:
            raise SessionError("stopped macOS cleanup timestamp is invalid")
        if observed > cap:
            raise SessionError("stopped macOS cleanup observation exceeded helper cleanup cap")


def _operation(value):
    if not isinstance(value, dict) or "op" not in value:
        raise ContractError("operation must be an object with op")
    fake = {
        "schema_version": 1,
        "session_id": "11111111-1111-4111-8111-111111111111",
        "sequence": 1,
        "challenge": "0" * 64,
        **value,
    }
    checked = validate_control_request(fake)
    return {key: checked[key] for key in ("op", "device_id") if key in checked}


def _hash_regular_file(binding, label, deadline=None):
    path = Path(binding["path"])
    if deadline is not None:
        deadline.remaining()
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode):
        raise ContractError("{} must be a regular non-symlink file".format(label))
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while True:
            if deadline is not None:
                deadline.remaining()
            block = handle.read(1024 * 1024)
            if not block:
                break
            digest.update(block)
    observed = digest.hexdigest()
    if deadline is not None:
        deadline.remaining()
    if observed != binding["sha256"]:
        raise ContractError("{}.sha256 does not match exact bytes".format(label))


def _read_json_file(path, deadline=None):
    """Read one bounded local JSON file while retaining the enclosing deadline."""
    target = Path(path)
    if deadline is not None:
        deadline.remaining()
    info = target.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_JSON_BYTES:
        raise ContractError("bounded local JSON input is not a regular file")
    value = bytearray()
    with target.open("rb") as handle:
        while True:
            if deadline is not None:
                deadline.remaining()
            block = handle.read(min(64 * 1024, MAX_JSON_BYTES + 1 - len(value)))
            if not block:
                break
            value.extend(block)
            if len(value) > MAX_JSON_BYTES:
                raise ContractError("bounded local JSON input exceeds its size bound")
    if deadline is not None:
        deadline.remaining()
    try:
        text = bytes(value).decode("utf-8")
        result = json.loads(text)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ContractError("bounded local JSON input is invalid") from error
    if deadline is not None:
        deadline.remaining()
    return result


def _copy_with_deadline(value, deadline=None):
    if deadline is not None:
        deadline.remaining()
    result = copy.deepcopy(value)
    if deadline is not None:
        deadline.remaining()
    return result


def _json_bytes_with_deadline(value, deadline=None):
    if deadline is not None:
        deadline.remaining()
    raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    if len(raw) > MAX_JSON_BYTES:
        raise SessionError("retained JSON exceeds bounded size")
    if deadline is not None:
        deadline.remaining()
    return raw


def _durable_bytes_with_deadline(path, raw, deadline=None):
    if deadline is not None:
        deadline.remaining()
    durable_bytes(path, raw)
    if deadline is not None:
        deadline.remaining()


class InstalledSession:
    """One installed guest lease and one owner-only adapter attachment."""

    def __init__(self, registered, transport, journal, host_lease):
        self.registered = registered
        self.transport = transport
        self.journal = journal
        self.state = "prepared"
        self._guest_sequence = 0
        self._guest_challenge = None
        self._operation_lock = threading.Lock()
        self._observations = []
        self._operation_records = []
        self._events = []
        self._last_running = None
        self._stopped_generation = None
        self._stopped_sequence = None
        self._stopped_challenge = None
        self._had_failure = False
        self._broker = None
        self._broker_thread = None
        self._broker_connection = None
        self._adapter_attached = False
        self._closing = False
        self._broker_temp_dir = None
        self._final_evidence = None
        self.package_manifest = None
        self._broker_failure = None
        self._host_lease = host_lease
        self._store_id = None
        self._verified = False
        self._close_lock = threading.Lock()
        self._cleanup_complete = False
        self._cleanup_stage_budgets = dict(CLEANUP_STAGE_BUDGETS)
        self._lifetime_deadline = Deadline(registered.config["lifetime_seconds"])
        self._service_outstanding = True
        self._partial_open = False
        self._pending_acquisition = None
        self._admitted_intent = None
        self._exchange_uncertain = False
        self._last_processed_request = None
        self._last_dispatched_request = None
        self._stop_candidates = []
        self._validated_stopped = None
        self.provider_tool_identity = None

    @classmethod
    def open(cls, config, registration_inventory, transport=None, verify_local_inputs=True, deadline=None):
        deadline = deadline or Deadline(OUTER_BUDGETS["greeting"])
        if not isinstance(deadline, Deadline):
            raise TypeError("session open requires a bounded Deadline")
        deadline.remaining()
        registered = read_registered_config(config, registration_inventory)
        host_lease = HostLease(registered).acquire(INNER_BUDGETS["greeting"] + CLEANUP_BUDGET)
        provider_identity = None
        try:
            if verify_local_inputs:
                from .transport import _verify_provider_tool
                provider_identity = _verify_provider_tool(registered.registration, deadline=deadline)
                for key in ("controller_bundle", "package", "package_manifest", "seed", "profile", "scenario"):
                    _hash_regular_file(registered.config[key], "session." + key, deadline=deadline)
                manifest = validate_package_manifest(
                    _read_json_file(registered.config["package_manifest"]["path"], deadline=deadline)
                )
                deadline.remaining()
                expected_path = "/usr/bin/teslatlas-hub" if registered.config["expected"]["os"] == "Debian 13" else "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
                expected = registered.config["expected"]
                if (manifest["package_sha256"], manifest["product_version"], manifest["os"], manifest["architecture"], manifest["hub_executable"]["path"], manifest["hub_executable"]["sha256"]) != (registered.config["package"]["sha256"], expected["product_version"], expected["os"], expected["architecture"], expected_path, expected["hub_executable_sha256"]):
                    raise ContractError("package manifest does not bind selected package and expected target")
            deadline.remaining()
        except BaseException:
            host_lease.release()
            raise
        if transport is None:
            from .transport import SSHTransport
            transport = SSHTransport()
        root = Path(registered.config["local_private_root"]) / registered.config["session_id"]
        journal = None
        session = None
        try:
            journal = _Journal(root / "controller.journal.jsonl")
            session = cls(registered, transport, journal, host_lease)
            session.package_manifest = manifest if verify_local_inputs else None
            session.provider_tool_identity = provider_identity
            if provider_identity is not None:
                provider_identity.revalidate(deadline)
            deadline.remaining()
            greeting = transport.open(registered, deadline)
            checked = _validate_guest_greeting(greeting, registered.config["session_id"])
            session._guest_challenge = checked["challenge"]
            session.state = "running"
            journal.append(
                {"kind": "opened", "session_id": registered.config["session_id"], "host_id": registered.config["host_id"]},
                deadline=deadline,
            )
            broker_path = root / "broker.sock"
            if len(os.fsencode(str(broker_path))) >= 96:
                session._broker_temp_dir = Path(tempfile.mkdtemp(prefix="teslatlas-ih-{}-".format(registered.config["session_id"][:8])))
                os.chmod(str(session._broker_temp_dir), 0o700)
                broker_path = session._broker_temp_dir / "broker.sock"
            session._open_broker(broker_path)
            deadline.remaining()
            return session
        except BaseException as open_error:
            if session is None:
                try:
                    transport.close(preserve_recovery=True)
                except BaseException:
                    pass
                if journal is not None:
                    journal.close()
                try:
                    host_lease.quarantine([{"resource":"partial-open","error":type(open_error).__name__}])
                finally:
                    host_lease.release()
            else:
                session._partial_open = True
                session._had_failure = True
                session.state = "failed"
                try:
                    session.close(deadline=deadline)
                except BaseException:
                    pass
                finally:
                    try:
                        session.journal.close()
                    except BaseException:
                        pass
                    host_lease.release()
            raise open_error

    @property
    def descriptor(self):
        if self._broker is None:
            raise SessionError("broker is not open")
        return validate_descriptor(
            {
                "schema_version": 1,
                "kind": "installed-host",
                "broker_socket": self._broker.getsockname(),
                "session_id": self.registered.config["session_id"],
                "registration_sha256": self.registered.registration_sha256,
            }
        )

    def observations_snapshot(self, deadline):
        """Return detached admitted running proofs under the operation barrier."""
        if not isinstance(deadline, Deadline):
            raise TypeError("observation snapshot requires a bounded Deadline")
        if not self._operation_lock.acquire(timeout=deadline.remaining()):
            raise TimeoutError("observation snapshot exceeded its operation barrier")
        try:
            deadline.remaining()
            copied = []
            for observation in self._observations:
                copied.append(_copy_with_deadline(observation, deadline))
            return tuple(copied)
        finally:
            self._operation_lock.release()

    def latest_observation(self, deadline):
        observations = self.observations_snapshot(deadline)
        if not observations:
            raise SessionError("no admitted installed observation is available")
        return observations[-1]

    def operation_records_snapshot(self, deadline):
        """Return detached public operation facts under the operation barrier."""
        if not isinstance(deadline, Deadline):
            raise TypeError("operation record snapshot requires a bounded Deadline")
        if not self._operation_lock.acquire(timeout=deadline.remaining()):
            raise TimeoutError("operation record snapshot exceeded its operation barrier")
        try:
            deadline.remaining()
            copied = []
            for record in self._operation_records:
                copied.append(_copy_with_deadline(record, deadline))
            return tuple(copied)
        finally:
            self._operation_lock.release()

    @staticmethod
    def _public_invitation(value):
        if not isinstance(value, dict):
            return None
        return {
            "pairing_id": value["pairingId"],
            "expires_at_ms": value["expiresAtMs"],
            "endpoint": value["endpoint"],
            "tls_pin": value["tlsPin"],
        }

    @staticmethod
    def _journal_operation_record(record):
        result_binding = record.get("result_binding")
        if isinstance(result_binding, dict):
            result_binding = {
                "name": Path(result_binding["path"]).name,
                "sha256": result_binding["sha256"],
            }
        return {
            "kind": "operation-record",
            "operation": record["operation"],
            "request": record["request"],
            "request_binding": record["request_binding"],
            "processed_binding": record["processed_binding"],
            "status": record["status"],
            "failure": record["failure"],
            "state": record["state"],
            "started_monotonic_ns": record["started_monotonic_ns"],
            "finished_monotonic_ns": record["finished_monotonic_ns"],
            "observed_at_ms": record["observed_at_ms"],
            "result_binding": result_binding,
            "result_sha256": record["result_sha256"],
            "proof": record["proof"],
            "invitation": record["invitation"],
            "expired_invitation": record["expired_invitation"],
            "advance": record["advance"],
            "events_sha256": record["events_sha256"],
        }

    def _retain_private_result(self, binding, result, deadline=None):
        if type(binding.get("sequence")) is not int or binding["sequence"] <= 0:
            raise SessionError("operation result has an invalid sequence binding")
        raw = _json_bytes_with_deadline(result, deadline)
        digest = hashlib.sha256(raw).hexdigest()
        if deadline is not None:
            deadline.remaining()
        path = self.journal.path.parent / "operation-{:06d}-result.json".format(binding["sequence"])
        _durable_bytes_with_deadline(path, raw, deadline)
        return {"path": str(path), "sha256": digest}

    def _record_operation(self, operation, binding, result, started_monotonic_ns, deadline=None):
        """Retain the non-secret facts for an admitted controller operation."""
        if len(self._operation_records) >= MAX_OPERATION_RECORDS:
            raise SessionError("operation record capacity exhausted")
        processed = self._last_processed_request
        if processed != binding:
            raise SessionError("operation did not complete its dispatched binding")
        request = _copy_with_deadline(operation, deadline)
        request_binding = _copy_with_deadline(binding, deadline)
        processed_binding = _copy_with_deadline(processed, deadline)
        proof = result.get("proof") if isinstance(result, dict) else None
        invitation = result.get("invitation") if isinstance(result, dict) else None
        expired_invitation = result.get("expired_invitation") if isinstance(result, dict) else None
        advance = result.get("advance") if isinstance(result, dict) else None
        events = result.get("events") if isinstance(result, dict) else None
        result_binding = self._retain_private_result(binding, result, deadline=deadline)
        events_raw = _json_bytes_with_deadline(events, deadline)
        events_sha256 = hashlib.sha256(events_raw).hexdigest()
        if deadline is not None:
            deadline.remaining()
        record = {
            "operation": operation["op"],
            "request": request,
            "request_binding": request_binding,
            "processed_binding": processed_binding,
            "status": "admitted",
            "failure": None,
            "state": self.state,
            "started_monotonic_ns": started_monotonic_ns,
            "finished_monotonic_ns": time.monotonic_ns(),
            "observed_at_ms": int(time.time() * 1000),
            "result_binding": result_binding,
            "result_sha256": result_binding["sha256"],
            "proof": _copy_with_deadline(proof, deadline),
            "invitation": self._public_invitation(invitation),
            "expired_invitation": self._public_invitation(expired_invitation),
            "advance": _copy_with_deadline(advance, deadline),
            "events_sha256": events_sha256,
            "final_stopped": None,
        }
        if deadline is not None:
            deadline.remaining()
        self.journal.append(self._journal_operation_record(record), deadline=deadline)
        self._operation_records.append(record)

    def _record_failed_operation(self, operation, binding, started_monotonic_ns, error):
        if len(self._operation_records) >= MAX_OPERATION_RECORDS:
            raise SessionError("operation record capacity exhausted")
        processed = self._last_processed_request
        record = {
            "operation": operation["op"],
            "request": copy.deepcopy(operation),
            "request_binding": copy.deepcopy(binding),
            "processed_binding": copy.deepcopy(processed) if processed == binding else None,
            "status": "failed",
            "failure": type(error).__name__,
            "state": self.state,
            "started_monotonic_ns": started_monotonic_ns,
            "finished_monotonic_ns": time.monotonic_ns(),
            "observed_at_ms": int(time.time() * 1000),
            "result_binding": None,
            "result_sha256": None,
            "proof": None,
            "invitation": None,
            "expired_invitation": None,
            "advance": None,
            "events_sha256": None,
            "final_stopped": None,
        }
        try:
            self.journal.append(self._journal_operation_record(record))
        except BaseException:
            # The in-memory row still preserves the redacted causal fact when
            # a terminal journal append itself fails; cleanup will quarantine.
            pass
        self._operation_records.append(record)

    def _record_final_stopped(self, final_stopped, deadline=None):
        if not isinstance(final_stopped, dict):
            raise SessionError("stopped proof is not an object")
        stop = final_stopped.get("service", {}).get("owned_generation", {}).get("stop_evidence", {})
        sequence, challenge = stop.get("operation_sequence"), stop.get("operation_challenge")
        if type(sequence) is not int or sequence <= 0 or not isinstance(challenge, str):
            raise SessionError("stopped proof does not carry a processed operation binding")
        for record in reversed(self._operation_records):
            if not isinstance(record, dict) or record.get("status") != "admitted":
                continue
            binding = record.get("processed_binding")
            if not isinstance(binding, dict):
                continue
            if type(binding.get("sequence")) is not int or binding["sequence"] <= 0 or not isinstance(binding.get("challenge"), str):
                continue
            if binding.get("op") == "stop" and binding.get("sequence") == sequence and binding.get("challenge") == challenge:
                record["final_stopped"] = _copy_with_deadline(final_stopped, deadline)
                self.journal.append(
                    {
                        "kind": "operation-final-stopped",
                        "operation": "stop",
                        "processed_binding": _copy_with_deadline(binding, deadline),
                        "final_stopped": _copy_with_deadline(final_stopped, deadline),
                    },
                    deadline=deadline,
                )
                return
        raise SessionError("stopped proof has no retained stop operation")

    def _open_broker(self, path):
        broker = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            broker.bind(str(path))
        except BaseException:
            broker.close()
            raise
        os.chmod(str(path), 0o600)
        broker.listen(1)
        broker.settimeout(0.2)
        self._broker = broker
        self._broker_thread = threading.Thread(target=self._serve_broker, name="installed-host-broker", daemon=True)
        self._broker_thread.start()

    def _peer_uid(self, connection):
        if hasattr(connection, "getpeereid"):
            return connection.getpeereid()[0]
        if hasattr(socket, "SO_PEERCRED"):
            raw = connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
            _pid, uid, _gid = struct.unpack("3i", raw)
            return uid
        # Darwin exposes getpeereid(3), but the Xcode Python 3.9 socket module
        # does not surface socket.getpeereid(). Bind the fixed libc API only.
        libc = ctypes.CDLL(None, use_errno=True)
        uid = ctypes.c_uint()
        gid = ctypes.c_uint()
        if libc.getpeereid(connection.fileno(), ctypes.byref(uid), ctypes.byref(gid)) != 0:
            raise SessionError("peer credential API failed")
        return int(uid.value)

    def _write_frame(self, connection, value, deadline=None):
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
        if len(raw) > MAX_JSON_BYTES:
            raise SessionError("broker reply exceeds bounded frame")
        write_all(connection.fileno(), raw, deadline or Deadline(10))

    def _read_frame(self, stream, deadline=None):
        raw = stream.read(deadline or Deadline(10))
        if not raw:
            return None
        if len(raw) > MAX_JSON_BYTES or not raw.endswith(b"\n"):
            raise ContractError("broker request exceeds bounded frame")
        try:
            return json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ContractError("broker request is not UTF-8 JSON") from error

    def _serve_broker(self):
        while not self._closing and self._broker is not None:
            try:
                connection, _ = self._broker.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            try:
                authorized = not self._adapter_attached and self._peer_uid(connection) == os.getuid()
            except BaseException:
                connection.close()
                self._mark_broker_failure("peer-credential-failed")
                threading.Thread(target=self._cleanup_after_broker_failure, name="installed-host-broker-cleanup", daemon=True).start()
                return
            if not authorized:
                connection.close()
                continue
            self._adapter_attached = True
            self._broker_connection = connection
            try:
                try:
                    self._serve_attachment(connection)
                except BaseException:
                    if not self._closing:
                        self._mark_broker_failure("attachment-io-failed")
            finally:
                try:
                    connection.close()
                finally:
                    self._broker_connection = None
            if self._broker_failure is not None and not self._closing:
                threading.Thread(target=self._cleanup_after_broker_failure, name="installed-host-broker-cleanup", daemon=True).start()
            return

    def _mark_broker_failure(self, reason):
        self._broker_failure = reason
        self._had_failure = True
        try:
            self.journal.append({"kind": "broker-failure", "reason": reason})
        except BaseException:
            pass

    def _cleanup_after_broker_failure(self):
        try:
            self.close()
        except CleanupError:
            pass

    def _serve_attachment(self, connection):
        sequence = 0
        # The private broker is an authenticated view of the already-open
        # guest session.  Keep one challenge chain across both boundaries so
        # an installed client can bind each broker proof to the exact guest
        # request that produced it.
        challenge = self._guest_challenge
        if not isinstance(challenge, str) or len(challenge) != 64:
            raise SessionError("guest challenge is unavailable for broker attachment")
        self._write_frame(connection, {"schema_version": 1, "type": "challenge", "session_id": self.registered.config["session_id"], "sequence": 0, "challenge": challenge}, Deadline(OUTER_BUDGETS["greeting"]))
        stream = IncrementalLineReader(connection.fileno(), MAX_JSON_BYTES)
        try:
            while not self._closing:
                stream.wait_readable(self._lifetime_deadline)
                frame_started = time.monotonic()
                try:
                    # The operation is inside the authenticated JSON frame, so
                    # use the smallest inner allowance until it is parsed.
                    request = self._read_frame(stream, Deadline(INNER_BUDGETS["verify"]))
                    if request is None:
                        if not self._closing:
                            self._mark_broker_failure("unexpected-eof")
                        return
                    checked = validate_control_request(request)
                    if checked["session_id"] != self.registered.config["session_id"] or checked["sequence"] != sequence + 1 or not secrets.compare_digest(checked["challenge"], challenge):
                        raise ContractError("stale, duplicate, or foreign broker request")
                    elapsed = time.monotonic() - frame_started
                    remaining = min(OUTER_BUDGETS[checked["op"]] - elapsed, INNER_BUDGETS[checked["op"]] - elapsed)
                    operation_deadline = Deadline(remaining)
                except ContractError:
                    self._mark_broker_failure("invalid-request")
                    challenge = secrets.token_hex(32)
                    self._write_frame(connection, {"schema_version": 1, "type": "error", "session_id": self.registered.config["session_id"], "sequence": sequence, "challenge": challenge, "error": {"code": "invalid-request"}}, Deadline(5))
                    return
                try:
                    result = self.request({key: checked[key] for key in ("op", "device_id") if key in checked}, deadline=operation_deadline)
                except (ContractError, SessionError, RuntimeError, TimeoutError):
                    self._mark_broker_failure("operation-failed")
                    challenge = secrets.token_hex(32)
                    self._write_frame(connection, {"schema_version": 1, "type": "error", "session_id": self.registered.config["session_id"], "sequence": sequence, "challenge": challenge, "error": {"code": "operation-failed"}}, operation_deadline)
                    return
                sequence = checked["sequence"]
                challenge = self._guest_challenge
                if not isinstance(challenge, str) or len(challenge) != 64:
                    raise SessionError("guest challenge disappeared after broker operation")
                self._write_frame(connection, {"schema_version": 1, "type": "reply", "session_id": checked["session_id"], "sequence": sequence, "challenge": challenge, "result": result}, operation_deadline)
        finally:
            pass

    def _exchange(self, operation, deadline=None):
        deadline = deadline or Deadline(INNER_BUDGETS[operation["op"]])
        self._guest_sequence += 1
        request = {
            "schema_version": 1,
            "session_id": self.registered.config["session_id"],
            "sequence": self._guest_sequence,
            "challenge": self._guest_challenge,
            **operation,
        }
        budget_ms = int(deadline.remaining() * 1000)
        maximum_ms = int(INNER_BUDGETS[operation["op"]] * 1000)
        request["budget_ms"] = max(1, min(budget_ms, maximum_ms))
        binding = {"sequence": request["sequence"], "challenge": request["challenge"], "op": operation["op"]}
        self._last_dispatched_request = binding
        if operation["op"] in ("start", "pair", "revoke", "advance-once"):
            self._pending_acquisition = binding
            self._service_outstanding = True
            self._validated_stopped = None
            self.journal.append({"kind": "acquisition-dispatch", "intent": binding}, deadline=deadline)
        if operation["op"] == "stop":
            self._stop_candidates.append(binding)
            if self._pending_acquisition is None:
                self._stopped_generation = self._last_running
        self._exchange_uncertain = True
        reply = self.transport.exchange(request, timeout=deadline.remaining())
        reply = _exact(reply, ("schema_version", "type", "session_id", "sequence", "challenge", "result"), "guest reply")
        if type(reply["schema_version"]) is not int or reply["schema_version"] != 1 or reply["type"] != "reply" or reply["session_id"] != request["session_id"]:
            raise SessionError("guest reply identity mismatch")
        if type(reply["sequence"]) is not int or reply["sequence"] != request["sequence"]:
            raise SessionError("guest reply sequence mismatch")
        challenge = reply["challenge"]
        if not isinstance(challenge, str) or len(challenge) != 64 or challenge == self._guest_challenge:
            raise SessionError("guest reply challenge was not fresh")
        try:
            bytes.fromhex(challenge)
        except ValueError as error:
            raise SessionError("guest reply challenge is invalid") from error
        self._guest_challenge = challenge
        self._exchange_uncertain = False
        self._last_processed_request = binding
        return reply["result"]

    def request(self, operation, deadline=None):
        operation = _operation(operation)
        deadline = deadline or Deadline(INNER_BUDGETS[operation["op"]])
        if self.state in ("closed", "failed") or self._closing or self._had_failure or self._exchange_uncertain:
            raise SessionError("session is not available for operations")
        if not self._operation_lock.acquire(timeout=deadline.remaining()):
            raise TimeoutError("operation queue exceeded its whole-operation budget")
        op = operation["op"]
        mutation = op in MUTATIONS
        binding = None
        started_monotonic_ns = None
        dispatched = False
        try:
            if self.state in ("closed", "failed") or self._closing or self._had_failure or self._exchange_uncertain:
                raise SessionError("session became unavailable while queued")
            self._host_lease.assert_valid(deadline.remaining() + CLEANUP_BUDGET if mutation else deadline.remaining())
            if op == "stop" and self.state != "running":
                raise SessionError("stop requires running state")
            if op == "start" and self.state != "stopped":
                raise SessionError("start requires stopped state")
            if op in ("pair", "revoke") and self.state != "running":
                raise SessionError("{} requires running state".format(op))
            if op == "verify" and self.state != "running":
                raise SessionError("verify requires running state")
            if mutation and not self._verified:
                raise SessionError("initial current verification is required before mutation")
            if len(self._operation_records) >= MAX_OPERATION_RECORDS - 1:
                raise SessionError("operation record capacity is reserved for cleanup")
            if mutation:
                self.journal.append(
                    {"kind": "intent", "operation": op, "sequence": self._guest_sequence + 1},
                    deadline=deadline,
                )
            prior_running = self._last_running
            proof_sequence = self._guest_sequence + 1
            proof_challenge = self._guest_challenge
            started_monotonic_ns = time.monotonic_ns()
            binding = {"sequence": proof_sequence, "challenge": proof_challenge, "op": op}
            dispatched = True
            result = self._exchange(operation, deadline)
            guest_result = result
            if op == "stop":
                result = _exact(result, ("stopped", "events"), "stop result")
                if result["stopped"] is not True or not isinstance(result["events"], list) or len(result["events"]) > 512:
                    raise SessionError("guest did not report stopped state")
                self._events = list(result["events"])
                self._stopped_generation = self._last_running
                self._stopped_sequence = proof_sequence
                self._stopped_challenge = proof_challenge
                self._service_outstanding = False
                self.state = "stopped"
            else:
                result = _exact(result, ("proof", "invitation", "expired_invitation", "events"), "running result")
                if not isinstance(result["events"], list) or len(result["events"]) > 512:
                    raise SessionError("running result events are not a bounded array")
                proof = validate_installed_observation(result["proof"], self.registered)
                if self.package_manifest is not None:
                    if proof["package"]["installed_version"] != self.package_manifest["package_manager_version"]:
                        raise SessionError("installed package-manager version differs from candidate manifest")
                    if proof["package"]["payload_manifest_sha256"] != self.package_manifest["payload_manifest_sha256"]:
                        raise SessionError("installed payload digest differs from candidate manifest")
                if proof["sequence"] != proof_sequence or not secrets.compare_digest(proof["challenge"], proof_challenge):
                    raise SessionError("proof does not bind the current guest request")
                if self.package_manifest is not None and proof["config"]["store_schema_version"] != self.package_manifest["store_schema_version"]:
                    raise SessionError("store schema version differs from the package manifest")
                if self._store_id is None:
                    self._store_id = proof["config"]["store_id"]
                elif proof["config"]["store_id"] != self._store_id:
                    raise SessionError("installed Hub store identity changed during the session")
                invitation = validate_invitation(result["invitation"], "running result.invitation")
                expired_invitation = validate_invitation(result["expired_invitation"], "running result.expired_invitation")
                if invitation["tlsPin"] != proof["tls"]["certificate_der_sha256"] or expired_invitation["tlsPin"] != proof["tls"]["certificate_der_sha256"]:
                    raise SessionError("invitation TLS pins differ from observed certificate")
                now_ms = int(time.time() * 1000)
                if expired_invitation["expiresAtMs"] >= now_ms or invitation["expiresAtMs"] <= now_ms or expired_invitation["pairingId"] == invitation["pairingId"]:
                    raise SessionError("fixed expired and active invitations have invalid lifetimes")
                certificate_digest = getattr(self.transport, "public_certificate_der_sha256", None)
                if certificate_digest is not None and proof["tls"]["certificate_der_sha256"] != certificate_digest:
                    raise SessionError("local tunnel certificate differs from guest observation")
                if hasattr(self.transport, "verify_local_tunnel"):
                    self.transport.verify_local_tunnel(proof, deadline)
                if op == "start" and self._stopped_generation is not None and not generation_changed(self._stopped_generation, proof):
                    raise SessionError("service generation did not change after start")
                if op in ("pair", "revoke") and prior_running is not None and not generation_changed(prior_running, proof):
                    raise SessionError("service generation did not change after {}".format(op))
                self._observations.append(proof)
                if op == "verify":
                    self._verified = True
                self._last_running = proof
                if self._pending_acquisition is not None:
                    self._admitted_intent = self._pending_acquisition
                    self._pending_acquisition = None
                self._service_outstanding = True
                self.state = "running"
                descriptor = {
                    "status": "ready",
                    "provenance": "installed-package-service",
                    "endpoint": proof["tls"]["endpoint"],
                    "hub_id": proof["discovery"]["hub_id"],
                    "hub_pid": proof["service"]["hub"]["pid"],
                    "hub_started_at": proof["service"]["hub"]["start_identity"],
                    "service_generation": proof["service"]["generation"],
                    "binary_sha256": proof["service"]["hub"]["executable_sha256"],
                    "seed_binary_sha256": proof["config"]["seed_sha256"],
                    "profile_id": proof["discovery"]["profile_id"],
                    "profile_path": self.registered.config["profile"]["path"],
                    "profile_sha256": proof["discovery"]["profile_sha256"],
                    "scenario_path": self.registered.config["scenario"]["path"],
                    "scenario_sha256": proof["config"]["scenario_sha256"],
                }
                certificate_path = getattr(self.transport, "public_certificate_path", None)
                if not isinstance(certificate_path, str) or not Path(certificate_path).is_absolute():
                    raise SessionError("transport did not publish the local certificate path")
                descriptor["certificate_path"] = certificate_path
                result = dict(result)
                result["descriptor"] = descriptor
                result["proof"] = proof
                self._events = list(result["events"])
            if mutation:
                proof_raw = _json_bytes_with_deadline(result.get("proof", {}), deadline)
                self.journal.append(
                    {
                        "kind": "result", "operation": op, "sequence": self._guest_sequence,
                        "state": self.state, "store_id": self._store_id,
                        "proof_sha256": hashlib.sha256(proof_raw).hexdigest(),
                    },
                    deadline=deadline,
                )
            deadline.remaining()
            self._record_operation(operation, binding, guest_result, started_monotonic_ns, deadline=deadline)
            return result
        except BaseException as error:
            self._had_failure = True
            if dispatched:
                try:
                    self._record_failed_operation(operation, binding, started_monotonic_ns, error)
                except BaseException:
                    pass
            if mutation:
                try:
                    self.journal.append({"kind": "failure", "operation": op, "sequence": self._guest_sequence, "error": type(error).__name__})
                except BaseException:
                    pass
            raise
        finally:
            self._operation_lock.release()

    def verify_stopped(self, timeout=15):
        value = self.transport.verify_stopped(self.registered, timeout=timeout)
        value = _exact(value, ("schema_version", "status", "session_id", "host_id", "service", "listener"), "stopped proof")
        if type(value["schema_version"]) is not int or value["schema_version"] != 1 or value["status"] != "stopped" or value["session_id"] != self.registered.config["session_id"] or value["host_id"] != self.registered.config["host_id"]:
            raise SessionError("stopped verifier observed a foreign session or host")
        service = _exact(value["service"], ("state", "generation", "forced_escalation", "normal_exit", "owned_generation", "cleanup_errors"), "stopped service")
        listener = _exact(value["listener"], ("host", "port", "owner_pid"), "stopped listener")
        if type(listener["port"]) is not int or type(listener["owner_pid"]) not in (int, type(None)):
            raise SessionError("stopped listener uses invalid JSON integer types")
        if service["state"] != "stopped" or service["generation"] is not None or service["forced_escalation"] is not False or type(service["normal_exit"]) is not bool or not isinstance(service["owned_generation"], dict) or service["cleanup_errors"] != []:
            raise SessionError("stopped verifier did not observe a clean normal stop")
        owned = _exact(service["owned_generation"], ("service", "stop_evidence"), "stopped owned generation")
        acquired = owned["service"]
        stop = owned["stop_evidence"]
        if not isinstance(acquired, dict) or not isinstance(stop, dict) or stop.get("session_id") != self.registered.config["session_id"]:
            raise SessionError("stopped proof lacks session-bound acquisition/stop evidence")
        hub = acquired.get("hub") or {}
        pending_matches = self._pending_acquisition is not None and acquired.get("acquisition_intent") == self._pending_acquisition
        expected_proof = self._stopped_generation or self._last_running
        if self._pending_acquisition is None:
            expected_proof = self._last_running or expected_proof
        expected = expected_proof["service"] if isinstance(expected_proof, dict) else None
        if not pending_matches and expected is not None and any(acquired.get(key) != value for key, value in expected.items()):
            raise SessionError("stopped proof does not bind the last acquired generation")
        if self._pending_acquisition is not None and not pending_matches and expected is None:
            raise SessionError("stopped proof does not bind the pending acquisition intent")
        # Only the exact dispatched intent can bind an unadmitted B. A retained
        # stop A is allowed only with A's known generation and stop operation.
        candidates = []
        if pending_matches:
            candidates.append(self._pending_acquisition)
        else:
            if self._pending_acquisition is not None:
                candidates.append(self._pending_acquisition)
            candidates.extend(self._stop_candidates)
            if self._last_dispatched_request is not None:
                candidates.append(self._last_dispatched_request)
            if self._last_processed_request is not None:
                candidates.append(self._last_processed_request)
            if self._stopped_sequence is not None:
                candidates.append({"sequence": self._stopped_sequence, "challenge": self._stopped_challenge})
        if candidates and not any(stop.get("operation_sequence") == item["sequence"] and stop.get("operation_challenge") == item["challenge"] for item in candidates):
            raise SessionError("stopped proof does not bind the last acquired stop operation")
        acquired_digest = hashlib.sha256(json.dumps(acquired, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        if stop.get("acquired_service_sha256") != acquired_digest:
            raise SessionError("stopped proof does not bind the last acquired stop operation")
        cleanup_only = stop.get("cleanup_only") is True
        if service["normal_exit"] is not True and not cleanup_only:
            raise SessionError("stopped verifier did not observe a clean normal stop")
        if self.registered.config["expected"]["os"] == "Debian 13":
            linux_keys = {
                "operation", "session_id", "pid", "hub_start_identity", "control_group",
                "invocation_id", "result_raw", "exec_main_code_raw",
                "exec_main_code_semantic", "exec_main_status_raw", "normal_exit",
                "operation_sequence", "operation_challenge", "acquired_service_sha256",
            }
            if set(stop) != linux_keys or stop.get("operation") != "stop" or stop.get("pid") != hub.get("pid") or stop.get("hub_start_identity") != hub.get("start_identity") or stop.get("control_group") != acquired.get("control_group") or stop.get("invocation_id") != acquired.get("invocation_id") or (stop.get("result_raw"), stop.get("exec_main_code_raw"), stop.get("exec_main_code_semantic"), stop.get("exec_main_status_raw"), stop.get("normal_exit")) != ("success", "1", "exited", "0", True):
                raise SessionError("stopped Linux proof does not bind CLD_EXITED for the acquired generation")
        else:
            if cleanup_only:
                from .macos import validate_wrapper_cleanup
                if not validate_wrapper_cleanup(acquired, stop, self.registered.config["session_id"]):
                    raise SessionError("wrapper-only stopped evidence is invalid")
                self._had_failure = True
            else:
                _validate_mac_stop_evidence(acquired, stop, self.registered.config["session_id"])
        if listener != {"host": "127.0.0.1", "port": 18480, "owner_pid": None}:
            raise SessionError("stopped verifier found a listener owner")
        return value

    def advance_once_for_ha(self, deadline=None):
        """Private runner hook; it is never reachable through the broker."""
        if self.registered.config["cell_id"] not in {"home_assistant__macos_arm64", "home_assistant__debian13_amd64", "home_assistant__debian13_arm64"} or self.registered.config["adapter_id"] != "home_assistant" or self.registered.config["client_id"] != "home_assistant":
            raise SessionError("advance-once requires a bound Home Assistant cell")
        deadline = deadline or Deadline(INNER_BUDGETS["advance-once"])
        if not isinstance(deadline, Deadline):
            raise TypeError("advance-once requires a bounded Deadline")
        deadline.remaining()
        self._host_lease.assert_valid(deadline.remaining() + CLEANUP_BUDGET)
        before_result = self.request({"op": "verify"}, deadline=deadline)
        before = before_result["proof"]
        if not self._operation_lock.acquire(timeout=deadline.remaining()):
            raise TimeoutError("advance-once queue exceeded its whole-operation budget")
        binding = None
        started_monotonic_ns = None
        dispatched = False
        try:
            if self.state in ("closed", "failed") or self._closing or self._had_failure or self._exchange_uncertain:
                raise SessionError("session became unavailable while queued")
            if len(self._operation_records) >= MAX_OPERATION_RECORDS - 1:
                raise SessionError("operation record capacity is reserved for cleanup")
            self.journal.append(
                {"kind": "intent", "operation": "advance-once", "sequence": self._guest_sequence + 1},
                deadline=deadline,
            )
            operation = {
                "op": "advance-once",
                "scope": {
                    "run_id": self.registered.config["run_id"],
                    "cell_id": self.registered.config["cell_id"],
                    "seed_sha256": self.registered.config["seed"]["sha256"],
                    "scenario_sha256": self.registered.config["scenario"]["sha256"],
                },
            }
            proof_sequence = self._guest_sequence + 1
            proof_challenge = self._guest_challenge
            started_monotonic_ns = time.monotonic_ns()
            binding = {"sequence": proof_sequence, "challenge": proof_challenge, "op": "advance-once"}
            dispatched = True
            result = self._exchange(operation, deadline)
            guest_result = result
            if not isinstance(result, dict) or set(result) != {"proof", "advance", "invitation", "expired_invitation", "events"}:
                raise SessionError("advance-once result has invalid fields")
            proof = validate_installed_observation(result["proof"], self.registered)
            if proof["sequence"] != proof_sequence or not secrets.compare_digest(proof["challenge"], proof_challenge):
                raise SessionError("advance-once proof does not bind the current guest request")
            invitation = validate_invitation(result["invitation"], "advance-once result.invitation")
            expired = validate_invitation(result["expired_invitation"], "advance-once result.expired_invitation")
            if invitation["tlsPin"] != proof["tls"]["certificate_der_sha256"] or expired["tlsPin"] != proof["tls"]["certificate_der_sha256"]:
                raise SessionError("advance-once invitation TLS pins differ from observed certificate")
            certificate_digest = getattr(self.transport, "public_certificate_der_sha256", None)
            if certificate_digest is not None and certificate_digest != proof["tls"]["certificate_der_sha256"]:
                raise SessionError("advance-once local tunnel certificate differs from guest observation")
            if self.package_manifest is not None and (
                proof["package"]["installed_version"] != self.package_manifest["package_manager_version"]
                or proof["package"]["payload_manifest_sha256"] != self.package_manifest["payload_manifest_sha256"]
            ):
                raise SessionError("advance-once package evidence differs from candidate manifest")
            advance = result["advance"]
            if not isinstance(advance, dict) or set(advance) != {"before_store_sha256", "after_store_sha256", "scenario_sha256", "seed_sha256"}:
                raise SessionError("advance-once evidence has invalid fields")
            for key, value in advance.items():
                if not isinstance(value, str) or len(value) != 64:
                    raise SessionError("advance-once evidence digest is invalid")
            if advance["before_store_sha256"] == advance["after_store_sha256"] or advance["scenario_sha256"] != self.registered.config["scenario"]["sha256"] or advance["seed_sha256"] != self.registered.config["seed"]["sha256"]:
                raise SessionError("advance-once did not bind a changed synthetic store")
            if not generation_changed(before, proof):
                raise SessionError("advance-once did not produce a new service generation")
            self._observations.append(proof)
            self._last_running = proof
            self._admitted_intent = self._pending_acquisition
            self._pending_acquisition = None
            if not isinstance(result["events"], list) or len(result["events"]) > 512:
                raise SessionError("advance-once events are not a bounded array")
            self._events = list(result["events"])
            self.journal.append(
                {"kind": "result", "operation": "advance-once", "sequence": self._guest_sequence, "state": "running"},
                deadline=deadline,
            )
            deadline.remaining()
            self._record_operation(
                operation,
                binding,
                guest_result,
                started_monotonic_ns,
                deadline=deadline,
            )
            return result
        except BaseException as error:
            self._had_failure = True
            if dispatched:
                try:
                    self._record_failed_operation(operation, binding, started_monotonic_ns, error)
                except BaseException:
                    pass
            try:
                self.journal.append({"kind": "failure", "operation": "advance-once", "sequence": self._guest_sequence, "error": type(error).__name__})
            except BaseException:
                pass
            raise
        finally:
            self._operation_lock.release()

    def _close_broker(self, deadline=None):
        errors = []
        self._closing = True
        connection = self._broker_connection
        if connection is not None:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        broker = self._broker
        path = broker.getsockname() if broker is not None else None
        if broker is not None:
            try:
                broker.close()
            except BaseException as error:
                errors.append(error)
            self._broker = None
        if self._broker_thread is not None and self._broker_thread is not threading.current_thread():
            try:
                timeout = 2 if deadline is None else deadline.remaining(2)
            except BaseException as error:
                errors.append(error)
                timeout = 0
            try:
                self._broker_thread.join(timeout=max(0, timeout))
            except BaseException as error:
                errors.append(error)
            if self._broker_thread.is_alive():
                errors.append(TimeoutError("broker thread did not stop within cleanup deadline"))
        if path:
            try:
                Path(path).unlink()
            except FileNotFoundError:
                pass
            except BaseException as error:
                errors.append(error)
        if self._broker_temp_dir is not None:
            try:
                self._broker_temp_dir.rmdir()
            except FileNotFoundError:
                pass
            except BaseException as error:
                errors.append(error)
            self._broker_temp_dir = None
        return errors

    def _evidence(self, final_stopped, errors):
        state = "closed" if not errors and not self._had_failure and final_stopped is not None else "failed"
        self.state = state
        digest = self.journal.digest()
        return SessionEvidence(
            self.registered.config["session_id"], state, tuple(self._observations), tuple(self._events),
            str(self.journal.path), digest, final_stopped, tuple(errors), getattr(self.transport, "local_transport_evidence", None),
        )

    def _cleanup_timeout(self, deadline, stage, fraction=1, errors=None):
        try:
            return max(0.0, deadline.remaining(self._cleanup_stage_budgets[stage] * fraction))
        except TimeoutError as error:
            if errors is not None:
                entry = {"resource": "cleanup-deadline-" + stage, "error": "TimeoutError"}
                if entry not in errors:
                    errors.append(entry)
            return 0.0

    def close(self, deadline=None):
        deadline = deadline or Deadline(CLEANUP_BUDGET)
        if not isinstance(deadline, Deadline):
            raise TypeError("session cleanup requires a bounded Deadline")
        try:
            if deadline.remaining() > CLEANUP_BUDGET:
                raise ValueError("session cleanup deadline exceeds its fixed budget")
        except TimeoutError as error:
            self._had_failure = True
            evidence = self._evidence(None, [{"resource": "cleanup-deadline-close", "error": "TimeoutError"}])
            self._final_evidence = evidence
            raise CleanupError(evidence) from error
        if not self._close_lock.acquire(timeout=self._cleanup_timeout(deadline, "close_lock")):
            evidence = self._evidence(None, [{"resource": "close-serialization", "error": "TimeoutError"}])
            self._final_evidence = evidence
            raise CleanupError(evidence)
        try:
            return self._close_locked(deadline)
        finally:
            self._close_lock.release()

    @staticmethod
    def _append_cleanup_error(errors, resource, error):
        value = cleanup_error(resource, error)
        if value not in errors:
            errors.append(value)

    def _close_locked(self, deadline):
        if self._final_evidence is not None and self._cleanup_complete:
            if self._final_evidence.state != "closed":
                raise CleanupError(self._final_evidence)
            return self._final_evidence
        errors = []
        final_stopped = self._validated_stopped
        lease_valid = True
        try:
            self._host_lease.assert_valid(deadline.remaining())
        except TimeoutError as error:
            lease_valid = False
            self._append_cleanup_error(errors, "cleanup-lease", error)
        except BaseException as error:
            lease_valid = False
            self._append_cleanup_error(errors, "cleanup-lease", error)
        try:
            for error in self._close_broker(deadline):
                self._append_cleanup_error(errors, "broker", error)
        except BaseException as error:
            self._append_cleanup_error(errors, "broker", error)

        acquired = False
        operation_budget = self._cleanup_timeout(deadline, "operation_barrier", errors=errors)
        try:
            acquired = self._operation_lock.acquire(timeout=operation_budget)
        except BaseException as error:
            self._append_cleanup_error(errors, "active-operation", error)
        if not acquired:
            self._append_cleanup_error(errors, "active-operation", TimeoutError("operation barrier timed out"))
            if hasattr(self.transport, "interrupt"):
                cancel_budget = self._cleanup_timeout(deadline, "cancel", errors=errors)
                try:
                    self.transport.interrupt(timeout=cancel_budget)
                except BaseException as error:
                    self._append_cleanup_error(errors, "active-operation-cancel", error)
            cancel_barrier = self._cleanup_timeout(deadline, "cancel_barrier", errors=errors)
            try:
                acquired = self._operation_lock.acquire(timeout=cancel_barrier)
            except BaseException as error:
                self._append_cleanup_error(errors, "active-operation-cancel-barrier", error)
            if not acquired:
                self._append_cleanup_error(errors, "active-operation-cancel-barrier", TimeoutError("cancelled operation barrier timed out"))

        writer_barrier = not hasattr(self.transport, "finish_writer")
        if acquired and lease_valid and self._service_outstanding and self.state == "running" and not self._had_failure and not self._exchange_uncertain:
            stop_budget = self._cleanup_timeout(deadline, "service", 0.45, errors=errors)
            if stop_budget <= 0:
                self._append_cleanup_error(errors, "guest-service", TimeoutError("cleanup deadline expired before stop"))
            else:
                try:
                    stop_deadline = Deadline(stop_budget)
                    stop_challenge = self._guest_challenge
                    started_monotonic_ns = time.monotonic_ns()
                    result = self._exchange({"op": "stop"}, stop_deadline)
                    self._stopped_sequence = self._guest_sequence
                    self._stopped_challenge = stop_challenge
                    if result.get("stopped") is not True:
                        raise SessionError("cleanup stop did not report stopped")
                    self.state = "stopped"
                    self._service_outstanding = False
                    stop_deadline.remaining()
                    self._record_operation(
                        {"op": "stop"},
                        {"sequence": self._guest_sequence, "challenge": stop_challenge, "op": "stop"},
                        result,
                        started_monotonic_ns,
                        deadline=deadline,
                    )
                except BaseException as error:
                    self._append_cleanup_error(errors, "guest-service", error)
        if acquired:
            self._operation_lock.release()

        if hasattr(self.transport, "finish_writer"):
            writer_budget = self._cleanup_timeout(deadline, "service", 0.2, errors=errors)
            try:
                writer_barrier = self.transport.finish_writer(writer_budget) is True
                if not writer_barrier:
                    raise RuntimeError("original guest writer completion is unproved")
            except BaseException as error:
                self._append_cleanup_error(errors, "guest-writer-barrier", error)
                writer_barrier = False
        if acquired and lease_valid and writer_barrier and self._service_outstanding and hasattr(self.transport, "recover_stop"):
            recovery_budget = self._cleanup_timeout(deadline, "service", 0.35, errors=errors)
            if recovery_budget <= 0:
                self._append_cleanup_error(errors, "fixed-recovery-stop", TimeoutError("cleanup deadline expired before recovery stop"))
            else:
                try:
                    self.transport.recover_stop(self.registered, timeout=recovery_budget)
                    self.state = "stopped"
                except BaseException as recovery_error:
                    self._append_cleanup_error(errors, "fixed-recovery-stop", recovery_error)
        # A recovery is itself a writer. Recheck the latest barrier even when
        # its local subprocess was reaped after timeout or SSH failure.
        if hasattr(self.transport, "writer_complete"):
            try:
                writer_barrier = writer_barrier and self.transport.writer_complete()
            except BaseException as error:
                self._append_cleanup_error(errors, "guest-writer-barrier", error)
                writer_barrier = False
        if writer_barrier and final_stopped is None:
            verify_budget = self._cleanup_timeout(deadline, "verify", errors=errors)
            if verify_budget <= 0:
                self._append_cleanup_error(errors, "fresh-stopped-verifier", TimeoutError("cleanup deadline expired before stopped verification"))
            else:
                try:
                    observed = self.verify_stopped(timeout=verify_budget)
                    raw = _json_bytes_with_deadline(observed, deadline) + b"\n"
                    _durable_bytes_with_deadline(self.journal.path.parent / "final-stopped.json", raw, deadline)
                    self._validated_stopped = observed
                    final_stopped = observed
                    self._service_outstanding = False
                    record_budget = self._cleanup_timeout(deadline, "operation_barrier", errors=errors)
                    if not self._operation_lock.acquire(timeout=record_budget):
                        raise TimeoutError("final stopped operation record exceeded its barrier")
                    try:
                        self._record_final_stopped(observed, deadline=deadline)
                    finally:
                        self._operation_lock.release()
                except BaseException as error:
                    self._append_cleanup_error(errors, "fresh-stopped-verifier", error)
        elif not writer_barrier:
            self._append_cleanup_error(errors, "fresh-stopped-verifier", RuntimeError("writer barrier unavailable"))

        transport_clean = False
        transport_budget = self._cleanup_timeout(deadline, "transport", errors=errors)
        try:
            self.transport.close(
                timeout=transport_budget,
                preserve_recovery=final_stopped is None or self._partial_open or not lease_valid,
            )
            transport_clean = True
        except BaseException as error:
            self._append_cleanup_error(errors, "transport", error)
        guest_failure = getattr(self.transport, "guest_failure", None)
        if guest_failure is not None:
            self._had_failure = True
            errors.append(dict(guest_failure))

        try:
            deadline.remaining()
        except TimeoutError as error:
            self._append_cleanup_error(errors, "cleanup-deadline-finalization", error)
        # Resource cleanup can be complete while the row remains permanently
        # failed because an operation/recovery proof was lost.  Keep those
        # outcomes separate: ``_cleanup_complete`` means the owned resources
        # are released, while ``errors`` and ``_had_failure`` drive evidence.
        deadline_failed = any(
            isinstance(item, dict) and str(item.get("resource", "")).startswith("cleanup-deadline-")
            for item in errors
        )
        safe = final_stopped is not None and transport_clean and acquired and writer_barrier and lease_valid and not self._partial_open and not deadline_failed
        if safe:
            try:
                self._host_lease.release()
                self._cleanup_complete = True
            except BaseException as error:
                self._append_cleanup_error(errors, "lease-release", error)
                self._cleanup_complete = False
        if errors:
            self._had_failure = True
        if not safe:
            try:
                self._host_lease.quarantine(errors)
            except BaseException as error:
                self._append_cleanup_error(errors, "durable-quarantine", error)
        try:
            self.journal.append({"kind": "closed" if not errors and not self._had_failure else "cleanup-failure", "errors": errors})
        except BaseException as error:
            self._append_cleanup_error(errors, "journal-terminal", error)
            self._had_failure = True
        if self._cleanup_complete:
            try:
                self.journal.close()
            except BaseException as error:
                self._cleanup_complete = False
                self._had_failure = True
                self._append_cleanup_error(errors, "journal-close", error)
        try:
            evidence = self._evidence(final_stopped, errors)
        except BaseException as error:
            self._append_cleanup_error(errors, "journal-evidence", error)
            self._had_failure = True
            evidence = SessionEvidence(
                self.registered.config["session_id"], "failed", tuple(self._observations), tuple(self._events),
                str(self.journal.path), "", final_stopped, tuple(errors), getattr(self.transport, "local_transport_evidence", None),
            )
        self._final_evidence = evidence
        if evidence.state != "closed" or errors:
            raise CleanupError(evidence)
        return evidence
