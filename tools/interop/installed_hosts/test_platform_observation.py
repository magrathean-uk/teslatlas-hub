# SPDX-License-Identifier: AGPL-3.0-only
import copy
import hashlib
import json
import unittest

from tools.interop.installed_hosts.contract import (
    ContractError,
    RegisteredConfig,
    generation_changed,
    validate_installed_observation,
)
from tools.interop.installed_hosts.linux import control_argv as linux_control_argv, parse_proc_stat, parse_systemd_show
from tools.interop.installed_hosts.macos import command_plan as macos_command_plan, validate_wrapper_child
from tools.interop.installed_hosts._common import process_tree_digest, validate_synthetic_config


D = "a" * 64
SESSION = "11111111-1111-4111-8111-111111111111"


def process(pid, uid, parent, executable, start, executable_sha=D):
    return {
        "pid": pid,
        "uid": uid,
        "parent_pid": parent,
        "boot_id": "boot-identity-1",
        "start_identity": start,
        "executable_path": executable,
        "executable_sha256": executable_sha,
        "argv_sha256": "b" * 64,
    }


def registered(os_name="Debian 13"):
    linux = os_name == "Debian 13"
    cfg = {
        "session_id": SESSION,
        "host_id": "fresh-debian" if linux else "fresh-macos",
        "controller_bundle": {"sha256": "c" * 64},
        "package": {"sha256": "d" * 64},
        "package_manifest": {"sha256": "e" * 64},
        "expected": {
            "os": os_name,
            "architecture": "amd64" if linux else "arm64",
            "native_or_emulated": "emulated" if linux else "native",
            "service_mode": "installed-deb-systemd" if linux else "installed-app-launchagent",
            "product_version": "2026.36.2",
            "hub_executable_sha256": "f" * 64,
        },
        "seed": {"sha256": "1" * 64},
        "profile": {"sha256": "2" * 64},
        "scenario": {"sha256": "3" * 64},
        "guest_run_id": "run-1/cell-1",
    }
    reg = {
        "install_state": "installed",
        "host_id": cfg["host_id"],
        "provider": "lima-debian" if linux else "tart-macos",
        "ssh": {"user": "matrix", "login_uid": 501},
        "guest": {
            "machine_identity_sha256": "4" * 64,
            "hypervisor_evidence_sha256": "5" * 64,
            "os": os_name,
            "architecture": cfg["expected"]["architecture"],
            "native_or_emulated": cfg["expected"]["native_or_emulated"],
            "python": {"path": "/usr/bin/python3", "sha256": "6" * 64},
            "python_process": {"path": "/usr/bin/python3", "sha256": "6" * 64},
            "permitted_service_uid": 997 if linux else 501,
        },
        "lease": {"lease_id": "synthetic", "resource_id": cfg["host_id"], "expires_at_unix": 2_000_000_000},
    }
    return RegisteredConfig(cfg, reg, "7" * 64)


def observation(os_name="Debian 13", start="1000:1"):
    linux = os_name == "Debian 13"
    r = registered(os_name)
    hub_path = "/usr/bin/teslatlas-hub" if linux else "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
    wrapper_path = hub_path if linux else "/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh"
    uid = r.registration["guest"]["permitted_service_uid"]
    supervisor = process(40, uid, 1, wrapper_path if linux else "/bin/bash", start, "f" * 64 if linux else "4" * 64)
    hub = process(40 if linux else 41, uid, 1 if linux else 40, hub_path, start, "f" * 64)
    service = {
        "mode": r.config["expected"]["service_mode"],
        "target": "teslatlas-hub.service" if linux else "gui/501/com.teslatlas.hub",
        "definition_sha256": "9" * 64,
        "state": "running",
        "generation": "boot-identity-1:" + start,
        "post_probe_generation": "boot-identity-1:" + start,
        "supervisor": supervisor,
        "hub": hub,
        "process_tree_sha256": process_tree_digest(hub) if linux else process_tree_digest(supervisor, hub),
    }
    if linux:
        fragment_sha = "a" * 64
        service.update(
            invocation_id="0123456789abcdef0123456789abcdef",
            control_group="/system.slice/teslatlas-hub.service",
            fragment_sha256=fragment_sha,
            definition_sha256=fragment_sha,
            drop_in_manifest_sha256=hashlib.sha256(b"[]").hexdigest(),
            result="success",
            exec_main_code="0",
            exec_main_status=0,
        )
        config_path = "/etc/teslatlas-hub/config.toml"
        data_dir = "/var/lib/teslatlas-hub/interop-matrix/run-1/cell-1/hub"
    else:
        app_process = process(30, 501, 1, "/Applications/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub", "900:1", "c" * 64)
        app_receipt = hashlib.sha256(json.dumps(app_process, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()
        service.update(
            plist_sha256="a" * 64,
            definition_sha256="a" * 64,
            wrapper_script_sha256="8" * 64,
            loaded_state="loaded",
            app_receipt_sha256=app_receipt,
            app_process=app_process,
        )
        config_path = "/Users/matrix/Library/Application Support/Teslatlas Hub/config.toml"
        data_dir = "/Users/matrix/Library/Application Support/Teslatlas Hub/InteropMatrix/run-1/cell-1/hub"
    return {
        "schema_version": 1,
        "status": "verified",
        "session_id": SESSION,
        "sequence": 3,
        "challenge": "d" * 64,
        "observer": {"bundle_sha256": "c" * 64, "process": process(20, 501, 1, "/usr/bin/python3", "800:1", "6" * 64)},
        "host": {
            "host_id": r.config["host_id"],
            "machine_identity_sha256": "4" * 64,
            "os": os_name,
            "os_version": "13.6" if linux else "13.7.4",
            "architecture": r.config["expected"]["architecture"],
            "kernel": "observed-kernel",
            "native_or_emulated": r.config["expected"]["native_or_emulated"],
            "hypervisor_evidence_sha256": "5" * 64,
        },
        "package": {
            "sha256": "d" * 64,
            "manifest_sha256": "e" * 64,
            "installed_version": "2026.36.2-1" if linux else "2026.36.2",
            "payload_manifest_sha256": "e" * 64,
            "installation_receipt_sha256": "f" * 64,
        },
        "service": service,
        "config": {
            "path": config_path,
            "sha256": "0" * 64,
            "data_dir": data_dir,
            "store_id": "hub-uuid-1",
            "store_schema_version": 59,
            "scenario_sha256": "3" * 64,
            "seed_sha256": "1" * 64,
        },
        "listener": {
            "host": "127.0.0.1",
            "port": 18480,
            "owner_pid": hub["pid"],
            "socket_identity": "tcp-inode-123",
            "observed_at_monotonic_ns": 123456789,
        },
        "tls": {
            "endpoint": "https://127.0.0.1:18480",
            "certificate_der_sha256": "1" * 64,
            "verified_chain": True,
            "verified_hostname": True,
            "redirect_count": 0,
        },
        "discovery": {
            "hub_id": "hub-uuid-1",
            "product_version": "2026.36.2",
            "response_sha256": "2" * 64,
            "profile_id": "hub-http-v1@1.0.0",
            "profile_sha256": "2" * 64,
            "validated_response_set_sha256": "3" * 64,
        },
    }


class PlatformObservationTests(unittest.TestCase):
    def test_synthetic_config_requires_fixed_tls_identity_paths(self):
        root = "/var/lib/teslatlas-hub/interop-matrix/run-1/cell-1"
        value = {
            "data_dir": root + "/hub",
            "bind": "127.0.0.1:18480",
            "tls": {
                "public_url": "https://127.0.0.1:18480",
                "certificate_path": root + "/server.pem",
                "private_key_path": root + "/server-key.pem",
            },
            "collector": {"interval_seconds": 0, "owner_api_base_url": "https://127.0.0.1:1/", "legacy_auth": {"enabled": False}},
            "terrain": {"enabled": False}, "geocoder": {"enabled": False},
            "http": {"allowed_origins": ["http://localhost:18481"]},
        }
        self.assertEqual(validate_synthetic_config(value, root, root + "/hub", ["http://localhost:18481"]), value["tls"])
        changed = copy.deepcopy(value)
        changed["tls"]["certificate_path"] = root + "/other.pem"
        with self.assertRaisesRegex(ValueError, "fixed fresh-cell identity"):
            validate_synthetic_config(changed, root, root + "/hub", ["http://localhost:18481"])

    def test_platform_control_commands_are_fixed_by_platform_code(self):
        self.assertEqual(linux_control_argv("start"), ["/usr/bin/sudo", "-n", "/bin/systemctl", "start", "teslatlas-hub.service"])
        self.assertEqual(linux_control_argv("stop"), ["/usr/bin/sudo", "-n", "/bin/systemctl", "stop", "teslatlas-hub.service"])
        with self.assertRaises(ValueError):
            linux_control_argv("restart")
        self.assertEqual(
            macos_command_plan("start", False, 501, "/Users/matrix/Library/LaunchAgents/com.teslatlas.hub.plist"),
            [["/bin/launchctl", "bootstrap", "gui/501", "/Users/matrix/Library/LaunchAgents/com.teslatlas.hub.plist"]],
        )
        self.assertEqual(macos_command_plan("stop", True, 501, "/ignored"), [["/bin/launchctl", "bootout", "gui/501/com.teslatlas.hub"]])
        with self.assertRaises(ValueError):
            macos_command_plan("restart", True, 501, "/ignored")

    def test_systemd_parser_rejects_missing_and_duplicate_machine_fields(self):
        valid = "\n".join(
            (
                "LoadState=loaded", "ActiveState=active", "SubState=running", "MainPID=40",
                "ControlGroup=/system.slice/teslatlas-hub.service", "InvocationID=0123456789abcdef0123456789abcdef",
                "FragmentPath=/usr/lib/systemd/system/teslatlas-hub.service", "DropInPaths=", "Result=success", "ExecMainCode=0", "ExecMainStatus=0",
            )
        )
        self.assertEqual(parse_systemd_show(valid)["MainPID"], "40")
        with self.assertRaisesRegex(ValueError, "missing systemd field"):
            parse_systemd_show(valid.replace("MainPID=40\n", ""))
        with self.assertRaisesRegex(ValueError, "duplicate systemd field"):
            parse_systemd_show(valid + "\nMainPID=41")

    def test_proc_stat_parser_handles_spaces_and_parentheses_in_comm(self):
        fields = ["R", "7"] + [str(number) for number in range(5, 23)]
        value = "41 (hub worker (matrix)) " + " ".join(fields)
        parsed = parse_proc_stat(value)
        self.assertEqual(parsed, {"pid": 41, "parent_pid": 7, "start_ticks": 22})

    def test_linux_observation_binds_package_process_listener_config_and_tls(self):
        value = observation()
        self.assertEqual(validate_installed_observation(value, registered())["service"]["hub"]["pid"], 40)
        mutations = (
            ("package mismatch", lambda v: v["package"].update(sha256="0" * 64)),
            ("wrong UID", lambda v: v["service"]["hub"].update(uid=501)),
            ("listener owner", lambda v: v["listener"].update(owner_pid=999)),
            ("config path", lambda v: v["config"].update(path="/tmp/config.toml")),
            ("redirect", lambda v: v["tls"].update(redirect_count=1)),
            ("post-probe generation", lambda v: v["service"].update(post_probe_generation="boot-identity-1:later")),
        )
        for name, mutate in mutations:
            with self.subTest(name=name):
                changed = copy.deepcopy(value)
                mutate(changed)
                with self.assertRaises(ContractError):
                    validate_installed_observation(changed, registered())

    def test_pid_reuse_is_not_a_new_generation(self):
        before = observation(start="1000:1")
        reused = observation(start="1000:1")
        reused["service"]["hub"]["pid"] = 99
        reused["service"]["supervisor"]["pid"] = 99
        reused["listener"]["owner_pid"] = 99
        self.assertFalse(generation_changed(before, reused))
        restarted = observation(start="2000:4")
        self.assertTrue(generation_changed(before, restarted))

    def test_macos_requires_fixed_wrapper_and_single_direct_hub_child(self):
        value = observation("macOS")
        self.assertEqual(validate_installed_observation(value, registered("macOS"))["service"]["hub"]["parent_pid"], 40)
        validate_wrapper_child(value["service"]["supervisor"], value["service"]["hub"], [41])
        for mutation in (
            lambda v: v["service"]["hub"].update(parent_pid=1),
            lambda v: v["service"]["supervisor"].update(executable_path="/bin/zsh"),
            lambda v: v["service"]["hub"].update(executable_path="/tmp/teslatlas-hub"),
        ):
            changed = copy.deepcopy(value)
            mutation(changed)
            with self.assertRaises(ContractError):
                validate_installed_observation(changed, registered("macOS"))
        with self.assertRaisesRegex(ValueError, "single direct Hub child"):
            validate_wrapper_child(value["service"]["supervisor"], value["service"]["hub"], [41, 42])

    def test_proof_rejects_boolean_redirect_count(self):
        value = observation()
        value["tls"]["redirect_count"] = False
        with self.assertRaises(ContractError):
            validate_installed_observation(value, registered())


if __name__ == "__main__":
    unittest.main()
