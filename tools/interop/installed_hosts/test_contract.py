# SPDX-License-Identifier: AGPL-3.0-only
import copy
import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from tools.interop.installed_hosts.contract import (
    ContractError,
    public_observation,
    read_registered_config,
    validate_control_request,
    validate_descriptor,
    validate_invitation,
    validate_package_manifest,
    payload_manifest_digest,
)
from tools.interop.installed_hosts.transport import build_ssh_argv, remote_session_paths
from tools.interop.installed_hosts.prepare import installation_commands


DIGEST = "1" * 64
SESSION_ID = "11111111-1111-4111-8111-111111111111"


def binding(path: str, digest: str = DIGEST) -> dict:
    return {"path": path, "sha256": digest}


def registration(host_id: str = "matrix-macos-fresh-1") -> dict:
    return {
        "schema_version": 1,
        "install_state": "installed",
        "host_id": host_id,
        "provider": "tart-macos",
        "provider_id": "matrix-macos-fresh-1",
        "provider_tool": binding("/Users/bolyki/.codex/artifacts/teslatlas-interop/2026-09-05-execution/tooling/tart-2.36.0/tart.app/Contents/MacOS/tart"),
        "ssh": {
            "executable": binding("/usr/bin/ssh"),
            "config": binding("/private/matrix/ssh_config"),
            "identity_file": binding("/private/matrix/id_ed25519"),
            "known_hosts": binding("/private/matrix/known_hosts"),
            "host_alias": "matrix-macos-fresh-1",
            "hostname": "127.0.0.1",
            "host_key_sha256": "2" * 64,
            "port": 22,
            "user": "matrix",
            "login_uid": 501,
        },
        "guest": {
            "machine_identity_sha256": "3" * 64,
            "hardware_identity_sha256": "4" * 64,
            "hypervisor_backend": "Virtualization.framework",
            "hypervisor_evidence_sha256": "5" * 64,
            "image_identity_sha256": "6" * 64,
            "os": "macOS",
            "architecture": "arm64",
            "native_or_emulated": "native",
            "python": binding("/usr/bin/python3"),
            "python_process": binding("/usr/bin/python3"),
            "python_version": "3.9.6",
            "python_toml_module": "tomli",
            "permitted_service_user": "matrix",
            "permitted_service_uid": 501,
        },
        "lease": {
            "lease_id": "lease-macos-fresh-1",
            "resource_id": "matrix-macos-fresh-1",
            "expires_at_unix": 2_000_000_000,
        },
        "protected_paths": [
            "/Users/admin/interop-upgrade-retained-20260905",
            "/home/bolyki.guest/interop-upgrade-retained-release",
            "/var/lib/teslatlas-hub/interop-upgrade-retained-v2026.36.1",
        ],
    }


def config(registration_path: str, registration_digest: str) -> dict:
    return {
        "schema_version": 1,
        "kind": "installed-host",
        "run_id": "run-20260905",
        "cell_id": "protocol_actual_hub__macos_arm64",
        "adapter_id": "protocol_actual_hub",
        "client_id": "protocol_actual_hub",
        "session_id": SESSION_ID,
        "host_id": "matrix-macos-fresh-1",
        "host_registration": binding(registration_path, registration_digest),
        "controller_bundle": binding("/private/input/controller.tar"),
        "package": binding("/private/input/teslatlas.pkg"),
        "package_manifest": binding("/private/input/package-manifest.json"),
        "expected": {
            "os": "macOS",
            "architecture": "arm64",
            "native_or_emulated": "native",
            "service_mode": "installed-app-launchagent",
            "product_version": "2026.36.2",
            "hub_executable_sha256": "7" * 64,
        },
        "seed": binding("/private/input/seed"),
        "profile": binding("/private/input/SHA256SUMS"),
        "scenario": binding("/private/input/scenario.json"),
        "local_private_root": "/private/output/installed-host",
        "guest_run_id": "run-20260905/protocol_actual_hub__macos_arm64",
        "lifetime_seconds": 900,
        "allowed_origins": ["http://127.0.0.1:18481", "http://localhost:18481"],
    }


class ContractTests(unittest.TestCase):
    def test_package_manifest_keeps_runtime_source_export_distinct_from_git_identity(self):
        members = [
            {"path": "/usr/bin/teslatlas-hub", "sha256": "b" * 64},
            {"path": "/usr/lib/systemd/system/teslatlas-hub.service", "sha256": "c" * 64},
        ]
        value = {
            "schema_version": 1, "kind": "installed-host-package",
            "package_sha256": "a" * 64, "product_version": "2026.36.2",
            "package_manager_version": "2026.36.2-1",
            "os": "Debian 13", "architecture": "amd64",
            "hub_executable": {"path": "/usr/bin/teslatlas-hub", "sha256": "b" * 64},
            "payload_members": members,
            "payload_manifest_sha256": payload_manifest_digest(members),
            "source_export_sha256": "d" * 64,
            "source_manifest_sha256": "e" * 64,
            "build_manifest_sha256": "f" * 64,
            "store_schema_version": 59,
            "platform_payload": {"hub_executable": members[0], "systemd_unit": members[1]},
            "package_scripts": {},
        }
        self.assertEqual(validate_package_manifest(value)["source_export_sha256"], "d" * 64)
        copied = copy.deepcopy(value)
        copied["git_commit"] = "a" * 40
        with self.assertRaisesRegex(ContractError, "unexpected fields: git_commit"):
            validate_package_manifest(copied)

    def test_package_manifest_rejects_missing_platform_payload(self):
        members = [{"path": "/usr/bin/teslatlas-hub", "sha256": "b" * 64}]
        value = {
            "schema_version": 1, "kind": "installed-host-package",
            "package_sha256": "a" * 64, "product_version": "2026.36.2",
            "package_manager_version": "2026.36.2-1", "store_schema_version": 59,
            "os": "Debian 13", "architecture": "amd64",
            "hub_executable": members[0], "payload_members": members,
            "payload_manifest_sha256": payload_manifest_digest(members),
            "source_export_sha256": "d" * 64, "source_manifest_sha256": "e" * 64,
            "build_manifest_sha256": "f" * 64,
            "package_scripts": {},
        }
        with self.assertRaisesRegex(ContractError, "platform_payload"):
            validate_package_manifest(value)

    def test_macos_manifest_separates_installed_payload_from_archived_template(self):
        members = [
            binding("/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub", "a" * 64),
            binding("/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh", "b" * 64),
            binding("/Applications/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub", "c" * 64),
            binding("/Applications/Teslatlas Hub.app/Contents/Info.plist", "d" * 64),
        ]
        value = {
            "schema_version": 1, "kind": "installed-host-package", "package_sha256": "e" * 64,
            "product_version": "2026.36.2", "package_manager_version": "2026.36.2",
            "store_schema_version": 59, "os": "macOS", "architecture": "arm64",
            "hub_executable": members[0], "platform_payload": {
                "hub_executable": members[0], "wrapper_script": members[1],
                "app_executable": members[2], "app_info_plist": members[3],
            },
            "payload_members": members, "payload_manifest_sha256": payload_manifest_digest(members),
            "package_scripts": {"launchagent_template": {"archive_path": "Scripts/com.teslatlas.hub.plist.in", "sha256": "f" * 64}},
            "source_export_sha256": "1" * 64, "source_manifest_sha256": "2" * 64,
            "build_manifest_sha256": "3" * 64,
        }
        checked = validate_package_manifest(value)
        self.assertNotIn(checked["package_scripts"]["launchagent_template"], checked["payload_members"])
        changed = copy.deepcopy(value); changed["package_scripts"]["launchagent_template"]["archive_path"] = "/installed/template"
        with self.assertRaisesRegex(ContractError, "actual package script member"):
            validate_package_manifest(changed)

    def write_registered(self, directory: Path, value=None):
        value = registration() if value is None else value
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
        path = directory / "registration.json"
        path.write_bytes(raw)
        digest = hashlib.sha256(raw).hexdigest()
        cfg = config(str(path), digest)
        return path, digest, cfg

    def test_registered_config_binds_inventory_and_exact_registration_bytes(self):
        with tempfile.TemporaryDirectory() as raw:
            path, digest, cfg = self.write_registered(Path(raw))
            loaded = read_registered_config(cfg, {cfg["host_id"]: digest})
            self.assertEqual(loaded.registration["host_id"], cfg["host_id"])
            self.assertEqual(loaded.registration_sha256, digest)
            path.write_text(path.read_text() + "\n")
            with self.assertRaisesRegex(ContractError, "host_registration.sha256"):
                read_registered_config(cfg, {cfg["host_id"]: digest})

    def test_registration_inventory_cannot_authorize_a_different_host(self):
        with tempfile.TemporaryDirectory() as raw:
            _, digest, cfg = self.write_registered(Path(raw))
            with self.assertRaisesRegex(ContractError, "verified registration inventory"):
                read_registered_config(cfg, {"some-other-host": digest})

    def test_wrong_architecture_json_types_are_rejected_before_registration_use(self):
        for wrong in (["arm64"], {"value": "arm64"}, None, 1):
            with self.subTest(wrong=wrong), tempfile.TemporaryDirectory() as raw:
                _, digest, cfg = self.write_registered(Path(raw))
                cfg["expected"]["architecture"] = wrong
                with self.assertRaisesRegex(ContractError, "expected.architecture"):
                    read_registered_config(cfg, {cfg["host_id"]: digest})

    def test_extra_fields_and_command_catalogs_are_rejected(self):
        with tempfile.TemporaryDirectory() as raw:
            _, digest, cfg = self.write_registered(Path(raw))
            cfg["argv"] = ["ssh", "host", "anything"]
            with self.assertRaisesRegex(ContractError, "unexpected fields: argv"):
                read_registered_config(cfg, {cfg["host_id"]: digest})

    def test_retained_predecessor_guest_and_path_are_refused(self):
        for host_id in (
            "teslatlas-interop-macos13-20260905",
            "teslatlas-interop-debian13-arm64",
            "teslatlas-interop-debian13-amd64",
        ):
            with self.subTest(host_id=host_id), tempfile.TemporaryDirectory() as raw:
                reg = registration(host_id)
                reg["provider_id"] = host_id
                reg["lease"]["resource_id"] = host_id
                _, digest, cfg = self.write_registered(Path(raw), reg)
                cfg["host_id"] = host_id
                with self.assertRaisesRegex(ContractError, "retained predecessor"):
                    read_registered_config(cfg, {host_id: digest})
        with tempfile.TemporaryDirectory() as raw:
            _, digest, cfg = self.write_registered(Path(raw))
            cfg["local_private_root"] = "/var/lib/teslatlas-hub/interop-upgrade-retained-v2026.36.1/new"
            with self.assertRaisesRegex(ContractError, "protected predecessor path"):
                read_registered_config(cfg, {cfg["host_id"]: digest})

    def test_provider_platform_and_service_mode_must_match(self):
        with tempfile.TemporaryDirectory() as raw:
            _, digest, cfg = self.write_registered(Path(raw))
            cfg["expected"]["service_mode"] = "installed-deb-systemd"
            with self.assertRaisesRegex(ContractError, "provider/platform/service binding"):
                read_registered_config(cfg, {cfg["host_id"]: digest})

    def test_control_requests_expose_only_five_operations(self):
        base = {
            "schema_version": 1,
            "session_id": SESSION_ID,
            "sequence": 1,
            "challenge": "8" * 64,
        }
        for op in ("verify", "stop", "start", "pair"):
            self.assertEqual(validate_control_request({**base, "op": op})["op"], op)
        device = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        self.assertEqual(
            validate_control_request({**base, "op": "revoke", "device_id": device})["device_id"],
            device,
        )
        for request in (
            {**base, "op": "restart"},
            {**base, "op": "verify", "command": "uname -a"},
            {**base, "op": "start", "device_id": device},
            {**base, "op": "revoke", "device_id": "not-a-uuid"},
        ):
            with self.subTest(request=request), self.assertRaises(ContractError):
                validate_control_request(request)
        for wrong in (True, 1.0):
            with self.subTest(wrong=wrong), self.assertRaises(ContractError):
                validate_control_request({**base, "schema_version": wrong, "op": "verify"})

    def test_descriptor_contains_no_proof_or_command_authority(self):
        descriptor = {
            "schema_version": 1,
            "kind": "installed-host",
            "broker_socket": "/private/output/broker.sock",
            "session_id": SESSION_ID,
            "registration_sha256": "9" * 64,
        }
        self.assertEqual(validate_descriptor(descriptor), descriptor)
        for key, value in (("proof", {}), ("hostname", "guest"), ("argv", ["id"])):
            with self.subTest(key=key), self.assertRaises(ContractError):
                validate_descriptor({**descriptor, key: value})

    def test_invitation_exactly_binds_fixed_endpoint_and_tls_pin(self):
        pairing_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        pin = "b" * 64
        value = {
            "pairingId": pairing_id,
            "secret": "private",
            "expiresAtMs": 2_000_000_000_000,
            "endpoint": "https://127.0.0.1:18480",
            "tlsPin": pin,
            "pairingUri": "teslatlas-hub://pair?endpoint=https%3A%2F%2F127.0.0.1%3A18480&pairing_id={}&secret=private&tls_pin={}".format(pairing_id, pin),
        }
        self.assertEqual(validate_invitation(value), value)
        with self.assertRaisesRegex(ContractError, "exactly bind"):
            validate_invitation({**value, "pairingUri": value["pairingUri"].replace(pin, "c" * 64)})

    def test_public_observation_redacts_secret_material_recursively(self):
        private = {
            "status": "verified",
            "host": {"host_id": "fresh"},
            "invitation": {"secret": "private"},
            "tls_private_key": "private",
            "config": {"path": "/fixed/config", "raw_config": "private", "sha256": DIGEST},
            "argv": ["private"],
        }
        self.assertEqual(
            public_observation(private),
            {"status": "verified", "host": {"host_id": "fresh"}, "config": {"path": "/fixed/config", "sha256": DIGEST}},
        )

    def test_ssh_transport_uses_only_registered_pinned_inputs(self):
        reg = registration()
        argv = build_ssh_argv(reg)
        self.assertEqual(argv[0], "/usr/bin/ssh")
        self.assertIn("UserKnownHostsFile=/private/matrix/known_hosts", argv)
        self.assertIn("/private/matrix/id_ed25519", argv)
        self.assertIn("matrix@matrix-macos-fresh-1", argv)
        self.assertIn("none", argv)
        self.assertNotIn("sudo", argv)
        paths = remote_session_paths(reg, SESSION_ID)
        self.assertEqual(paths["entrypoint"], paths["root"] + "/installed_hosts/guest.py")
        self.assertNotIn("teslatlas-interop-macos13-20260905", paths["root"])

    def test_preparation_install_commands_are_platform_fixed_and_leave_service_stopped(self):
        linux = registration("fresh-linux")
        linux["provider"] = "lima-debian"
        linux["provider_tool"]["path"] = "/opt/homebrew/bin/limactl"
        linux["provider_id"] = "fresh-linux"
        linux["ssh"]["host_alias"] = "lima-fresh-linux"
        linux["lease"]["resource_id"] = "fresh-linux"
        linux["guest"].update(os="Debian 13", architecture="amd64", native_or_emulated="emulated", permitted_service_user="teslatlas", permitted_service_uid=997)
        commands = installation_commands(linux, "/tmp/teslatlas-package-" + "a" * 64 + ".deb")
        self.assertEqual(commands[0], ["/usr/bin/sudo", "-n", "/bin/systemctl", "stop", "teslatlas-hub.service"])
        self.assertEqual(commands[1], ["/usr/bin/sudo", "-n", "/usr/bin/dpkg", "--install", "/tmp/teslatlas-package-" + "a" * 64 + ".deb"])
        self.assertEqual(len(commands), 2)
        with self.assertRaises(ValueError):
            installation_commands(linux, "/tmp/other.deb")


if __name__ == "__main__":
    unittest.main()
