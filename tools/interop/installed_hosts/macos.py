# SPDX-License-Identifier: AGPL-3.0-only
"""Fixed packaged macOS LaunchAgent observation and control primitives."""
import ctypes
import json
import os
import plistlib
import re
import signal
import struct
import threading
import time
from pathlib import Path

from ._common import (
    loads_toml, normal_tls_probe, process_tree_digest, run_checked, sha256_bytes,
    validate_scenario_bytes,
    sha256_file, validate_synthetic_config,
    verify_payload_members,
)
from .bounded import Deadline


LABEL = "com.teslatlas.hub"
WRAPPER = "/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh"
EXECUTABLE = "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
APP_EXECUTABLE = "/Applications/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub"


class ProcessAbsent(RuntimeError):
    pass


def command_plan(operation, loaded, uid, plist):
    if isinstance(uid, bool) or not isinstance(uid, int) or uid < 1:
        raise ValueError("invalid console UID")
    target = "gui/{}/{}".format(uid, LABEL)
    domain = "gui/{}".format(uid)
    if operation == "stop":
        return [["/bin/launchctl", "bootout", target]] if loaded else []
    if operation == "start":
        return [["/bin/launchctl", "kickstart", target]] if loaded else [["/bin/launchctl", "bootstrap", domain, plist]]
    raise ValueError("unsupported fixed LaunchAgent operation")


def validate_wrapper_child(wrapper, hub, direct_children):
    if wrapper.get("executable_path") != "/bin/bash":
        raise ValueError("wrapper does not resolve to the fixed macOS system shell process")
    if hub.get("executable_path") != EXECUTABLE or hub.get("parent_pid") != wrapper.get("pid"):
        raise ValueError("Hub is not a direct packaged wrapper child")
    if direct_children != [hub.get("pid")]:
        raise ValueError("expected a single direct Hub child")
    return True


def _procargs(pid):
    libc = ctypes.CDLL(None, use_errno=True)
    mib = (ctypes.c_int * 3)(1, 49, pid)  # CTL_KERN, KERN_PROCARGS2, pid
    size = ctypes.c_size_t(0)
    if libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0) != 0 or size.value > 262_144:
        raise RuntimeError("cannot size bounded kernel argv observation")
    buffer = ctypes.create_string_buffer(size.value)
    if libc.sysctl(mib, 3, buffer, ctypes.byref(size), None, 0) != 0:
        raise RuntimeError("cannot read kernel argv observation")
    raw = buffer.raw[: size.value]
    argc = struct.unpack_from("i", raw, 0)[0]
    if argc < 1 or argc > 64:
        raise RuntimeError("kernel argv count is invalid")
    offset = 4
    while offset < len(raw) and raw[offset] != 0:
        offset += 1
    while offset < len(raw) and raw[offset] == 0:
        offset += 1
    argv = []
    for _ in range(argc):
        end = raw.find(b"\0", offset)
        if end < 0:
            raise RuntimeError("kernel argv is truncated")
        argv.append(raw[offset:end].decode("utf-8"))
        offset = end + 1
    return argv


def _process(pid, expected_argv=None, deadline=None):
    deadline = deadline or Deadline(10)
    deadline.remaining()
    libc = ctypes.CDLL(None, use_errno=True)
    path_buffer = ctypes.create_string_buffer(4096)
    if libc.proc_pidpath(pid, path_buffer, len(path_buffer)) <= 0:
        if ctypes.get_errno() == 3:
            raise ProcessAbsent("process is absent")
        raise RuntimeError("cannot observe process executable path")
    executable = path_buffer.value.decode("utf-8")
    info = ctypes.create_string_buffer(136)
    if libc.proc_pidinfo(pid, 3, 0, info, len(info)) < 136:  # PROC_PIDTBSDINFO
        if ctypes.get_errno() == 3:
            raise ProcessAbsent("process is absent")
        raise RuntimeError("cannot observe precise process generation")
    raw = info.raw
    observed_pid = struct.unpack_from("I", raw, 12)[0]
    parent_pid = struct.unpack_from("I", raw, 16)[0]
    uid = struct.unpack_from("I", raw, 20)[0]
    start_sec = struct.unpack_from("Q", raw, 120)[0]
    start_usec = struct.unpack_from("Q", raw, 128)[0]
    if observed_pid != pid or start_sec == 0:
        raise RuntimeError("kernel process identity changed while reading")
    argv = _procargs(pid)
    if expected_argv is not None and argv != list(expected_argv):
        raise RuntimeError("kernel argv differs from fixed packaged definition")
    boot = run_checked(["/usr/sbin/sysctl", "-n", "kern.boottime"], 10, deadline=deadline).stdout.strip()
    deadline.remaining()
    identity = {
        "pid": pid, "uid": uid, "parent_pid": parent_pid, "boot_id": sha256_bytes(boot),
        "start_identity": "{}:{:06d}".format(start_sec, start_usec), "executable_path": executable,
        "executable_sha256": sha256_file(executable),
        "argv_sha256": sha256_bytes(b"\0".join(item.encode("utf-8") for item in argv) + b"\0"),
    }
    deadline.remaining()
    return identity


def _generation_present(identity):
    libc = ctypes.CDLL(None, use_errno=True)
    info = ctypes.create_string_buffer(136)
    ctypes.set_errno(0)
    if libc.proc_pidinfo(identity["pid"], 3, 0, info, len(info)) < 136:
        if ctypes.get_errno() == 3:
            return False
        raise RuntimeError("cannot observe owned process during stop")
    raw = info.raw
    observed_pid = struct.unpack_from("I", raw, 12)[0]
    uid = struct.unpack_from("I", raw, 20)[0]
    start = "{}:{:06d}".format(struct.unpack_from("Q", raw, 120)[0], struct.unpack_from("Q", raw, 128)[0])
    if observed_pid != identity["pid"]:
        raise RuntimeError("owned PID identity changed during stop")
    return uid == identity["uid"] and start == identity["start_identity"]


def _observe_absent_before(identity, deadline_ns, clock=None, present=None, pause=None):
    """Return the post-observation timestamp only when it is strictly in-window."""
    clock = clock or time.monotonic_ns
    present = present or _generation_present
    pause = pause or time.sleep
    while clock() < deadline_ns:
        is_present = present(identity)
        observed_ns = clock()
        if not is_present and observed_ns < deadline_ns:
            return observed_ns
        remaining = (deadline_ns - observed_ns) / 1_000_000_000
        if remaining <= 0:
            return None
        pause(min(0.05, remaining))
    return None


def _local_stop_is_normal(service, stop, session_id):
    """Guest-side validation before publishing a normal launchd stop."""
    if isinstance(stop,dict) and {"operation_sequence","operation_challenge","acquired_service_sha256"}.issubset(stop):
        stop={key:value for key,value in stop.items() if key not in {"operation_sequence","operation_challenge","acquired_service_sha256"}}
    if not isinstance(service, dict) or not isinstance(stop, dict) or stop.get("operation") != "stop" or stop.get("session_id") != session_id or stop.get("normal_exit") is not True or stop.get("normal_exit_code") != "unavailable":
        return False
    predicate = stop.get("normal_exit_predicate")
    if predicate == "no-hub-generation-acquired-app-absence":
        keys = {"operation","session_id","hub","app","transient_helpers","normal_exit","normal_exit_code","normal_exit_predicate","stop_requested_monotonic_ns","app_absent_monotonic_ns"}
        requested, absent = stop.get("stop_requested_monotonic_ns"), stop.get("app_absent_monotonic_ns")
        return set(stop) == keys and service.get("acquisition_state") == "app-acquired" and service.get("hub") is None and service.get("supervisor") is None and stop.get("hub") is None and stop.get("app") == service.get("app_process") and stop.get("transient_helpers") == [] and type(requested) is int and type(absent) is int and absent >= requested
    keys = {"operation","session_id","wrapper","hub","app","transient_helpers","normal_exit","normal_exit_code","normal_exit_predicate","stop_requested_monotonic_ns","hub_absent_monotonic_ns","elapsed_to_hub_absence_ns","escalation_boundary_ns","accepted_deadline_ns","wrapper_helper_cleanup_cap_ns","cleanup_absence_observations"}
    if predicate != "hub-child-pre-escalation-generation-absence" or set(stop) != keys or stop.get("wrapper") != service.get("supervisor") or stop.get("hub") != service.get("hub") or stop.get("app") != service.get("app_process") or stop.get("transient_helpers") != service.get("transient_helpers", []):
        return False
    integer_fields = ("stop_requested_monotonic_ns","hub_absent_monotonic_ns","elapsed_to_hub_absence_ns","escalation_boundary_ns","accepted_deadline_ns","wrapper_helper_cleanup_cap_ns")
    if any(type(stop.get(key)) is not int for key in integer_fields):
        return False
    requested, absent = stop["stop_requested_monotonic_ns"], stop["hub_absent_monotonic_ns"]
    if requested < 0 or absent < requested or stop["elapsed_to_hub_absence_ns"] != absent-requested or stop["escalation_boundary_ns"] != 2_000_000_000 or stop["accepted_deadline_ns"] != 1_800_000_000 or stop["wrapper_helper_cleanup_cap_ns"] != 30_000_000_000 or absent >= requested+stop["accepted_deadline_ns"]:
        return False
    expected = [("app",service.get("app_process")),("wrapper",service.get("supervisor"))] + [("transient-helper",value) for value in service.get("transient_helpers",[])]
    observations = stop.get("cleanup_absence_observations")
    if not isinstance(observations,list) or len(observations)!=len(expected):
        return False
    cap=requested+stop["wrapper_helper_cleanup_cap_ns"]
    return all(isinstance(row,dict) and set(row)=={"role","identity","present","observed_monotonic_ns"} and row["role"]==role and row["identity"]==identity and row["present"] is False and type(row["observed_monotonic_ns"]) is int and requested<=row["observed_monotonic_ns"]<=cap for row,(role,identity) in zip(observations,expected))


def validate_wrapper_cleanup(service, stop, session_id):
    raw = {key: value for key, value in stop.items() if key not in {"operation_sequence", "operation_challenge", "acquired_service_sha256"}}
    keys = {"operation", "session_id", "normal_exit", "cleanup_only", "normal_exit_predicate", "wrapper", "hub", "app", "transient_helpers", "stop_requested_monotonic_ns", "cleanup_absence_observations"}
    if set(raw) != keys or raw["operation"] != "stop" or raw["session_id"] != session_id or raw["normal_exit"] is not False or raw["cleanup_only"] is not True or raw["normal_exit_predicate"] != "wrapper-only-generation-absence":
        return False
    if service.get("acquisition_state") != "wrapper-acquired" or service.get("hub") is not None or raw["hub"] is not None or raw["wrapper"] != service.get("supervisor") or raw["app"] != service.get("app_process") or raw["transient_helpers"] != service.get("transient_helpers", []):
        return False
    expected = [("wrapper", service.get("supervisor")), ("app", service.get("app_process"))] + [("transient-helper", h) for h in service.get("transient_helpers", [])]
    expected = [(role, identity) for role, identity in expected if identity is not None]
    requested = raw["stop_requested_monotonic_ns"]
    rows = raw["cleanup_absence_observations"]
    return type(requested) is int and requested >= 0 and isinstance(rows, list) and len(rows) == len(expected) and all(
        isinstance(row, dict) and set(row) == {"role", "identity", "present", "observed_monotonic_ns"} and row["role"] == role and row["identity"] == identity and row["present"] is False and type(row["observed_monotonic_ns"]) is int and requested <= row["observed_monotonic_ns"] <= requested + 30_000_000_000
        for row, (role, identity) in zip(rows, expected))


class MacOSController:
    def __init__(self, registered, private):
        self.registered = registered
        self.cfg = registered.config
        self.reg = registered.registration
        self.private = private
        self.uid = self.reg["ssh"]["login_uid"]
        self.home = "/Users/" + self.reg["ssh"]["user"]
        self.data_root = self.home + "/Library/Application Support/Teslatlas Hub/InteropMatrix/" + self.cfg["guest_run_id"]
        self.config_path = self.home + "/Library/Application Support/Teslatlas Hub/config.toml"
        self.plist_path = self.home + "/Library/LaunchAgents/" + LABEL + ".plist"
        self.connection = None
        self.app_process = None
        self._prepared = False
        self._last_stop = None
        self.deadline = None
        self.transient_helpers = []
        self.acquisition_callback = None
        self.context_callback = None
        self.last_context = None
        self.expected_context = None
        self.owned_acquisition = None

    def _process(self, pid, expected_argv=None):
        return _process(pid, expected_argv, deadline=self.deadline)

    def set_deadline(self, deadline):
        self.deadline = deadline

    def _run(self, argv, timeout=15, allowed_status=(0,)):
        return run_checked(argv, timeout, allowed_status, self.deadline)

    def _launch_status(self, required=True):
        target = "gui/{}/{}".format(self.uid, LABEL)
        result = self._run(["/bin/launchctl", "print", target], 10, (0, 3, 113))
        if result.returncode != 0:
            if required:
                raise RuntimeError("LaunchAgent is not loaded")
            return None
        raw = result.stdout.decode("utf-8")
        values = re.findall(r"^\s*pid = ([1-9][0-9]*)\s*$", raw, re.MULTILINE)
        if len(values) != 1:
            if not required and not values:
                return None, raw
            raise RuntimeError("LaunchAgent output lacks one wrapper PID")
        return int(values[0]), raw

    def _launch_app_once(self):
        before = set()
        result = self._run(["/usr/bin/pgrep", "-x", "Teslatlas Hub"], 10, (0, 1))
        if result.returncode == 0:
            before = {int(line) for line in result.stdout.splitlines()}
        self._run(["/usr/bin/open", "-a", "/Applications/Teslatlas Hub.app"], 15)
        deadline = min(time.monotonic() + 15, self.deadline.end if self.deadline is not None else float("inf"))
        process = None
        while time.monotonic() < deadline:
            found = self._run(["/usr/bin/pgrep", "-x", "Teslatlas Hub"], 10, (0, 1))
            after = {int(line) for line in found.stdout.splitlines()} if found.returncode == 0 else set()
            created = after - before
            if len(created) == 1:
                process = self._process(created.pop())
                self.app_process = process
                if self.acquisition_callback is not None:
                    self.acquisition_callback({"acquisition_state": "app-acquired", "app_process": process})
                break
            time.sleep(0.1)
        if process is None or process["executable_path"] != APP_EXECUTABLE or process["uid"] != self.uid:
            raise RuntimeError("installed App launch was not uniquely observed")
        current = self._process(process["pid"])
        if current != process:
            raise RuntimeError("installed App generation changed before stop")
        os.kill(process["pid"], signal.SIGTERM)
        deadline = min(time.monotonic() + 10, self.deadline.end if self.deadline is not None else float("inf"))
        while time.monotonic() < deadline:
            try:
                os.kill(process["pid"], 0)
            except ProcessLookupError:
                return
            time.sleep(0.1)
        raise RuntimeError("installed App launched by this session did not stop normally")

    def prepare(self):
        if self._prepared:
            raise RuntimeError("cell preparation is single-use")
        root = Path(self.data_root)
        run_root = root.parent
        for ancestor in (run_root.parent, run_root):
            if ancestor.is_symlink():
                raise RuntimeError("macOS seed ancestor is symlinked")
            ancestor.mkdir(mode=0o700, parents=True, exist_ok=True)
            info = ancestor.lstat()
            if not info.st_mode & 0o170000 == 0o040000 or info.st_uid != self.uid or info.st_mode & 0o077:
                raise RuntimeError("macOS seed ancestor owner or mode differs")
        if root.exists() or root.is_symlink():
            raise RuntimeError("fresh macOS matrix root already exists")
        seed = self.private["guest_inputs"]["seed"]
        if sha256_file(seed) != self.cfg["seed"]["sha256"]:
            raise RuntimeError("guest seed bytes do not match session")
        validate_scenario_bytes(Path(self.private["guest_inputs"]["scenario"]).read_bytes())
        output = self._run([seed, "--output", self.data_root, "--port", "18480"], 60)
        prepared = json.loads(output.stdout.decode("utf-8"))
        expected_config = self.data_root + "/config.toml"
        if prepared.get("config_path") != expected_config or prepared.get("certificate_path") != self.data_root + "/server.pem":
            raise RuntimeError("target-native seed returned unexpected paths")
        scenario_target = root / "scenario.json"
        scenario_target.write_bytes(Path(self.private["guest_inputs"]["scenario"]).read_bytes())
        os.chmod(str(scenario_target), 0o600)
        validate_scenario_bytes(scenario_target.read_bytes())
        config_raw = Path(expected_config).read_bytes()
        config_raw += b"\n[http]\nallowed_origins = " + json.dumps(self.cfg["allowed_origins"], separators=(",", ":")).encode("utf-8") + b"\n"
        target = Path(self.config_path)
        if target.exists() and target.is_symlink():
            raise RuntimeError("fixed packaged config path is a symlink")
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        staged = target.parent / (target.name + ".installing-" + self.cfg["session_id"])
        staged.write_bytes(config_raw)
        os.chmod(str(staged), 0o600)
        os.replace(str(staged), str(target))
        ca_copy = Path(self.private["controller_root"]) / "public-ca.pem"
        ca_copy.write_bytes(Path(self.data_root + "/server.pem").read_bytes())
        os.chmod(str(ca_copy), 0o600)
        self.connection = prepared
        self.pin_current_context()
        self._launch_app_once()
        loaded = self._launch_status(required=False)
        if loaded is not None:
            self.stop()
        self._prepared = True
        manifest = json.loads(Path(self.private["guest_inputs"]["package_manifest"]).read_text(encoding="utf-8"))
        schema = int(self._run([self.reg["guest"]["python"]["path"], "-B", "-c", "import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); print(c.execute('pragma user_version').fetchone()[0])", self.data_root + "/hub/hub.sqlite"], 10).stdout)
        if prepared.get("hub_id") is None or schema != manifest["store_schema_version"] or sha256_file(scenario_target) != self.cfg["scenario"]["sha256"]:
            raise RuntimeError("seeded store identity/schema/scenario differs")
        self.current_context()
        expired = self.pair(1)
        invitation = self.pair(900)
        expires = expired.get("expiresAtMs")
        if type(expires) is int:
            while int(time.time() * 1000) <= expires:
                time.sleep(0.05)
        return {"invitation": invitation, "expired_invitation": expired, "seed_facts": {"store_id": prepared["hub_id"], "store_schema_version": schema, "scenario_sha256": self.cfg["scenario"]["sha256"]}}

    def cleanup_partial(self):
        if self.app_process is not None and _generation_present(self.app_process):
            os.kill(self.app_process["pid"], signal.SIGTERM)
            deadline = min(time.monotonic() + 8, self.deadline.end if self.deadline is not None else float("inf"))
            while time.monotonic() < deadline:
                if not _generation_present(self.app_process):
                    return
                time.sleep(0.05)
            raise RuntimeError("session-owned App survived bounded cleanup")

    def preflight(self):
        ioreg = self._run(["/usr/sbin/ioreg", "-rd1", "-c", "IOPlatformExpertDevice"], 10).stdout
        version = self._run(["/usr/bin/sw_vers", "-productVersion"], 10).stdout.decode("ascii").strip()
        architecture = self._run(["/usr/bin/uname", "-m"], 10).stdout.decode("ascii").strip()
        uid = os.getuid()
        if sha256_bytes(ioreg) != self.reg["guest"]["machine_identity_sha256"] or version.split(".")[0] != "13" or architecture != "arm64" or uid != self.uid:
            raise RuntimeError("macOS current host identity differs from registration")
        self._receipt()
        self.verify_stopped(initial=True)
        return {"machine_identity_sha256": sha256_bytes(ioreg), "os_version": version, "architecture": architecture, "service_uid": uid, "stopped": True}

    def assert_owned(self, ownership):
        if ownership.get("session_id") != self.cfg["session_id"] or ownership.get("lease") != self.reg["lease"] or ownership.get("config_path") != self.config_path:
            raise RuntimeError("macOS mutation ownership record is foreign")
        service = ownership.get("service") or {}
        current = self.current_context(ownership)
        if ownership.get("store_id") != current["store_id"]:
            raise RuntimeError("macOS mutation store identity changed")
        if ownership.get("state") == "stopped":
            self.verify_stopped(owned=ownership)
            return
        if service.get("acquisition_state") == "app-acquired" and service.get("app_process") is not None and service.get("supervisor") is None and service.get("hub") is None:
            if self._launch_status(required=False) is not None or not _generation_present(service["app_process"]):
                raise RuntimeError("macOS partial App acquisition is no longer owned")
            self.app_process = service["app_process"]
            return
        status = self._launch_status()
        wrapper = service.get("supervisor")
        hub = service.get("hub")
        if service.get("acquisition_state") == "wrapper-acquired" and hub is None:
            if wrapper is None or status[0] != wrapper.get("pid") or self._process(wrapper["pid"]) != wrapper:
                raise RuntimeError("macOS owned wrapper generation was replaced")
            if self._hub_child(wrapper["pid"], [EXECUTABLE, "--config", self.config_path, "serve"], allow_missing=True) is not None:
                raise RuntimeError("wrapper-only ownership acquired an unrecorded Hub")
            return
        if wrapper is None or hub is None or status[0] != wrapper.get("pid") or self._process(wrapper["pid"]) != wrapper or self._process(hub["pid"]) != hub:
            raise RuntimeError("macOS owned service generation was replaced")

    def bind_recovery(self, ownership):
        self.connection = {"hub_id": ownership.get("store_id")}
        self.expected_context = dict(ownership.get("config") or {})
        service = ownership.get("service") or {}
        self.app_process = service.get("app_process")
        self.transient_helpers = list(service.get("transient_helpers", []))
        self.owned_acquisition = dict(service)

    def reconcile_owned(self, ownership):
        service = ownership.get("service") or {}
        state = service.get("acquisition_state")
        self.owned_acquisition = dict(service)
        self.app_process = service.get("app_process")
        self.transient_helpers = list(service.get("transient_helpers", []))
        if state == "complete" or state == "app-acquired":
            return dict(service)
        self.current_context(ownership)
        if state == "pending-start":
            if service.get("target") != "gui/{}/{}".format(self.uid, LABEL) or service.get("launchagent_unloaded_before") is not True or type(service.get("intent_monotonic_ns")) is not int:
                raise RuntimeError("macOS pending acquisition target is foreign")
            self.app_process = service.get("app_process")
            status=self._launch_status(required=False)
            if status is None:
                if self.app_process is not None and _generation_present(self.app_process):
                    return {"acquisition_state":"app-acquired","app_process":self.app_process}
                raise RuntimeError("macOS pending start has no exact owned generation")
            wrapper=self._process(status[0])
            partial=dict(service); partial.update({"acquisition_state":"wrapper-acquired","supervisor":wrapper})
            if self.acquisition_callback is not None: self.acquisition_callback(partial)
            hub=self._hub_child(wrapper["pid"],[EXECUTABLE,"--config",self.config_path,"serve"])
            result=dict(partial); result.update({"acquisition_state":"complete","hub":hub,"transient_helpers":list(self.transient_helpers)})
            if self.acquisition_callback is not None: self.acquisition_callback(result)
            return result
        if state == "wrapper-acquired":
            wrapper = service.get("supervisor")
            status = self._launch_status()
            if wrapper is None or status[0] != wrapper.get("pid") or self._process(wrapper["pid"]) != wrapper:
                raise RuntimeError("macOS pending wrapper generation was replaced")
            self.app_process = service.get("app_process")
            hub = self._hub_child(wrapper["pid"], [EXECUTABLE,"--config",self.config_path,"serve"], allow_missing=True)
            if hub is None:
                return dict(service)
            result=dict(service); result.update({"acquisition_state":"complete","supervisor":wrapper,"hub":hub,"app_process":self.app_process,"transient_helpers":list(self.transient_helpers)})
            if self.acquisition_callback is not None: self.acquisition_callback(result)
            return result
        raise RuntimeError("macOS acquisition state cannot be reconciled")

    def pin_current_context(self):
        context = self.current_context()
        self.expected_context = dict(context)
        if self.context_callback is not None:
            self.context_callback(dict(context))
        return context

    def pair(self, seconds=900):
        if type(seconds) is not int or seconds not in (1, 900):
            raise ValueError("pair duration is not a fixed controller value")
        self.current_context()
        result = self._run([EXECUTABLE, "--config", self.config_path, "pair", "--json", "--label", "Owned matrix lane", "--expires-in-seconds", str(seconds)], 60)
        value = json.loads(result.stdout.decode("utf-8"))
        if not isinstance(value, dict) or "secret" not in value:
            raise RuntimeError("installed CLI returned invalid invitation")
        return value

    def paired_device_ids(self):
        self.current_context()
        values = json.loads(self._run([EXECUTABLE, "--config", self.config_path, "control", "paired-devices"], 30).stdout.decode("utf-8"))
        if not isinstance(values, list):
            raise RuntimeError("installed CLI paired-device output is invalid")
        ids = set()
        for value in values:
            device = value.get("device_id", value.get("deviceId")) if isinstance(value, dict) else None
            if not isinstance(device, str):
                raise RuntimeError("installed CLI paired-device row is invalid")
            ids.add(device)
        return ids

    def revoke(self, device_id):
        self.current_context()
        self._run([EXECUTABLE, "--config", self.config_path, "control", "revoke-device", device_id], 60)

    def advance_once(self):
        self.current_context()
        database = self.data_root + "/hub/hub.sqlite"
        before = sha256_file(database)
        self._run([self.private["guest_inputs"]["seed"], "--advance", self.data_root], 60)
        after = sha256_file(database)
        return {"before_store_sha256": before, "after_store_sha256": after, "scenario_sha256": sha256_file(self.private["guest_inputs"]["scenario"]), "seed_sha256": sha256_file(self.private["guest_inputs"]["seed"])}

    def start(self):
        loaded = self._launch_status(required=False) is not None
        if loaded:
            raise RuntimeError("LaunchAgent start requires an unloaded target")
        intent_ns=time.monotonic_ns()
        pending={"acquisition_state": "pending-start", "target": "gui/{}/{}".format(self.uid, LABEL), "intent_monotonic_ns":intent_ns, "launchagent_unloaded_before":True, "app_process": self.app_process}
        if self.acquisition_callback is not None:
            self.acquisition_callback(pending)
        for argv in command_plan("start", loaded, self.uid, self.plist_path):
            try:
                self._run(argv, 30)
            except BaseException:
                try:
                    if self._launch_status(required=False) is not None:
                        self.reconcile_owned({"service":pending,"config":self.expected_context,"store_schema_version":(self.expected_context or {}).get("store_schema_version")})
                except BaseException:
                    pass
                raise
        deadline = self.deadline or Deadline(30)
        while True:
            try:
                self._launch_status()
                self.reconcile_owned({"service":pending,"config":self.expected_context,"store_schema_version":(self.expected_context or {}).get("store_schema_version")})
                return
            except RuntimeError:
                time.sleep(min(0.1, deadline.remaining()))

    def _hub_child(self, wrapper_pid, hub_expected, allow_missing=False):
        result = self._run(["/usr/bin/pgrep", "-P", str(wrapper_pid)], 10, (0, 1))
        stable = []
        transient = []
        for raw_pid in result.stdout.splitlines():
            pid = int(raw_pid)
            try:
                child = self._process(pid)
            except ProcessAbsent:
                continue
            if child["parent_pid"] != wrapper_pid or child["uid"] != self.uid:
                raise RuntimeError("LaunchAgent child identity is foreign")
            if child["executable_path"] == EXECUTABLE:
                stable.append(self._process(pid, hub_expected))
            else:
                transient.append(child)
        for identity in transient:
            if identity not in self.transient_helpers:
                self.transient_helpers.append(identity)
        if self.acquisition_callback is not None and self.owned_acquisition:
            self.acquisition_callback({"transient_helpers": list(self.transient_helpers)})
        if not stable and allow_missing and not transient:
            return None
        if len(stable) != 1:
            raise RuntimeError("packaged wrapper lacks one direct Hub child")
        # The reviewed wrapper invokes short-lived system helpers while it
        # supervises Hub. No second child generation may persist as a service.
        if transient:
            if self.deadline is not None and self.deadline.remaining() < 1.2:
                raise TimeoutError("whole operation budget cannot cover helper stability observation")
            time.sleep(1.2)
            for child in transient:
                try:
                    current = self._process(child["pid"])
                except ProcessAbsent:
                    continue
                if current == child:
                    raise RuntimeError("packaged wrapper has an unexpected persistent child")
        return stable[0]

    def capture_service(self, publish=True):
        wrapper_pid, _ = self._launch_status()
        wrapper = self._process(wrapper_pid)
        if publish and self.acquisition_callback is not None:
            self.acquisition_callback({"acquisition_state": "wrapper-acquired", "supervisor": wrapper, "app_process": self.app_process})
        hub = self._hub_child(wrapper_pid, [EXECUTABLE, "--config", self.config_path, "serve"])
        value = {"acquisition_state": "complete", "supervisor": wrapper, "hub": hub, "app_process": self.app_process, "transient_helpers": list(self.transient_helpers)}
        if publish and self.acquisition_callback is not None:
            self.acquisition_callback(value)
        return value

    def stop(self):
        owned = self.owned_acquisition or {}
        if owned.get("acquisition_state") == "wrapper-acquired" and owned.get("hub") is None:
            return self._stop_wrapper_only(owned)
        status = self._launch_status(required=False)
        loaded = status is not None
        hub = None
        acquired = None
        if loaded and status[0] is not None:
            wrapper = self._process(status[0])
            hub = self._hub_child(status[0], [EXECUTABLE, "--config", self.config_path, "serve"])
            for role, identity in (("supervisor", wrapper), ("hub", hub)):
                if owned.get(role) is not None and owned[role] != identity:
                    raise RuntimeError("macOS stop generation differs from canonical acquisition")
            acquired=dict(owned)
            acquired.update({"acquisition_state":"complete","supervisor":wrapper,"hub":hub,"app_process":self.app_process,"transient_helpers":list(self.transient_helpers)})
            if self.acquisition_callback is not None:
                self.acquisition_callback(acquired)
        if not loaded and self.app_process is not None:
            acquired={"acquisition_state":"app-acquired","app_process":self.app_process}
            requested = time.monotonic_ns()
            if _generation_present(self.app_process):
                os.kill(self.app_process["pid"], signal.SIGTERM)
                deadline = self.deadline or Deadline(10)
                while _generation_present(self.app_process):
                    time.sleep(min(0.05, deadline.remaining()))
            observed = time.monotonic_ns()
            self._last_stop = {"operation":"stop","session_id":self.cfg["session_id"],"hub":None,"app":self.app_process,"transient_helpers":[],"normal_exit":True,"normal_exit_code":"unavailable","normal_exit_predicate":"no-hub-generation-acquired-app-absence","stop_requested_monotonic_ns":requested,"app_absent_monotonic_ns":observed}
        for argv in command_plan("stop", loaded, self.uid, self.plist_path):
            if hub is None:
                self._run(argv, 30)
                continue
            if self.app_process is None:
                raise RuntimeError("owned App generation is absent from stop context")
            stop_requested_ns = time.monotonic_ns()
            normal_deadline_ns = stop_requested_ns + 1_800_000_000
            if self.deadline is not None:
                self.deadline.remaining()
                normal_deadline_ns = min(normal_deadline_ns, int(self.deadline.end * 1_000_000_000))
            command_result = []
            command_error = []
            def issue_stop():
                try:
                    elapsed = (time.monotonic_ns() - stop_requested_ns) / 1_000_000_000
                    command_result.append(self._run(argv, max(0.001, 30 - elapsed)))
                except BaseException as error:
                    command_error.append(error)
            command = threading.Thread(target=issue_stop, name="installed-host-launchctl-stop")
            command.start()
            hub_absent_ns = _observe_absent_before(hub, normal_deadline_ns)
            join_until = min(stop_requested_ns / 1_000_000_000 + 30, self.deadline.end if self.deadline is not None else float("inf"))
            command.join(timeout=max(0.0, join_until - time.monotonic()))
            if command.is_alive():
                raise RuntimeError("LaunchAgent bootout exceeded its command bound")
            if command_error:
                raise command_error[0]
            if len(command_result) != 1:
                raise RuntimeError("LaunchAgent bootout result is absent")
            if hub_absent_ns is None:
                raise RuntimeError("Hub did not exit before the packaged wrapper escalation boundary")
            cleanup_deadline_ns = stop_requested_ns + 30_000_000_000
            if self.deadline is not None:
                cleanup_deadline_ns = min(cleanup_deadline_ns, int(self.deadline.end * 1_000_000_000))
            app_observed_ns = _observe_absent_before(self.app_process, cleanup_deadline_ns)
            if app_observed_ns is None:
                raise RuntimeError("owned App generation survived stop")
            cleanup_observations = [{"role": "app", "identity": self.app_process, "present": False, "observed_monotonic_ns": app_observed_ns}]
            for identity in [wrapper] + list(self.transient_helpers):
                observed_ns = _observe_absent_before(identity, cleanup_deadline_ns)
                cleanup_observations.append({"role": "wrapper" if identity == wrapper else "transient-helper", "identity": identity, "present": observed_ns is None, "observed_monotonic_ns": observed_ns})
                if observed_ns is None:
                    raise RuntimeError("owned wrapper/helper survived finite cleanup cap")
            if self.deadline is not None:
                self.deadline.remaining()
            self._last_stop = {
                "operation": "stop", "session_id": self.cfg["session_id"], "wrapper": wrapper, "hub": hub, "app": self.app_process, "transient_helpers": list(self.transient_helpers),
                "normal_exit": True, "normal_exit_code": "unavailable",
                "normal_exit_predicate": "hub-child-pre-escalation-generation-absence",
                "stop_requested_monotonic_ns": stop_requested_ns,
                "hub_absent_monotonic_ns": hub_absent_ns,
                "elapsed_to_hub_absence_ns": hub_absent_ns - stop_requested_ns,
                "escalation_boundary_ns": 2_000_000_000,
                "accepted_deadline_ns": 1_800_000_000,
                "wrapper_helper_cleanup_cap_ns": 30_000_000_000,
                "cleanup_absence_observations": cleanup_observations,
            }
        deadline = self.deadline or Deadline(30)
        while True:
            try:
                self.verify_stopped(owned={"service":acquired,"stop_evidence":self._last_stop})
                return dict(self._last_stop) if self._last_stop is not None else None
            except RuntimeError:
                time.sleep(min(0.1, deadline.remaining()))

    def _stop_wrapper_only(self, owned):
        """Dispose an exact wrapper without manufacturing normal Hub-exit proof."""
        wrapper = owned["supervisor"]
        status = self._launch_status(required=False)
        deadline = self.deadline or Deadline(30)
        requested = time.monotonic_ns()
        cleanup_until = min(requested + 30_000_000_000, int(deadline.end * 1_000_000_000))
        if status is not None:
            if status[0] != wrapper["pid"] or self._process(wrapper["pid"]) != wrapper:
                raise RuntimeError("wrapper-only stop generation was replaced")
            if self._hub_child(wrapper["pid"], [EXECUTABLE, "--config", self.config_path, "serve"], allow_missing=True) is not None:
                raise RuntimeError("wrapper-only stop found an unrecorded Hub child")
            for argv in command_plan("stop", True, self.uid, self.plist_path):
                self._run(argv, deadline.remaining(30))
        rows = []
        for role, identity in [("wrapper", wrapper), ("app", owned.get("app_process"))] + [("transient-helper", h) for h in owned.get("transient_helpers", [])]:
            if identity is None:
                continue
            if role == "app" and _generation_present(identity):
                os.kill(identity["pid"], signal.SIGTERM)
            absent = _observe_absent_before(identity, cleanup_until)
            if absent is None:
                raise TimeoutError("wrapper-only generation absence budget expired")
            rows.append({"role": role, "identity": identity, "present": False, "observed_monotonic_ns": absent})
        deadline.remaining()
        self._last_stop = {"operation": "stop", "session_id": self.cfg["session_id"],
            "normal_exit": False, "cleanup_only": True, "normal_exit_predicate": "wrapper-only-generation-absence",
            "wrapper": wrapper, "hub": None, "app": owned.get("app_process"),
            "transient_helpers": list(owned.get("transient_helpers", [])),
            "stop_requested_monotonic_ns": requested, "cleanup_absence_observations": rows}
        self.verify_stopped(owned={"service": owned, "stop_evidence": self._last_stop})
        return dict(self._last_stop)

    def current_context(self, ownership=None):
        payload_digest = verify_payload_members(self.private["guest_inputs"]["package_manifest"])
        receipt, _ = self._receipt()
        if payload_digest != receipt["payload_manifest_sha256"]:
            raise RuntimeError("installed payload differs before use")
        raw = Path(self.config_path).read_bytes()
        config = loads_toml(raw, self.reg["guest"]["python_toml_module"])
        validate_synthetic_config(config, self.data_root, self.data_root + "/hub", self.cfg["allowed_origins"])
        connection = json.loads(Path(self.data_root + "/connection.json").read_text(encoding="utf-8"))
        witness_id = connection.get("hub_id")
        database = self.data_root + "/hub/hub.sqlite"
        query = "import json,sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); c.execute('PRAGMA query_only=ON'); r=c.execute(\"SELECT m.value,p.user_version FROM hub_metadata AS m CROSS JOIN pragma_user_version AS p WHERE m.key='installation_id'\").fetchall(); raise SystemExit(print(json.dumps(r[0],separators=(',',':'))) if len(r)==1 else 2)"
        try:
            database_identity = json.loads(self._run([self.reg["guest"]["python"]["path"], "-B", "-c", query, database], 10).stdout.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("installed database identity query is invalid") from error
        if not isinstance(database_identity, list) or len(database_identity) != 2 or not isinstance(database_identity[0], str) or type(database_identity[1]) is not int or database_identity[1] < 1:
            raise RuntimeError("installed database identity/schema is invalid")
        store_id, schema = database_identity
        import uuid
        if not isinstance(witness_id, str) or str(uuid.UUID(store_id)) != store_id:
            raise RuntimeError("installed store UUID is not canonical")
        if witness_id != store_id:
            raise RuntimeError("database installation identity changed from seed witness")
        scenario_raw = Path(self.data_root + "/scenario.json").read_bytes()
        validate_scenario_bytes(scenario_raw)
        scenario_digest = sha256_bytes(scenario_raw)
        seed_digest = sha256_file(self.private["guest_inputs"]["seed"])
        if scenario_digest != self.cfg["scenario"]["sha256"] or seed_digest != self.cfg["seed"]["sha256"]:
            raise RuntimeError("installed scenario or seed binding changed")
        context = {"path": self.config_path, "sha256": sha256_bytes(raw), "data_dir": config["data_dir"], "store_id": store_id, "store_schema_version": schema, "scenario_sha256": scenario_digest, "seed_sha256": seed_digest}
        recorded = (ownership or {}).get("config") if ownership is not None else self.expected_context
        if recorded is not None:
            if any(recorded.get(key) != context[key] for key in context) or (ownership is not None and ownership.get("store_schema_version") != schema):
                if recorded.get("store_id") != store_id:
                    raise RuntimeError("database installation identity changed after acquisition")
                raise RuntimeError("installed config/store context changed after acquisition")
        self.last_context = dict(context)
        return context

    def _listener(self, pid):
        result = self._run(["/usr/sbin/lsof", "-nP", "-a", "-p", str(pid), "-iTCP:18480", "-sTCP:LISTEN", "-FpcnT"], 10)
        raw = result.stdout
        text = raw.decode("utf-8")
        if "p{}\n".format(pid) not in text or "n127.0.0.1:18480\n" not in text or "TST=LISTEN\n" not in text:
            raise RuntimeError("lsof does not bind fixed listener to Hub child")
        return {"host": "127.0.0.1", "port": 18480, "owner_pid": pid, "socket_identity": sha256_bytes(raw), "observed_at_monotonic_ns": time.monotonic_ns()}

    def _receipt(self):
        raw = self._run(["/usr/bin/sudo", "-n", "/bin/cat", self.private["installation_receipt"]], 10).stdout
        value = json.loads(raw.decode("utf-8"))
        keys = {"schema_version", "package_sha256", "package_manifest_sha256", "payload_manifest_sha256", "installed_version", "provider_id", "install_exit_code", "package_components", "launchagent_template_sha256", "rendered_launchagent_sha256", "preflight_sha256", "stopped_observation"}
        if not isinstance(value, dict) or set(value) != keys or type(value["schema_version"]) is not int or value["schema_version"] != 1 or type(value["install_exit_code"]) is not int or value["install_exit_code"] != 0 or value["provider_id"] != self.reg["provider_id"]:
            raise RuntimeError("macOS installation receipt is invalid")
        if value["package_sha256"] != self.cfg["package"]["sha256"] or value["package_manifest_sha256"] != self.cfg["package_manifest"]["sha256"]:
            raise RuntimeError("macOS installation receipt does not bind candidate bytes")
        manifest = json.loads(Path(self.private["guest_inputs"]["package_manifest"]).read_text(encoding="utf-8"))
        if value["installed_version"] != manifest["package_manager_version"]:
            raise RuntimeError("macOS installation receipt version differs from candidate manifest")
        components = value["package_components"]
        if not isinstance(components, list) or len(components) < 2 or len({item.get("identifier") for item in components if isinstance(item, dict)}) != len(components):
            raise RuntimeError("macOS receipt component identities are invalid")
        if not {"com.teslatlas.hub.app", "com.teslatlas.hub.service"}.issubset({item["identifier"] for item in components}):
            raise RuntimeError("macOS receipt lacks App or service component")
        if value["launchagent_template_sha256"] != manifest["package_scripts"]["launchagent_template"]["sha256"] or value["rendered_launchagent_sha256"] != sha256_file(self.plist_path) or value["stopped_observation"] != {"listener_owner": None, "service_loaded": False}:
            raise RuntimeError("macOS rendered LaunchAgent or stopped preparation evidence changed")
        pkg = self._run(["/usr/sbin/pkgutil", "--pkg-info-plist", "com.teslatlas.hub.service"], 10).stdout
        info = plistlib.loads(pkg)
        if info.get("pkg-version") != value["installed_version"]:
            raise RuntimeError("pkgutil version differs from installation receipt")
        return value, sha256_bytes(raw + b"\0" + pkg)

    def observe(self):
        loaded = self._launch_status()
        wrapper_pid, launch_raw = loaded
        plist = plistlib.loads(Path(self.plist_path).read_bytes())
        plist_argv = [WRAPPER, "--config", self.config_path, "--stdout-log", self.home + "/Library/Logs/Teslatlas Hub/hub.out.log", "--stderr-log", self.home + "/Library/Logs/Teslatlas Hub/hub.err.log"]
        if plist.get("Label") != LABEL or plist.get("ProgramArguments") != plist_argv:
            raise RuntimeError("loaded LaunchAgent plist differs from fixed packaged definition")
        expected_wrapper_argv = ["/bin/sh"] + plist_argv
        wrapper = self._process(wrapper_pid, expected_wrapper_argv)
        hub_expected = [EXECUTABLE, "--config", self.config_path, "serve"]
        hub = self._hub_child(wrapper_pid, hub_expected)
        validate_wrapper_child(wrapper, hub, [hub["pid"]])
        listener = self._listener(hub["pid"])
        config_raw = Path(self.config_path).read_bytes()
        config = loads_toml(config_raw, self.reg["guest"]["python_toml_module"])
        tls = validate_synthetic_config(config, self.data_root, self.data_root + "/hub", self.cfg["allowed_origins"])
        receipt, receipt_digest = self._receipt()
        if verify_payload_members(self.private["guest_inputs"]["package_manifest"]) != receipt["payload_manifest_sha256"]:
            raise RuntimeError("installed payload differs from installation receipt")
        probe = normal_tls_probe(str(Path(self.private["controller_root"]) / "public-ca.pem"), self.cfg["expected"]["product_version"], deadline=self.deadline, profile_path=self.private["guest_inputs"]["profile"])
        if self._process(wrapper_pid, expected_wrapper_argv) != wrapper or self._process(hub["pid"], hub_expected) != hub:
            raise RuntimeError("LaunchAgent generation changed during verification")
        ioreg = self._run(["/usr/sbin/ioreg", "-rd1", "-c", "IOPlatformExpertDevice"], 10).stdout
        version = self._run(["/usr/bin/sw_vers", "-productVersion"], 10).stdout.decode("ascii").strip()
        architecture = self._run(["/usr/bin/uname", "-m"], 10).stdout.decode("ascii").strip()
        if architecture != "arm64":
            raise RuntimeError("macOS guest architecture is not arm64")
        connection = self.connection or json.loads(Path(self.data_root + "/connection.json").read_text(encoding="utf-8"))
        generation = "{}:{}".format(hub["boot_id"], hub["start_identity"])
        app_receipt = sha256_bytes(json.dumps(self.app_process, sort_keys=True, separators=(",", ":")).encode("utf-8"))
        observer = self._process(os.getpid())
        return {
            "observer": {"bundle_sha256": self.cfg["controller_bundle"]["sha256"], "process": observer},
            "host": {"host_id": self.cfg["host_id"], "machine_identity_sha256": sha256_bytes(ioreg), "os": "macOS", "os_version": version, "architecture": "arm64", "kernel": self._run(["/usr/bin/uname", "-r"], 10).stdout.decode("ascii").strip(), "native_or_emulated": self.cfg["expected"]["native_or_emulated"], "hypervisor_evidence_sha256": self.reg["guest"]["hypervisor_evidence_sha256"]},
            "package": {"sha256": receipt["package_sha256"], "manifest_sha256": receipt["package_manifest_sha256"], "installed_version": receipt["installed_version"], "payload_manifest_sha256": receipt["payload_manifest_sha256"], "installation_receipt_sha256": receipt_digest},
            "service": {"mode": "installed-app-launchagent", "target": "gui/{}/{}".format(self.uid, LABEL), "definition_sha256": sha256_file(self.plist_path), "state": "running", "generation": generation, "post_probe_generation": generation, "supervisor": wrapper, "hub": hub, "process_tree_sha256": process_tree_digest(wrapper, hub), "plist_sha256": sha256_file(self.plist_path), "wrapper_script_sha256": sha256_file(WRAPPER), "loaded_state": "loaded", "app_receipt_sha256": app_receipt, "app_process": self.app_process},
            "config": {"path": self.config_path, "sha256": sha256_bytes(config_raw), "data_dir": config["data_dir"], "store_id": connection["hub_id"], "store_schema_version": self.current_context()["store_schema_version"], "scenario_sha256": sha256_file(self.private["guest_inputs"]["scenario"]), "seed_sha256": sha256_file(self.private["guest_inputs"]["seed"])},
            "listener": listener,
            "tls": {"endpoint": tls["public_url"], "certificate_der_sha256": probe["certificate_der_sha256"], "verified_chain": True, "verified_hostname": True, "redirect_count": 0},
            "discovery": {"hub_id": probe["hub_id"], "product_version": self.cfg["expected"]["product_version"], "response_sha256": probe["response_sha256"], "profile_id": "hub-http-v1@1.0.0", "profile_sha256": sha256_file(self.private["guest_inputs"]["profile"]), "validated_response_set_sha256": probe["validated_response_set_sha256"]},
        }

    def verify_stopped(self, owned=None, initial=False):
        if self._launch_status(required=False) is not None:
            raise RuntimeError("LaunchAgent remains loaded")
        lsof = self._run(["/usr/sbin/lsof", "-nP", "-iTCP:18480", "-sTCP:LISTEN", "-FpcnT"], 10, (0, 1))
        if lsof.returncode == 0 and lsof.stdout:
            raise RuntimeError("an unseen process owns the fixed listener")
        context = owned or {}
        service_context = context.get("service", context)
        identities = [service_context.get("supervisor"), service_context.get("wrapper"), service_context.get("hub"), service_context.get("app"), service_context.get("app_process")]
        identities.extend(service_context.get("transient_helpers", []))
        for identity in identities:
            if identity is not None and _generation_present(identity):
                raise RuntimeError("owned macOS process generation survived stop")
        stop_evidence = context.get("stop_evidence") or (context if context.get("normal_exit_predicate") else self._last_stop)
        normal = initial or _local_stop_is_normal(service_context, stop_evidence, self.cfg["session_id"])
        owned_generation = None if initial else {"service": service_context, "stop_evidence": stop_evidence}
        return {"schema_version": 1, "status": "stopped", "session_id": self.cfg["session_id"], "host_id": self.cfg["host_id"], "service": {"state": "stopped", "generation": None, "forced_escalation": False, "normal_exit": normal, "owned_generation": owned_generation, "cleanup_errors": []}, "listener": {"host": "127.0.0.1", "port": 18480, "owner_pid": None}}
