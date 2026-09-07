# SPDX-License-Identifier: AGPL-3.0-only
"""Fixed installed guest controller; accepts bounded authenticated JSON only."""
import base64
import hashlib
import json
import os
import secrets
import signal
import stat
import sys
import threading
import importlib
import time
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from installed_hosts.contract import MAX_JSON_BYTES, RegisteredConfig, validate_control_request, validate_package_manifest, validate_registration, validate_session_config
    from installed_hosts.session import SessionError
    from installed_hosts.bounded import Deadline, IncrementalLineReader, write_all, cleanup_error
else:
    from .contract import MAX_JSON_BYTES, RegisteredConfig, validate_control_request, validate_package_manifest, validate_registration, validate_session_config
    from .session import SessionError
    from .bounded import Deadline, IncrementalLineReader, write_all, cleanup_error

MAX_INPUT_BYTES = 128 * 1024 * 1024


class GuestController:
    """State machine over a fixed platform implementation."""

    def __init__(self, registered, platform, challenge_factory=None, journal=None):
        self.registered = registered
        self.platform = platform
        self.challenge_factory = challenge_factory or (lambda: secrets.token_hex(32))
        self.journal = journal or (lambda value: None)
        self.sequence = 0
        self.challenge = None
        self.state = "new"
        self.invitation = None
        self.expired_invitation = None
        self.events = []
        self.baseline_devices = set()
        self.claimed_devices = set()
        self.cleanup_required = False
        self.seed_facts = None
        self.owned_service = None
        self.verified = False
        self.ownership_path = Path(self.platform.private["ownership_path"])
        self.config_context = None
        self.stop_evidence = None
        self.absence_evidence = None
        self.active_operation = None
        self.acquisition_intent = {"sequence": 0, "challenge": None, "op": "initial-start"}
        if hasattr(self.platform, "acquisition_callback"):
            self.platform.acquisition_callback = self._record_acquisition
        if hasattr(self.platform, "context_callback"):
            self.platform.context_callback = self._record_prepared_context

    def _lease(self, minimum_seconds=0):
        lease = self.registered.registration["lease"]
        if lease["expires_at_unix"] <= int(time.time() + minimum_seconds):
            raise SessionError("registered guest lease expired")

    def _record_acquisition(self, service):
        if getattr(self.platform, "last_context", None) is not None:
            self.config_context = dict(self.platform.last_context)
        if service.get("acquisition_state") == "pending-start":
            current = {}
            self.stop_evidence = None
            self.absence_evidence = None
        else:
            current = dict(self.owned_service or {})
        current.update(service)
        current.setdefault("acquisition_intent", dict(self.acquisition_intent))
        self.owned_service = current
        if hasattr(self.platform, "owned_acquisition"):
            self.platform.owned_acquisition = dict(current)
        self._write_ownership()

    def _record_prepared_context(self, context):
        """Persist the store/config baseline before the first launch side effect."""
        if not isinstance(context, dict):
            raise SessionError("prepared context is invalid")
        self.config_context = dict(context)
        self._write_ownership()

    def _bind_stop_evidence(self, raw, sequence, challenge):
        if not isinstance(raw, dict):
            raise SessionError("platform stop did not return raw evidence")
        acquired = self.owned_service
        if not isinstance(acquired, dict) or not acquired:
            raise SessionError("stop lacks an acquired service generation")
        value = dict(raw)
        value["operation_sequence"] = sequence
        value["operation_challenge"] = challenge
        value["acquired_service_sha256"] = hashlib.sha256(json.dumps(acquired, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        self.stop_evidence = value
        return value

    def _write_ownership(self, proof=None, normal_stop=False):
        if proof is not None and proof.get("config") is not None:
            self.config_context = dict(proof["config"])
        service = self.owned_service
        value = {
            "schema_version": 1, "session_id": self.registered.config["session_id"],
            "host_id": self.registered.config["host_id"], "lease": dict(self.registered.registration["lease"]),
            "store_id": self.seed_facts["store_id"] if self.seed_facts else (self.config_context or {}).get("store_id"),
            "store_schema_version": self.seed_facts["store_schema_version"] if self.seed_facts else (self.config_context or {}).get("store_schema_version"),
            "config_path": getattr(self.platform, "config_path", None),
            "config": self.config_context,
            "service": service, "claimed_device_ids": sorted(self.claimed_devices),
            "sequence": self.sequence, "challenge": self.challenge, "state": self.state,
            "normal_stop": bool(normal_stop), "stop_evidence": self.stop_evidence,
            "absence_evidence": self.absence_evidence,
            "active_operation": self.active_operation,
        }
        temporary = self.ownership_path.with_suffix(".tmp")
        temporary.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
        os.chmod(str(temporary), 0o600)
        with temporary.open("rb") as handle:
            os.fsync(handle.fileno())
        os.replace(str(temporary), str(self.ownership_path))
        directory = os.open(str(self.ownership_path.parent), os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        return value

    def open(self):
        self._lease()
        self.journal({"kind": "intent", "operation": "preflight"})
        preflight = self.platform.preflight()
        self.journal({"kind": "result", "operation": "preflight", "facts_sha256": hashlib.sha256(json.dumps(preflight, sort_keys=True, separators=(",", ":")).encode()).hexdigest()})
        self.cleanup_required = True
        self.journal({"kind": "intent", "operation": "prepare"})
        self.state = "preparing"
        prepared = self.platform.prepare()
        if not isinstance(prepared, dict) or set(prepared) != {"invitation", "expired_invitation", "seed_facts"}:
            raise SessionError("platform preparation did not return fixed invitations")
        self.invitation = prepared["invitation"]
        self.expired_invitation = prepared["expired_invitation"]
        self.seed_facts = prepared["seed_facts"]
        if set(self.seed_facts) != {"store_id", "store_schema_version", "scenario_sha256"} or self.seed_facts["scenario_sha256"] != self.registered.config["scenario"]["sha256"]:
            raise SessionError("platform preparation did not bind the seeded store")
        self.journal({"kind": "result", "operation": "prepare", "seed_facts": self.seed_facts})
        self.baseline_devices = set(self.platform.paired_device_ids())
        if hasattr(self.platform, "current_context"):
            self.config_context = self.platform.current_context()
        self.state = "stopped"
        self._write_ownership(normal_stop=True)
        self.journal({"kind": "intent", "operation": "initial-start"})
        self.state = "starting"
        self._write_ownership()
        self.platform.start()
        self.state = "running"
        if hasattr(self.platform, "capture_service"):
            self._record_acquisition(self.platform.capture_service())
        self.challenge = self.challenge_factory()
        self._write_ownership()
        return {
            "schema_version": 1,
            "type": "challenge",
            "session_id": self.registered.config["session_id"],
            "sequence": 0,
            "challenge": self.challenge,
        }

    def _observe(self, request):
        proof = self.platform.observe()
        proof = dict(proof)
        proof["schema_version"] = 1
        proof["status"] = "verified"
        proof["session_id"] = self.registered.config["session_id"]
        proof["sequence"] = request["sequence"]
        proof["challenge"] = request["challenge"]
        current_devices = set(self.platform.paired_device_ids())
        self.claimed_devices.update(current_devices - self.baseline_devices)
        if proof["config"]["store_id"] != self.seed_facts["store_id"] or proof["config"]["store_schema_version"] != self.seed_facts["store_schema_version"]:
            raise SessionError("observed store identity differs from seeded identity")
        for role in ("hub", "supervisor", "app_process"):
            previous = (self.owned_service or {}).get(role)
            if previous is not None and proof["service"].get(role) != previous:
                raise SessionError("running proof changed canonical acquired identity")
        self._record_acquisition(proof["service"])
        self._write_ownership(proof)
        self.journal({"kind": "observation", "sequence": request["sequence"], "challenge": request["challenge"], "proof_sha256": hashlib.sha256(json.dumps(proof, sort_keys=True, separators=(",", ":")).encode()).hexdigest(), "claimed_device_ids": sorted(self.claimed_devices)})
        return proof

    def handle(self, value, deadline=None):
        if not isinstance(value, dict) or type(value.get("budget_ms")) is not int or value["budget_ms"] <= 0:
            raise SessionError("private guest request budget_ms is invalid")
        request_value = dict(value)
        budget_ms = request_value.pop("budget_ms")
        request = validate_control_request(request_value)
        maximum_ms = {"verify": 25_000, "stop": 50_000, "start": 50_000, "pair": 145_000, "revoke": 145_000}[request["op"]]
        if budget_ms > maximum_ms:
            raise SessionError("private guest request budget exceeds operation maximum")
        deadline = deadline or Deadline(budget_ms / 1000)
        if hasattr(self.platform, "set_deadline"):
            self.platform.set_deadline(deadline)
        if request["session_id"] != self.registered.config["session_id"] or request["sequence"] != self.sequence + 1 or not secrets.compare_digest(request["challenge"], self.challenge):
            raise SessionError("stale, duplicate, or foreign guest request")
        self._lease(deadline.remaining())
        op = request["op"]
        if op == "revoke" and request["device_id"] not in self.claimed_devices:
            raise SessionError("device was not claimed in this session")
        self.active_operation = {"sequence": request["sequence"], "challenge": request["challenge"], "op": op}
        if op in ("start", "pair", "revoke"):
            self.acquisition_intent = dict(self.active_operation)
        mutation = op in ("stop", "start", "pair", "revoke")
        if mutation:
            if not self.verified:
                raise SessionError("initial current verification is required before mutation")
            self._lease(deadline.remaining() + 45)
            if hasattr(self.platform, "assert_owned"):
                self.platform.assert_owned(json.loads(self.ownership_path.read_text(encoding="utf-8")))
            self.journal({"kind": "intent", "operation": op, "sequence": request["sequence"]})
        try:
            if op == "verify":
                if self.state != "running":
                    raise SessionError("verify requires running state")
                result = {"proof": self._observe(request), "invitation": self.invitation, "expired_invitation": self.expired_invitation, "events": list(self.events)}
                self.verified = True
            elif op == "stop":
                if self.state != "running":
                    raise SessionError("stop requires running state")
                self.state = "stopping"
                self._write_ownership()
                self._bind_stop_evidence(self.platform.stop(), request["sequence"], request["challenge"])
                self.state = "stopped"
                self._write_ownership(normal_stop=True)
                event = {"operation": "stop", "forced_escalation": False}
                self.events.append(event)
                result = {"stopped": True, "events": list(self.events)}
            elif op == "start":
                if self.state != "stopped":
                    raise SessionError("start requires stopped state")
                self.state = "starting"
                self._write_ownership()
                self.platform.start()
                self.state = "running"
                if hasattr(self.platform, "capture_service"):
                    self._record_acquisition(self.platform.capture_service())
                self.events.append({"operation": "start"})
                result = {"proof": self._observe(request), "invitation": self.invitation, "expired_invitation": self.expired_invitation, "events": list(self.events)}
            else:
                if self.state != "running":
                    raise SessionError("{} requires running state".format(op))
                self.state = "stopping"
                self._write_ownership()
                self._bind_stop_evidence(self.platform.stop(), request["sequence"], request["challenge"])
                self.state = "stopped"
                self._write_ownership(normal_stop=True)
                if op == "pair":
                    self.invitation = self.platform.pair(900)
                    event = {"operation": "pair"}
                else:
                    self.platform.revoke(request["device_id"])
                    self.claimed_devices.remove(request["device_id"])
                    event = {"operation": "revoke", "device_id": request["device_id"]}
                if hasattr(self.platform, "assert_owned"):
                    self.platform.assert_owned(json.loads(self.ownership_path.read_text(encoding="utf-8")))
                self.state = "starting"
                self._write_ownership()
                self.platform.start()
                self.state = "running"
                if hasattr(self.platform, "capture_service"):
                    self._record_acquisition(self.platform.capture_service())
                self.events.append(event)
                result = {"proof": self._observe(request), "invitation": self.invitation, "expired_invitation": self.expired_invitation, "events": list(self.events)}
            self.sequence = request["sequence"]
            self.challenge = self.challenge_factory()
            self._write_ownership(result.get("proof") if isinstance(result, dict) else None, normal_stop=self.state == "stopped")
            if mutation:
                self.journal({"kind": "result", "operation": op, "sequence": self.sequence, "state": self.state})
            return {
                "schema_version": 1, "type": "reply", "session_id": request["session_id"],
                "sequence": self.sequence, "challenge": self.challenge, "result": result,
            }
        except BaseException as error:
            if mutation:
                self.journal({"kind": "failure", "operation": op, "sequence": request["sequence"], "error": type(error).__name__})
            try:
                self.state = "failed"
                if self.cleanup_required:
                    self._write_ownership()
            except BaseException as journal_error:
                self.journal({"kind": "cleanup-failure", "operation": op, "resource": "ownership", "error": type(journal_error).__name__})
            raise

    def handle_advance_once(self, value, deadline=None):
        required = {"schema_version", "session_id", "sequence", "challenge", "op", "scope", "budget_ms"}
        if not isinstance(value, dict) or set(value) != required or type(value["schema_version"]) is not int or value["schema_version"] != 1 or type(value["sequence"]) is not int or value["op"] != "advance-once":
            raise SessionError("invalid private advance-once request")
        if type(value["budget_ms"]) is not int or not 0 < value["budget_ms"] <= 145_000:
            raise SessionError("invalid private advance-once budget")
        deadline = deadline or Deadline(value["budget_ms"] / 1000)
        if hasattr(self.platform, "set_deadline"):
            self.platform.set_deadline(deadline)
        scope = value["scope"]
        expected_scope = {
            "run_id": self.registered.config["run_id"], "cell_id": self.registered.config["cell_id"],
            "seed_sha256": self.registered.config["seed"]["sha256"], "scenario_sha256": self.registered.config["scenario"]["sha256"],
        }
        ha_cells = {"home_assistant__macos_arm64", "home_assistant__debian13_amd64", "home_assistant__debian13_arm64"}
        if scope != expected_scope or self.registered.config["cell_id"] not in ha_cells or self.registered.config["adapter_id"] != "home_assistant" or self.registered.config["client_id"] != "home_assistant":
            raise SessionError("private advance-once scope is not the bound HA cell")
        if value["session_id"] != self.registered.config["session_id"] or value["sequence"] != self.sequence + 1 or not secrets.compare_digest(value["challenge"], self.challenge):
            raise SessionError("stale, duplicate, or foreign advance-once request")
        self._lease(deadline.remaining() + 45)
        if not self.verified:
            raise SessionError("advance-once requires current verification")
        if hasattr(self.platform, "assert_owned"):
            self.platform.assert_owned(json.loads(self.ownership_path.read_text(encoding="utf-8")))
        self.active_operation = {"sequence": value["sequence"], "challenge": value["challenge"], "op": "advance-once"}
        self.acquisition_intent = dict(self.active_operation)
        self.journal({"kind": "intent", "operation": "advance-once", "sequence": value["sequence"]})
        try:
            self.state = "stopping"
            self._write_ownership()
            self._bind_stop_evidence(self.platform.stop(), value["sequence"], value["challenge"])
            self.state = "stopped"
            self._write_ownership(normal_stop=True)
            advance = self.platform.advance_once()
            if hasattr(self.platform, "assert_owned"):
                self.platform.assert_owned(json.loads(self.ownership_path.read_text(encoding="utf-8")))
            self.state = "starting"
            self._write_ownership()
            self.platform.start()
            self.state = "running"
            if hasattr(self.platform, "capture_service"):
                self._record_acquisition(self.platform.capture_service())
            proof = self._observe(value)
            self.sequence = value["sequence"]
            self.challenge = self.challenge_factory()
            self.events.append({"operation": "advance-once"})
            self.journal({"kind": "result", "operation": "advance-once", "sequence": self.sequence, "state": self.state})
            return {"schema_version": 1, "type": "reply", "session_id": value["session_id"], "sequence": self.sequence, "challenge": self.challenge, "result": {"proof": proof, "advance": advance, "invitation": self.invitation, "expired_invitation": self.expired_invitation, "events": list(self.events)}}
        except BaseException as error:
            self.journal({"kind": "failure", "operation": "advance-once", "sequence": value["sequence"], "error": type(error).__name__})
            try:
                self.state = "failed"
                if self.cleanup_required:
                    self._write_ownership()
            except BaseException as journal_error:
                self.journal({"kind": "cleanup-failure", "operation": "advance-once", "resource": "ownership", "error": type(journal_error).__name__})
            raise

    def close(self):
        errors = []
        try:
            if self.cleanup_required:
                if hasattr(self.platform, "set_deadline"):
                    self.platform.set_deadline(Deadline(45))
                owned = json.loads(self.ownership_path.read_text(encoding="utf-8")) if self.ownership_path.exists() else None
                if owned is not None and self.stop_evidence is None and hasattr(self.platform, "reconcile_owned"):
                    reconciled = self.platform.reconcile_owned(owned)
                    if not isinstance(reconciled, dict) or not reconciled:
                        raise SessionError("pending acquisition could not be reconciled")
                    self._record_acquisition(reconciled)
                    owned = json.loads(self.ownership_path.read_text(encoding="utf-8"))
                if owned is not None and hasattr(self.platform, "assert_owned"):
                    self.platform.assert_owned(owned)
                if self.stop_evidence is None:
                    operation = self.active_operation or {"sequence": self.sequence, "challenge": self.challenge}
                    self._bind_stop_evidence(self.platform.stop(), operation["sequence"], operation["challenge"])
                    self.state = "stopped"
                    self._write_ownership(normal_stop=True)
        except BaseException as error:
            errors.append(cleanup_error("service", error))
        try:
            if hasattr(self.platform, "cleanup_partial"):
                self.platform.cleanup_partial()
        except BaseException as error:
            errors.append(cleanup_error("partial-acquisition", error))
        try:
            owned = json.loads(self.ownership_path.read_text(encoding="utf-8")) if self.ownership_path.exists() else None
            stopped = self.platform.verify_stopped(owned=owned)
            self.absence_evidence = {"observed_at_monotonic_ns": time.monotonic_ns(), "proof_sha256": hashlib.sha256(json.dumps(stopped, sort_keys=True, separators=(",", ":")).encode()).hexdigest()}
            if self.ownership_path.exists():
                self._write_ownership(normal_stop=self.stop_evidence is not None)
        except BaseException as error:
            stopped = None
            errors.append(cleanup_error("stopped-observer", error))
        self.journal({"kind": "closed" if not errors else "cleanup-failure", "errors": errors})
        self.state = "closed" if not errors else "failed"
        return stopped, errors


class DurableJournal:
    def __init__(self, path):
        self.path = Path(path)
        self.file = self.path.open("xb", buffering=0)
        os.chmod(str(self.path), 0o600)

    def __call__(self, value):
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
        if len(raw) > MAX_JSON_BYTES:
            raise SessionError("guest journal entry exceeds bound")
        self.file.write(raw)
        os.fsync(self.file.fileno())

    def close(self):
        if not self.file.closed:
            self.file.flush()
            os.fsync(self.file.fileno())
            self.file.close()


def _load_private(path):
    target = Path(path)
    if not target.is_absolute():
        raise SessionError("private session path must be absolute")
    info = target.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_JSON_BYTES or info.st_mode & 0o077:
        raise SessionError("private session must be a bounded mode-private regular file")
    raw = target.read_bytes()
    value = json.loads(raw.decode("utf-8"))
    required = {"schema_version", "session", "registration", "registration_json_b64", "registration_sha256", "controller_bundle_sha256", "controller_root", "guest_inputs", "installation_receipt", "ownership_path"}
    if not isinstance(value, dict) or set(value) != required or type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise SessionError("private session fields are invalid")
    cfg = validate_session_config(value["session"])
    reg = validate_registration(value["registration"])
    expected_root = "/tmp/teslatlas-installed-host-" + cfg["session_id"]
    if value["controller_root"] != expected_root or str(target.parent) != expected_root:
        raise SessionError("private session does not use its fixed guest controller root")
    if value["ownership_path"] != expected_root + "/ownership.json":
        raise SessionError("private ownership journal path is not fixed")
    if value["ownership_path"] != expected_root + "/ownership.json":
        raise SessionError("private ownership journal path is not fixed")
    receipt_root = "/Library/Application Support/Teslatlas Hub/interop-package-receipts" if reg["provider"] == "tart-macos" else "/var/lib/teslatlas-hub/interop-package-receipts"
    if value["installation_receipt"] != receipt_root + "/" + cfg["package"]["sha256"] + ".json":
        raise SessionError("private session installation receipt path is not package-derived")
    if sys.version_info < (3, 9):
        raise SessionError("registered guest requires Python 3.9 or later")
    expected_version = tuple(int(part) for part in reg["guest"]["python_version"].split("-")[0].split("+")[0].split(".")[:2])
    if sys.version_info[:2] != expected_version:
        raise SessionError("running Python minor version differs from registration")
    invocation = Path(reg["guest"]["python"]["path"])
    if hashlib.sha256(invocation.read_bytes()).hexdigest() != reg["guest"]["python"]["sha256"]:
        raise SessionError("registered Python invocation bytes changed")
    process_path = Path(sys.executable)
    if str(process_path) != reg["guest"]["python_process"]["path"] or hashlib.sha256(process_path.read_bytes()).hexdigest() != reg["guest"]["python_process"]["sha256"]:
        raise SessionError("running Python process differs from registration")
    importlib.import_module(reg["guest"]["python_toml_module"])
    inputs = value["guest_inputs"]
    if not isinstance(inputs, dict) or set(inputs) != {"seed", "profile", "scenario", "package_manifest"}:
        raise SessionError("guest input mapping is invalid")
    for key in inputs:
        input_path = Path(inputs[key])
        expected_input = expected_root + "/profile/SHA256SUMS" if key == "profile" else expected_root + "/" + key
        if str(input_path) != expected_input:
            raise SessionError("guest input path escapes controller root")
        input_info = input_path.lstat()
        unsafe_mode = input_info.st_mode & (0o022 if key == "seed" else 0o077)
        if not stat.S_ISREG(input_info.st_mode) or unsafe_mode or input_info.st_size > MAX_INPUT_BYTES:
            raise SessionError("guest input is not a bounded private regular file")
        if key == "seed" and input_info.st_mode & 0o111 == 0:
            raise SessionError("guest seed input is not executable")
        if hashlib.sha256(input_path.read_bytes()).hexdigest() != cfg[key]["sha256"]:
            raise SessionError("guest input bytes differ from session binding")
    manifest = validate_package_manifest(json.loads(Path(inputs["package_manifest"]).read_text(encoding="utf-8")))
    if manifest["package_sha256"] != cfg["package"]["sha256"]:
        raise SessionError("guest package manifest differs from candidate package")
    try:
        registration_raw = base64.b64decode(value["registration_json_b64"], validate=True)
    except ValueError as error:
        raise SessionError("registration bytes are invalid") from error
    if hashlib.sha256(registration_raw).hexdigest() != value["registration_sha256"]:
        raise SessionError("registration bytes do not match pinned digest")
    if json.loads(registration_raw.decode("utf-8")) != reg:
        raise SessionError("embedded registration differs from pinned bytes")
    archive = Path(value["controller_root"]) / "controller.tar"
    archive_info = archive.lstat()
    if not stat.S_ISREG(archive_info.st_mode) or archive_info.st_mode & 0o077:
        raise SessionError("running controller archive is not a private regular file")
    if hashlib.sha256(archive.read_bytes()).hexdigest() != value["controller_bundle_sha256"] or value["controller_bundle_sha256"] != cfg["controller_bundle"]["sha256"]:
        raise SessionError("running controller bundle does not match pinned digest")
    return RegisteredConfig(cfg, reg, value["registration_sha256"]), value


def _platform(registered, private):
    if registered.registration["provider"] == "lima-debian":
        if __package__ in (None, ""):
            from installed_hosts.linux import LinuxController
        else:
            from .linux import LinuxController
        return LinuxController(registered, private)
    if __package__ in (None, ""):
        from installed_hosts.macos import MacOSController
    else:
        from .macos import MacOSController
    return MacOSController(registered, private)


def _write(value, deadline=None):
    raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
    if len(raw) > MAX_JSON_BYTES:
        raise SessionError("guest reply exceeds bounded frame")
    write_all(sys.stdout.fileno(), raw, deadline or Deadline(5))


def _replace_ownership(path, value):
    target = Path(path)
    temporary = target.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
    os.chmod(str(temporary), 0o600)
    with temporary.open("rb") as handle:
        os.fsync(handle.fileno())
    os.replace(str(temporary), str(target))
    directory = os.open(str(target.parent), os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    recovery = len(argv) == 4 and argv[0] in ("--verify-stopped", "--recover-stop") and argv[2] == "--budget-ms"
    ordinary = len(argv) == 2 and argv[0] == "--session"
    if not ordinary and not recovery:
        raise SessionError("usage: guest.py --session PRIVATE_JSON or (--verify-stopped|--recover-stop) PRIVATE_JSON --budget-ms N")
    recovery_deadline = None
    if recovery:
        if not argv[3].isdigit() or argv[3].startswith("0"):
            raise SessionError("private recovery budget_ms is invalid")
        budget_ms = int(argv[3])
        if not 0 < budget_ms <= 45_000:
            raise SessionError("private recovery budget_ms exceeds fixed maximum")
        recovery_deadline = Deadline(budget_ms / 1000)
    registered, private = _load_private(argv[1])
    platform = _platform(registered, private)
    if argv[0] in ("--verify-stopped", "--recover-stop"):
        recovery_deadline.remaining()
        if hasattr(platform, "set_deadline"):
            platform.set_deadline(recovery_deadline)
        ownership = json.loads(Path(private["ownership_path"]).read_text(encoding="utf-8")) if Path(private["ownership_path"]).exists() else None
        if argv[0] == "--recover-stop":
            if ownership is None or ownership.get("session_id") != registered.config["session_id"] or ownership.get("lease") != registered.registration["lease"]:
                raise SessionError("recovery ownership record is absent or foreign")
            if registered.registration["lease"]["expires_at_unix"] <= int(time.time() + recovery_deadline.remaining()):
                raise SessionError("recovery lease does not cover bounded stop")
            platform.bind_recovery(ownership)
            def record_recovery_acquisition(service):
                current = dict(ownership.get("service") or {})
                current.update(service)
                ownership["service"] = current
                if hasattr(platform, "owned_acquisition"):
                    platform.owned_acquisition = dict(current)
                _replace_ownership(private["ownership_path"], ownership)
            if hasattr(platform, "acquisition_callback"):
                platform.acquisition_callback = record_recovery_acquisition
            if ownership.get("stop_evidence") is None and hasattr(platform, "reconcile_owned"):
                reconciled = platform.reconcile_owned(ownership)
                if not isinstance(reconciled, dict) or not reconciled:
                    raise SessionError("recovery pending acquisition could not be reconciled")
                record_recovery_acquisition(reconciled)
            platform.assert_owned(ownership)
            if ownership.get("stop_evidence") is None:
                stop_evidence = platform.stop()
                service = ownership.get("service")
                if not isinstance(service, dict) or not service:
                    raise SessionError("recovery stop lacks an acquired service generation")
                stop_evidence = dict(stop_evidence)
                operation = ownership.get("active_operation") or ownership
                stop_evidence.update({"operation_sequence":operation.get("sequence"),"operation_challenge":operation.get("challenge"),"acquired_service_sha256":hashlib.sha256(json.dumps(service,sort_keys=True,separators=(",", ":")).encode()).hexdigest()})
                ownership["stop_evidence"] = stop_evidence
                ownership["state"] = "stopped"
            _replace_ownership(private["ownership_path"], ownership)
        stopped=platform.verify_stopped(owned=ownership, initial=ownership is None)
        if argv[0] == "--recover-stop":
            ownership["absence_evidence"]={"observed_at_monotonic_ns":time.monotonic_ns(),"proof_sha256":hashlib.sha256(json.dumps(stopped,sort_keys=True,separators=(",", ":")).encode()).hexdigest()}
            _replace_ownership(private["ownership_path"],ownership)
        _write(stopped, recovery_deadline)
        return 0
    journal = DurableJournal(Path(private["controller_root"]) / "guest.journal.jsonl")
    controller = GuestController(registered, platform, journal=journal)
    timer = threading.Timer(registered.config["lifetime_seconds"], lambda: os.kill(os.getpid(), signal.SIGTERM))
    timer.daemon = True
    def interrupted(_signal, _frame):
        raise SystemExit(1)
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    timer.start()
    status = 0
    try:
        _write(controller.open())
        reader = IncrementalLineReader(sys.stdin.fileno(), MAX_JSON_BYTES)
        lifetime_deadline = Deadline(registered.config["lifetime_seconds"])
        while True:
            # Idle time belongs to the registered cell lifetime. The bounded
            # complete-frame allowance starts only when the next byte exists.
            reader.wait_readable(lifetime_deadline)
            raw = reader.read(Deadline(160))
            if not raw:
                break
            if len(raw) > MAX_JSON_BYTES or not raw.endswith(b"\n"):
                raise SessionError("guest request exceeds bounded frame")
            request = json.loads(raw.decode("utf-8"))
            if not isinstance(request, dict) or type(request.get("budget_ms")) is not int or request["budget_ms"] <= 0:
                raise SessionError("guest request lacks private remaining budget")
            operation_deadline = Deadline(request["budget_ms"] / 1000)
            _write(controller.handle_advance_once(request, operation_deadline) if request.get("op") == "advance-once" else controller.handle(request, operation_deadline), operation_deadline)
    except BaseException as error:
        status = 1
        try:
            _write({"schema_version": 1, "type": "error", "session_id": registered.config["session_id"], "sequence": controller.sequence, "challenge": secrets.token_hex(32), "error": {"code": "controller-failed"}})
        except BaseException:
            pass
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            status = 1
    finally:
        timer.cancel()
        _stopped, errors = controller.close()
        journal.close()
        if errors:
            status = 1
    return status


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BaseException as error:
        print("installed guest controller failed: {}".format(type(error).__name__), file=sys.stderr)
        sys.exit(1)
