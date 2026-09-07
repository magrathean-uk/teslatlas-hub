# SPDX-License-Identifier: AGPL-3.0-only
"""Fixed Debian systemd observation and control primitives."""
import json
import os
import re
import time
from pathlib import Path

from ._common import (
    loads_toml, normal_tls_probe, process_tree_digest, run_checked,
    validate_scenario_bytes,
    sha256_bytes, sha256_file, validate_synthetic_config,
    verify_payload_members,
)
from .bounded import Deadline


UNIT = "teslatlas-hub.service"
EXECUTABLE = "/usr/bin/teslatlas-hub"
CONFIG = "/etc/teslatlas-hub/config.toml"
SYSTEMD_FIELDS = (
    "LoadState", "ActiveState", "SubState", "MainPID", "ControlGroup",
    "InvocationID", "FragmentPath", "DropInPaths", "Result", "ExecMainCode", "ExecMainStatus",
)


def _cross_boot_tick(deadline=None, clock_ns=None, pause=None, ticks_per_second=None):
    """Cross one Linux boot-clock tick and return its exact tick basis."""
    if clock_ns is None:
        if not hasattr(time, "CLOCK_BOOTTIME"):
            raise RuntimeError("Linux CLOCK_BOOTTIME is unavailable")
        clock_ns = lambda: time.clock_gettime_ns(time.CLOCK_BOOTTIME)
    pause = pause or time.sleep
    ticks = ticks_per_second or os.sysconf("SC_CLK_TCK")
    if type(ticks) is not int or ticks <= 0:
        raise RuntimeError("Linux SC_CLK_TCK is invalid")
    first = clock_ns() * ticks // 1_000_000_000
    while True:
        if deadline is not None:
            deadline.remaining()
        now_ns = clock_ns()
        current = now_ns * ticks // 1_000_000_000
        if current > first:
            return {"clock_basis":"CLOCK_BOOTTIME_SC_CLK_TCK","ticks_per_second":ticks,"not_before_start_tick":current,"boundary_boottime_ns":now_ns}
        pause(0.001)


def control_argv(operation):
    if operation not in ("start", "stop"):
        raise ValueError("unsupported fixed systemd operation")
    return ["/usr/bin/sudo", "-n", "/bin/systemctl", operation, UNIT]


def parse_systemd_show(raw):
    values = {}
    for line in raw.splitlines():
        if not line or "=" not in line:
            continue
        key, value = line.split("=", 1)
        if key not in SYSTEMD_FIELDS:
            continue
        if key in values:
            raise ValueError("duplicate systemd field: {}".format(key))
        values[key] = value
    empty_when_stopped = {"ControlGroup", "InvocationID", "DropInPaths"}
    for key in SYSTEMD_FIELDS:
        if key not in values or (values[key] == "" and key not in empty_when_stopped):
            raise ValueError("missing systemd field: {}".format(key))
    return values


def parse_proc_stat(raw):
    match = re.fullmatch(r"([1-9][0-9]*) \((.*)\) (.+)", raw.strip())
    if match is None:
        raise ValueError("invalid /proc stat")
    fields = match.group(3).split()
    if len(fields) < 20 or not fields[1].isdigit() or not fields[19].isdigit():
        raise ValueError("incomplete /proc stat")
    return {"pid": int(match.group(1)), "parent_pid": int(fields[1]), "start_ticks": int(fields[19])}


def _canonical_digest(value):
    return sha256_bytes(json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8"))


class LinuxController:
    def __init__(self, registered, private):
        self.registered = registered
        self.cfg = registered.config
        self.reg = registered.registration
        self.private = private
        self.data_root = "/var/lib/teslatlas-hub/interop-matrix/" + self.cfg["guest_run_id"]
        self.config_path = CONFIG
        self.connection = None
        self._prepared = False
        self._owned = None
        self._last_stop = None
        self.deadline = None
        self.acquisition_callback = None
        self.context_callback = None
        self.last_context = None
        self.expected_context = None

    def set_deadline(self, deadline):
        self.deadline = deadline

    def _run(self, argv, timeout=15, allowed_status=(0,)):
        return run_checked(argv, timeout, allowed_status, self.deadline)

    def _as_service(self, argv):
        return ["/usr/bin/sudo", "-n", "-u", self.reg["guest"]["permitted_service_user"]] + argv

    def _service_read(self, path):
        return self._run(self._as_service(["/bin/cat", path]), 10).stdout

    def _systemd(self):
        argv = [
            "/bin/systemctl", "show", "--no-page",
            "--property=" + ",".join(SYSTEMD_FIELDS), UNIT,
        ]
        return parse_systemd_show(self._run(argv, 10).stdout.decode("utf-8"))

    def prepare(self):
        if self._prepared:
            raise RuntimeError("cell preparation is single-use")
        service = self.reg["guest"]["permitted_service_user"]
        parts = self.cfg["guest_run_id"].split("/")
        if len(parts) != 2:
            raise RuntimeError("guest run id is not a run/cell path")
        base = "/var/lib/teslatlas-hub/interop-matrix"
        run_root = base + "/" + parts[0]
        for ancestor in (base, run_root):
            status = self._run(self._as_service(["/usr/bin/stat", "-c", "%U:%a:%F", ancestor]), 10, (0, 1))
            if status.returncode == 1:
                self._run(["/usr/bin/sudo", "-n", "/usr/bin/install", "-d", "-o", service, "-g", service, "-m", "0700", ancestor], 10)
                status = self._run(self._as_service(["/usr/bin/stat", "-c", "%U:%a:%F", ancestor]), 10)
            if status.stdout.decode("utf-8").strip() != service + ":700:directory":
                raise RuntimeError("Debian seed ancestor is symlinked, foreign-owned, or has wrong mode")
        self._run(self._as_service(["/usr/bin/test", "!", "-e", self.data_root]), 10)
        seed = self.private["guest_inputs"]["seed"]
        if sha256_file(seed) != self.cfg["seed"]["sha256"]:
            raise RuntimeError("guest seed bytes do not match session")
        validate_scenario_bytes(Path(self.private["guest_inputs"]["scenario"]).read_bytes())
        output = self._run(self._as_service([seed, "--output", self.data_root, "--port", "18480"]), 60)
        prepared = json.loads(output.stdout.decode("utf-8"))
        expected_config = self.data_root + "/config.toml"
        expected_certificate = self.data_root + "/server.pem"
        if prepared.get("config_path") != expected_config or prepared.get("certificate_path") != expected_certificate or prepared.get("endpoint") != "https://127.0.0.1:18480":
            raise RuntimeError("target-native seed returned unexpected paths")
        scenario_target = self.data_root + "/scenario.json"
        self._run(["/usr/bin/sudo", "-n", "/usr/bin/install", "-o", service, "-g", service, "-m", "0600", self.private["guest_inputs"]["scenario"], scenario_target], 10)
        config_raw = self._service_read(expected_config)
        config_raw += b"\n[http]\nallowed_origins = " + json.dumps(self.cfg["allowed_origins"], separators=(",", ":")).encode("utf-8") + b"\n"
        staged = Path(self.private["controller_root"]) / "config.toml"
        staged.write_bytes(config_raw)
        os.chmod(str(staged), 0o600)
        self._run(["/usr/bin/sudo", "-n", "/usr/bin/install", "-o", self.reg["guest"]["permitted_service_user"], "-g", self.reg["guest"]["permitted_service_user"], "-m", "0600", str(staged), CONFIG], 10)
        ca_copy = Path(self.private["controller_root"]) / "public-ca.pem"
        ca_copy.write_bytes(self._service_read(expected_certificate))
        os.chmod(str(ca_copy), 0o600)
        self.connection = prepared
        manifest = json.loads(Path(self.private["guest_inputs"]["package_manifest"]).read_text(encoding="utf-8"))
        installed_scenario = self._service_read(scenario_target)
        validate_scenario_bytes(installed_scenario)
        if prepared.get("hub_id") is None or sha256_bytes(installed_scenario) != self.cfg["scenario"]["sha256"]:
            raise RuntimeError("seeded identity or installed scenario is absent")
        schema = int(self._run(self._as_service([self.reg["guest"]["python"]["path"], "-B", "-c", "import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); print(c.execute('pragma user_version').fetchone()[0])", self.data_root + "/hub/hub.sqlite"]), 10).stdout)
        if schema != manifest["store_schema_version"]:
            raise RuntimeError("seeded store schema differs from package manifest")
        self.pin_current_context()
        self._prepared = True
        expired = self.pair(1)
        invitation = self.pair(900)
        expires = expired.get("expiresAtMs")
        if type(expires) is int:
            while int(time.time() * 1000) <= expires:
                time.sleep(0.05)
        return {"invitation": invitation, "expired_invitation": expired, "seed_facts": {"store_id": prepared["hub_id"], "store_schema_version": schema, "scenario_sha256": self.cfg["scenario"]["sha256"]}}

    def preflight(self):
        machine = sha256_file("/etc/machine-id")
        os_release = {}
        for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
            if "=" in line:
                key, value = line.split("=", 1)
                os_release[key] = value.strip('"')
        architecture = self._run(["/usr/bin/dpkg", "--print-architecture"], 10).stdout.decode("ascii").strip()
        uid = int(self._run(["/usr/bin/id", "-u", self.reg["guest"]["permitted_service_user"]], 10).stdout)
        if machine != self.reg["guest"]["machine_identity_sha256"] or os_release.get("ID") != "debian" or os_release.get("VERSION_ID") != "13" or architecture != self.cfg["expected"]["architecture"] or uid != self.reg["guest"]["permitted_service_uid"]:
            raise RuntimeError("Debian current host identity differs from registration")
        self._receipt()
        self.verify_stopped(initial=True)
        return {"machine_identity_sha256": machine, "os": "Debian 13", "architecture": architecture, "service_uid": uid, "stopped": True}

    def assert_owned(self, ownership):
        if ownership.get("session_id") != self.cfg["session_id"] or ownership.get("lease") != self.reg["lease"] or ownership.get("config_path") != CONFIG:
            raise RuntimeError("Linux mutation ownership record is foreign")
        current = self.current_context(ownership)
        if ownership.get("store_id") != current["store_id"]:
            raise RuntimeError("Linux mutation store identity changed")
        if ownership.get("state") == "stopped":
            self.verify_stopped(owned=ownership)
            return
        service = ownership.get("service") or {}
        status = self._systemd()
        hub = service.get("hub")
        if hub is None or int(status["MainPID"]) != hub.get("pid") or status["ControlGroup"] != service.get("control_group") or self._process(hub["pid"], (EXECUTABLE, "--config", CONFIG, "serve"), service_owned=True) != hub:
            raise RuntimeError("Linux owned service generation was replaced")

    def bind_recovery(self, ownership):
        self.connection = {"hub_id": ownership.get("store_id")}
        self.expected_context = dict(ownership.get("config") or {})

    def reconcile_owned(self, ownership):
        service = ownership.get("service") or {}
        if service.get("acquisition_state") != "pending-start":
            return dict(service)
        boundary=service.get("start_boundary")
        if service.get("target") != UNIT or service.get("pre_start_state") != {"active":"inactive","sub":"dead","pid":0,"invocation_id":service.get("pre_start_state",{}).get("invocation_id","")} or not isinstance(boundary,dict) or set(boundary)!={"clock_basis","ticks_per_second","not_before_start_tick","boundary_boottime_ns"} or boundary.get("clock_basis")!="CLOCK_BOOTTIME_SC_CLK_TCK" or type(boundary.get("ticks_per_second")) is not int or type(boundary.get("not_before_start_tick")) is not int or type(boundary.get("boundary_boottime_ns")) is not int:
            raise RuntimeError("Linux pending acquisition target is foreign")
        self.current_context(ownership)
        status = self._systemd()
        if status["ActiveState"] != "active" or status["SubState"] != "running" or int(status["MainPID"]) <= 1:
            raise RuntimeError("Linux pending start has no exact active generation")
        return self._capture_pending(service, status)

    def _capture_pending(self, service, status):
        if status["InvocationID"].lower() == service["pre_start_state"]["invocation_id"].lower():
            raise RuntimeError("Linux pending start retained the previous invocation")
        captured = self.capture_service(status, publish=False)
        start_ticks = captured["hub"].get("start_identity")
        boundary=service["start_boundary"]
        if boundary["ticks_per_second"] != os.sysconf("SC_CLK_TCK") or not isinstance(start_ticks, str) or not start_ticks.isdigit() or int(start_ticks) < boundary["not_before_start_tick"]:
            raise RuntimeError("Linux pending start observed a pre-intent generation")
        result = dict(service); result.update(captured)
        if self.acquisition_callback is not None:
            self.acquisition_callback(result)
        return result

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
        result = self._run(self._as_service([EXECUTABLE, "--config", CONFIG, "pair", "--json", "--label", "Owned matrix lane", "--expires-in-seconds", str(seconds)]), 60)
        value = json.loads(result.stdout.decode("utf-8"))
        if not isinstance(value, dict) or "secret" not in value:
            raise RuntimeError("installed CLI returned invalid invitation")
        return value

    def paired_device_ids(self):
        self.current_context()
        result = self._run(self._as_service([EXECUTABLE, "--config", CONFIG, "control", "paired-devices"]), 30)
        values = json.loads(result.stdout.decode("utf-8"))
        if not isinstance(values, list):
            raise RuntimeError("installed CLI paired-device output is invalid")
        ids = set()
        for value in values:
            if not isinstance(value, dict):
                raise RuntimeError("installed CLI paired-device row is invalid")
            device = value.get("device_id", value.get("deviceId"))
            if not isinstance(device, str):
                raise RuntimeError("installed CLI paired-device ID is absent")
            ids.add(device)
        return ids

    def revoke(self, device_id):
        self.current_context()
        self._run(self._as_service([EXECUTABLE, "--config", CONFIG, "control", "revoke-device", device_id]), 60)

    def advance_once(self):
        self.current_context()
        database = self.data_root + "/hub/hub.sqlite"
        before = self._run(self._as_service(["/usr/bin/sha256sum", database]), 30).stdout.decode("ascii").split()[0]
        self._run(self._as_service([self.private["guest_inputs"]["seed"], "--advance", self.data_root]), 60)
        after = self._run(self._as_service(["/usr/bin/sha256sum", database]), 30).stdout.decode("ascii").split()[0]
        return {"before_store_sha256": before, "after_store_sha256": after, "scenario_sha256": sha256_file(self.private["guest_inputs"]["scenario"]), "seed_sha256": sha256_file(self.private["guest_inputs"]["seed"])}

    def start(self):
        before = self._systemd()
        if before["ActiveState"] != "inactive" or before["SubState"] != "dead" or int(before["MainPID"]) != 0:
            raise RuntimeError("systemd start requires the registered unit to be stopped")
        # /proc starttime is quantized in SC_CLK_TCK units on the boot clock.
        # Cross a whole boot tick, then re-prove absence, so a legitimate child
        # in the boundary tick is accepted and any earlier generation is not.
        boundary=_cross_boot_tick(self.deadline)
        at_boundary=self._systemd()
        if at_boundary["ActiveState"] != "inactive" or at_boundary["SubState"] != "dead" or int(at_boundary["MainPID"]) != 0 or at_boundary["InvocationID"] != before["InvocationID"]:
            raise RuntimeError("systemd unit changed while establishing the start boundary")
        pending={"acquisition_state": "pending-start", "target": UNIT, "start_boundary":boundary, "pre_start_state":{"active":"inactive","sub":"dead","pid":0,"invocation_id":before["InvocationID"]}}
        if self.acquisition_callback is not None:
            self.acquisition_callback(pending)
        try:
            self._run(control_argv("start"), 30)
        except BaseException:
            # The local SSH command can lose completion after systemd accepted
            # the fixed unit start. Reconcile only the exact active unit under
            # the already durable pending intent and current lease/context.
            try:
                status = self._systemd()
                if status["ActiveState"] == "active" and status["SubState"] == "running" and int(status["MainPID"]) > 1:
                    self._capture_pending(pending,status)
            except BaseException:
                pass
            raise
        deadline = self.deadline or Deadline(30)
        while True:
            status = self._systemd()
            if status["ActiveState"] == "active" and int(status["MainPID"]) > 1:
                self._capture_pending(pending,status)
                return
            time.sleep(min(0.1, deadline.remaining()))

    def capture_service(self, status=None, publish=True):
        status = status or self._systemd()
        pid = int(status["MainPID"])
        process = self._process(pid, (EXECUTABLE, "--config", CONFIG, "serve"), service_owned=True)
        value = {"acquisition_state": "complete", "hub": process, "supervisor": process, "control_group": status["ControlGroup"], "invocation_id": status["InvocationID"]}
        if publish and self.acquisition_callback is not None:
            self.acquisition_callback(value)
        return value

    def stop(self):
        before = self._systemd()
        prior_pid = int(before["MainPID"])
        prior_cgroup = before["ControlGroup"]
        if prior_pid <= 1:
            raise RuntimeError("systemd stop lacks a live acquired MainPID")
        prior_process = self._process(prior_pid, (EXECUTABLE, "--config", CONFIG, "serve"), service_owned=True)
        prior_invocation = before["InvocationID"].lower()
        self._run(control_argv("stop"), 30)
        deadline = self.deadline or Deadline(30)
        while True:
            try:
                proof = self.verify_stopped(owned={"pid": prior_pid, "control_group": prior_cgroup})
                if proof["status"] == "stopped":
                    after = self._systemd()
                    try:
                        raw_code = int(after["ExecMainCode"])
                        raw_status = int(after["ExecMainStatus"])
                    except ValueError as error:
                        raise RuntimeError("systemd exit properties are not numeric") from error
                    if after["Result"] != "success" or raw_code != 1 or raw_status != 0:
                        raise RuntimeError("systemd owned generation did not exit normally")
                    self._last_stop = {"operation": "stop", "session_id": self.cfg["session_id"], "pid": prior_pid, "hub_start_identity": prior_process["start_identity"], "control_group": prior_cgroup, "invocation_id": prior_invocation, "result_raw": after["Result"], "exec_main_code_raw": after["ExecMainCode"], "exec_main_code_semantic": "exited", "exec_main_status_raw": after["ExecMainStatus"], "normal_exit": True}
                    return dict(self._last_stop)
            except RuntimeError:
                pass
            time.sleep(min(0.1, deadline.remaining()))

    def current_context(self, ownership=None):
        payload_digest = verify_payload_members(self.private["guest_inputs"]["package_manifest"])
        receipt, _ = self._receipt()
        if payload_digest != receipt["payload_manifest_sha256"]:
            raise RuntimeError("installed payload differs before use")
        raw = self._service_read(CONFIG)
        config = loads_toml(raw, self.reg["guest"]["python_toml_module"])
        validate_synthetic_config(config, self.data_root, self.data_root + "/hub", self.cfg["allowed_origins"])
        connection = json.loads(self._service_read(self.data_root + "/connection.json").decode("utf-8"))
        witness_id = connection.get("hub_id")
        if not isinstance(witness_id, str):
            raise RuntimeError("installed store UUID witness is absent")
        database = self.data_root + "/hub/hub.sqlite"
        query = "import json,sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); c.execute('PRAGMA query_only=ON'); r=c.execute(\"SELECT m.value,p.user_version FROM hub_metadata AS m CROSS JOIN pragma_user_version AS p WHERE m.key='installation_id'\").fetchall(); raise SystemExit(print(json.dumps(r[0],separators=(',',':'))) if len(r)==1 else 2)"
        try:
            database_identity = json.loads(self._run(self._as_service([self.reg["guest"]["python"]["path"], "-B", "-c", query, database]), 10).stdout.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("installed database identity query is invalid") from error
        if not isinstance(database_identity, list) or len(database_identity) != 2 or not isinstance(database_identity[0], str) or type(database_identity[1]) is not int or database_identity[1] < 1:
            raise RuntimeError("installed database identity/schema is invalid")
        store_id, schema = database_identity
        import uuid
        if str(uuid.UUID(store_id)) != store_id:
            raise RuntimeError("installed store UUID is not canonical")
        if witness_id != store_id:
            raise RuntimeError("database installation identity changed from seed witness")
        scenario_raw = self._service_read(self.data_root + "/scenario.json")
        validate_scenario_bytes(scenario_raw)
        scenario_digest = sha256_bytes(scenario_raw)
        seed_digest = sha256_file(self.private["guest_inputs"]["seed"])
        if scenario_digest != self.cfg["scenario"]["sha256"] or seed_digest != self.cfg["seed"]["sha256"]:
            raise RuntimeError("installed scenario or seed binding changed")
        context = {"path": CONFIG, "sha256": sha256_bytes(raw), "data_dir": config["data_dir"], "store_id": store_id, "store_schema_version": schema, "scenario_sha256": scenario_digest, "seed_sha256": seed_digest}
        recorded = (ownership or {}).get("config") if ownership is not None else self.expected_context
        if recorded is not None:
            if any(recorded.get(key) != context[key] for key in context) or (ownership is not None and ownership.get("store_schema_version") != schema):
                if recorded.get("store_id") != store_id:
                    raise RuntimeError("database installation identity changed after acquisition")
                raise RuntimeError("installed config/store context changed after acquisition")
        self.last_context = dict(context)
        return context

    def _process(self, pid, expected_argv=None, service_owned=False):
        base = Path("/proc") / str(pid)
        def read(name):
            path = str(base / name)
            return self._run(self._as_service(["/bin/cat", path]), 10).stdout if service_owned else (base / name).read_bytes()
        stat_value = parse_proc_stat(read("stat").decode("utf-8"))
        status = {}
        for line in read("status").decode("utf-8").splitlines():
            if ":" in line:
                key, value = line.split(":", 1)
                status[key] = value.strip()
        uid = int(status["Uid"].split()[0])
        executable = self._run(self._as_service(["/usr/bin/readlink", str(base / "exe")]), 10).stdout.decode("utf-8").strip() if service_owned else os.readlink(str(base / "exe"))
        argv = read("cmdline")
        expected = None if expected_argv is None else b"\0".join(item.encode("utf-8") for item in expected_argv) + b"\0"
        if expected is not None and argv != expected:
            raise RuntimeError("systemd MainPID argv differs from fixed packaged unit")
        boot_id = Path("/proc/sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
        return {
            "pid": pid, "uid": uid, "parent_pid": stat_value["parent_pid"], "boot_id": boot_id,
            "start_identity": str(stat_value["start_ticks"]), "executable_path": executable,
            "executable_sha256": sha256_file(executable), "argv_sha256": sha256_bytes(argv),
        }

    def _listener(self, pid):
        matches = []
        for table in ("/proc/net/tcp", "/proc/net/tcp6"):
            for line in Path(table).read_text(encoding="ascii").splitlines()[1:]:
                fields = line.split()
                if len(fields) >= 10 and fields[1].upper() == "0100007F:4830" and fields[3] == "0A":
                    matches.append(fields[9])
        if len(matches) != 1:
            raise RuntimeError("fixed loopback listener was not uniquely observed")
        inode = matches[0]
        owned = self._run(self._as_service(["/usr/bin/find", "/proc/{}/fd".format(pid), "-maxdepth", "1", "-type", "l", "-lname", "socket:\\[{}\\]".format(inode), "-print"]), 10).stdout.splitlines()
        if len(owned) != 1:
            raise RuntimeError("fixed listener belongs to another process")
        return {"host": "127.0.0.1", "port": 18480, "owner_pid": pid, "socket_identity": "inode:" + inode, "observed_at_monotonic_ns": time.monotonic_ns()}

    def _receipt(self):
        raw = self._run(["/usr/bin/sudo", "-n", "/bin/cat", self.private["installation_receipt"]], 10).stdout
        value = json.loads(raw.decode("utf-8"))
        keys = {"schema_version", "package_sha256", "package_manifest_sha256", "payload_manifest_sha256", "installed_version", "provider_id", "install_exit_code", "package_components", "launchagent_template_sha256", "rendered_launchagent_sha256", "preflight_sha256", "stopped_observation"}
        if not isinstance(value, dict) or set(value) != keys or type(value["schema_version"]) is not int or value["schema_version"] != 1 or type(value["install_exit_code"]) is not int or value["install_exit_code"] != 0:
            raise RuntimeError("installation receipt is invalid")
        if value["package_sha256"] != self.cfg["package"]["sha256"] or value["package_manifest_sha256"] != self.cfg["package_manifest"]["sha256"] or value["provider_id"] != self.reg["provider_id"]:
            raise RuntimeError("installation receipt does not bind this package and guest")
        manifest = json.loads(Path(self.private["guest_inputs"]["package_manifest"]).read_text(encoding="utf-8"))
        if value["installed_version"] != manifest["package_manager_version"]:
            raise RuntimeError("installation receipt version differs from candidate manifest")
        if value["package_components"] != [] or value["launchagent_template_sha256"] is not None or value["rendered_launchagent_sha256"] is not None or value["stopped_observation"] != {"listener_owner": None, "service_loaded": False}:
            raise RuntimeError("Debian receipt contains invalid platform preparation evidence")
        dpkg = self._run(["/usr/bin/dpkg-query", "-W", "-f=${Status}\n${Version}\n${Architecture}\n", "teslatlas-hub"], 10).stdout
        lines = dpkg.decode("utf-8").splitlines()
        expected_arch = "amd64" if self.cfg["expected"]["architecture"] == "amd64" else "arm64"
        if lines != ["install ok installed", value["installed_version"], expected_arch]:
            raise RuntimeError("dpkg state differs from installation receipt")
        return value, sha256_bytes(raw + b"\0" + dpkg)

    def observe(self):
        status = self._systemd()
        if status["LoadState"] != "loaded" or status["ActiveState"] != "active" or status["SubState"] != "running":
            raise RuntimeError("systemd unit is not running")
        pid = int(status["MainPID"])
        process = self._process(pid, (EXECUTABLE, "--config", CONFIG, "serve"), service_owned=True)
        if process["uid"] != self.reg["guest"]["permitted_service_uid"]:
            raise RuntimeError("systemd MainPID UID differs from registration")
        listener = self._listener(pid)
        membership = self._service_read("/proc/{}/cgroup".format(pid)).decode("utf-8")
        paths = []
        for row in membership.splitlines():
            fields = row.split(":", 2)
            if len(fields) != 3:
                raise RuntimeError("systemd MainPID cgroup row is invalid")
            paths.append(fields[2])
        if paths.count(status["ControlGroup"]) != 1:
            raise RuntimeError("systemd MainPID is outside the observed control group")
        config_raw = self._service_read(CONFIG)
        config = loads_toml(config_raw, self.reg["guest"]["python_toml_module"])
        tls = validate_synthetic_config(config, self.data_root, self.data_root + "/hub", self.cfg["allowed_origins"])
        connection = self.connection or json.loads(self._service_read(self.data_root + "/connection.json").decode("utf-8"))
        receipt, receipt_digest = self._receipt()
        if verify_payload_members(self.private["guest_inputs"]["package_manifest"]) != receipt["payload_manifest_sha256"]:
            raise RuntimeError("installed payload differs from installation receipt")
        tls_probe = normal_tls_probe(str(Path(self.private["controller_root"]) / "public-ca.pem"), self.cfg["expected"]["product_version"], deadline=self.deadline, profile_path=self.private["guest_inputs"]["profile"])
        after = self._process(pid, (EXECUTABLE, "--config", CONFIG, "serve"), service_owned=True)
        if process != after:
            raise RuntimeError("Hub generation changed during verification")
        fragment = status["FragmentPath"]
        if fragment != "/usr/lib/systemd/system/teslatlas-hub.service":
            raise RuntimeError("systemd fragment is not the packaged unit")
        drop_ins = []
        for path in status["DropInPaths"].split():
            drop_ins.append({"path": path, "sha256": sha256_file(path)})
        if drop_ins:
            raise RuntimeError("installed systemd unit has an unreviewed drop-in")
        machine = Path("/etc/machine-id")
        architecture = self._run(["/usr/bin/dpkg", "--print-architecture"], 10).stdout.decode("ascii").strip()
        if architecture != self.cfg["expected"]["architecture"]:
            raise RuntimeError("guest architecture differs from session")
        os_release = {}
        for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
            if "=" in line:
                key, val = line.split("=", 1)
                os_release[key] = val.strip('"')
        observer = self._process(os.getpid())
        generation = "{}:{}".format(process["boot_id"], process["start_identity"])
        return {
            "observer": {"bundle_sha256": self.cfg["controller_bundle"]["sha256"], "process": observer},
            "host": {"host_id": self.cfg["host_id"], "machine_identity_sha256": sha256_file(machine), "os": "Debian 13", "os_version": os_release.get("VERSION_ID"), "architecture": architecture, "kernel": self._run(["/bin/uname", "-r"], 10).stdout.decode("ascii").strip(), "native_or_emulated": self.cfg["expected"]["native_or_emulated"], "hypervisor_evidence_sha256": self.reg["guest"]["hypervisor_evidence_sha256"]},
            "package": {"sha256": receipt["package_sha256"], "manifest_sha256": receipt["package_manifest_sha256"], "installed_version": receipt["installed_version"], "payload_manifest_sha256": receipt["payload_manifest_sha256"], "installation_receipt_sha256": receipt_digest},
            "service": {"mode": "installed-deb-systemd", "target": UNIT, "definition_sha256": sha256_file(fragment), "state": "running", "generation": generation, "post_probe_generation": generation, "supervisor": process, "hub": process, "process_tree_sha256": process_tree_digest(process), "invocation_id": status["InvocationID"].lower(), "control_group": status["ControlGroup"], "fragment_sha256": sha256_file(fragment), "drop_in_manifest_sha256": _canonical_digest(drop_ins), "result": status["Result"], "exec_main_code": status["ExecMainCode"], "exec_main_status": int(status["ExecMainStatus"])},
            "config": {"path": CONFIG, "sha256": sha256_bytes(config_raw), "data_dir": config["data_dir"], "store_id": connection["hub_id"], "store_schema_version": self.current_context()["store_schema_version"], "scenario_sha256": sha256_file(self.private["guest_inputs"]["scenario"]), "seed_sha256": sha256_file(self.private["guest_inputs"]["seed"])},
            "listener": listener,
            "tls": {"endpoint": tls["public_url"], "certificate_der_sha256": tls_probe["certificate_der_sha256"], "verified_chain": True, "verified_hostname": True, "redirect_count": 0},
            "discovery": {"hub_id": tls_probe["hub_id"], "product_version": self.cfg["expected"]["product_version"], "response_sha256": tls_probe["response_sha256"], "profile_id": "hub-http-v1@1.0.0", "profile_sha256": sha256_file(self.private["guest_inputs"]["profile"]), "validated_response_set_sha256": tls_probe["validated_response_set_sha256"]},
        }

    def verify_stopped(self, owned=None, initial=False):
        status = self._systemd()
        pid = int(status["MainPID"])
        populated = ""
        context = owned or {}
        service_context = context.get("service", context)
        hub_context = service_context.get("hub", {}) if isinstance(service_context, dict) else {}
        check_group = service_context.get("control_group") or status["ControlGroup"]
        if check_group:
            cgroup = Path("/sys/fs/cgroup" + check_group)
            populated = (cgroup / "cgroup.procs").read_text(encoding="ascii").strip() if cgroup.exists() else ""
        listener_owner = None
        for table in ("/proc/net/tcp", "/proc/net/tcp6"):
            for line in Path(table).read_text(encoding="ascii").splitlines()[1:]:
                fields = line.split()
                if len(fields) >= 4 and fields[1].upper() == "0100007F:4830" and fields[3] == "0A":
                    listener_owner = -1
        clean = status["ActiveState"] == "inactive" and status["SubState"] == "dead" and pid == 0 and populated == "" and listener_owner is None
        prior_pid = service_context.get("pid") or hub_context.get("pid")
        if prior_pid and Path("/proc/{}".format(prior_pid)).exists():
            clean = False
        if not clean:
            raise RuntimeError("independent systemd stopped observation failed")
        stop_evidence = context.get("stop_evidence") or self._last_stop
        normal = initial or bool(stop_evidence and stop_evidence.get("normal_exit") is True and stop_evidence.get("session_id") == self.cfg["session_id"] and stop_evidence.get("exec_main_code_raw") == "1" and stop_evidence.get("exec_main_status_raw") == "0" and stop_evidence.get("result_raw") == "success")
        owned_generation = None if initial else {"service": service_context, "stop_evidence": stop_evidence}
        return {"schema_version": 1, "status": "stopped", "session_id": self.cfg["session_id"], "host_id": self.cfg["host_id"], "service": {"state": "stopped", "generation": None, "forced_escalation": False, "normal_exit": normal, "owned_generation": owned_generation, "cleanup_errors": []}, "listener": {"host": "127.0.0.1", "port": 18480, "owner_pid": None}}
