# SPDX-License-Identifier: AGPL-3.0-only
"""Explicit operator-only installation recipe for a registered disposable guest."""
import argparse
import base64
import hashlib
import json
import os
import re
import shlex
import time
import xml.etree.ElementTree as ET
from pathlib import Path

from .contract import ContractError, read_registered_config, validate_package_manifest
from .transport import _hash_regular, _verify_provider_tool, _verify_known_host, build_ssh_argv
from .bounded import Deadline, run_capped
from .lease import HostLease


_REMOTE_PACKAGE = re.compile(r"^/tmp/teslatlas-package-([0-9a-f]{64})\.(deb|pkg)$")


def installation_commands(registration, remote_package):
    match = _REMOTE_PACKAGE.fullmatch(remote_package)
    if match is None:
        raise ValueError("remote package path is not controller-derived")
    if registration["provider"] == "lima-debian":
        if match.group(2) != "deb":
            raise ValueError("Debian guest requires a .deb package")
        return [
            ["/usr/bin/sudo", "-n", "/bin/systemctl", "stop", "teslatlas-hub.service"],
            ["/usr/bin/sudo", "-n", "/usr/bin/dpkg", "--install", remote_package],
        ]
    if registration["provider"] == "tart-macos":
        if match.group(2) != "pkg":
            raise ValueError("macOS guest requires a .pkg package")
        uid = registration["ssh"]["login_uid"]
        return [
            ["/bin/launchctl", "bootout", "gui/{}/com.teslatlas.hub".format(uid)],
            ["/usr/bin/sudo", "-n", "/usr/sbin/installer", "-pkg", remote_package, "-target", "/"],
        ]
    raise ValueError("unsupported registered provider")


class HostPreparer:
    def __init__(self, registered):
        self.registered = registered
        self.base = build_ssh_argv(registered.registration)
        self.deadline = None
        self._active_install = None
        self._failure_unwound = False
        self.provider_identity = None

    def _remote(self, argv, timeout, allowed_status=(0,), input_bytes=None):
        command = shlex.join(argv)
        return run_capped(self.base + [command], Deadline(min(timeout, self.deadline.remaining())), input_bytes=input_bytes, maximum=262_144, allowed_status=allowed_status)

    def _transfer(self, raw, remote_path):
        provider = self.registered.registration["provider"]
        decoder = ["/usr/bin/base64", "-D"] if provider == "tart-macos" else ["/usr/bin/base64", "-d"]
        command = shlex.join(decoder) + " > " + shlex.quote(remote_path)
        result = run_capped(self.base + [command], Deadline(min(300, self.deadline.remaining())), input_bytes=base64.b64encode(raw), maximum=262_144)
        if result.stdout:
            raise RuntimeError("base64 package transfer failed")

    def _preflight_absent(self):
        reg = self.registered.registration
        if reg["install_state"] != "preinstall":
            raise ContractError("operator preparation requires a preinstall registration")
        if reg["provider"] == "lima-debian":
            machine = self._remote(["/bin/cat", "/etc/machine-id"], 10).stdout
            os_release = self._remote(["/bin/cat", "/etc/os-release"], 10).stdout
            architecture = self._remote(["/usr/bin/dpkg", "--print-architecture"], 10).stdout.decode().strip()
            account = self._remote(["/usr/bin/id", "-u", "teslatlas"], 10, (0, 1))
            package = self._remote(["/usr/bin/dpkg-query", "-W", "teslatlas-hub"], 10, (0, 1))
            unit = self._remote(["/usr/bin/test", "!", "-e", "/usr/lib/systemd/system/teslatlas-hub.service"], 10)
            parsed = dict(line.split("=", 1) for line in os_release.decode().splitlines() if "=" in line)
            if hashlib.sha256(machine).hexdigest() != reg["guest"]["machine_identity_sha256"] or parsed.get("ID", "").strip('"') != "debian" or parsed.get("VERSION_ID", "").strip('"') != "13" or architecture != self.registered.config["expected"]["architecture"] or account.returncode != 1 or package.returncode != 1 or unit.returncode != 0:
                raise RuntimeError("Debian preinstall host/account/package/unit state differs")
            return {"machine_identity_sha256": hashlib.sha256(machine).hexdigest(), "architecture": architecture, "account_absent": True, "package_absent": True, "unit_absent": True}
        ioreg = self._remote(["/usr/sbin/ioreg", "-rd1", "-c", "IOPlatformExpertDevice"], 10).stdout
        version = self._remote(["/usr/bin/sw_vers", "-productVersion"], 10).stdout.decode().strip()
        architecture = self._remote(["/usr/bin/uname", "-m"], 10).stdout.decode().strip()
        login_uid = int(self._remote(["/usr/bin/id", "-u"], 10).stdout)
        console_uid = int(self._remote(["/usr/bin/stat", "-f", "%u", "/dev/console"], 10).stdout)
        absent = [
            "/Applications/Teslatlas Hub.app", "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub",
            "/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh",
            "/Users/{}/Library/LaunchAgents/com.teslatlas.hub.plist".format(reg["ssh"]["user"]),
        ]
        for path in absent:
            self._remote(["/usr/bin/test", "!", "-e", path], 10)
        service_receipt = self._remote(["/usr/sbin/pkgutil", "--pkg-info", "com.teslatlas.hub.service"], 10, (0, 1))
        app_receipt = self._remote(["/usr/sbin/pkgutil", "--pkg-info", "com.teslatlas.hub.app"], 10, (0, 1))
        loaded = self._remote(["/bin/launchctl", "print", "gui/{}/com.teslatlas.hub".format(reg["ssh"]["login_uid"])], 10, (0, 113))
        if hashlib.sha256(ioreg).hexdigest() != reg["guest"]["machine_identity_sha256"] or version.split(".")[0] != "13" or architecture != "arm64" or login_uid != reg["ssh"]["login_uid"] or console_uid != login_uid or service_receipt.returncode == 0 or app_receipt.returncode == 0 or loaded.returncode == 0:
            raise RuntimeError("macOS preinstall host/package/service state differs")
        return {"machine_identity_sha256": hashlib.sha256(ioreg).hexdigest(), "os_version": version, "architecture": architecture, "login_uid": login_uid, "console_uid": console_uid, "service_package_absent": True, "app_package_absent": True, "app_absent": True, "service_payload_absent": True, "launchagent_absent": True}

    def _observe_stopped(self):
        reg = self.registered.registration
        if reg["provider"] == "lima-debian":
            raw = self._remote(["/bin/systemctl", "show", "--no-page", "--property=LoadState,ActiveState,SubState,MainPID,ControlGroup", "teslatlas-hub.service"], 10).stdout.decode()
            values = dict(line.split("=", 1) for line in raw.splitlines() if "=" in line)
            sockets = self._remote(["/usr/bin/ss", "-H", "-ltn", "sport", "=", ":18480"], 10)
            if values.get("LoadState") != "loaded" or values.get("ActiveState") != "inactive" or values.get("SubState") != "dead" or values.get("MainPID") != "0" or values.get("ControlGroup") not in ("", None) or sockets.stdout:
                raise RuntimeError("installed Debian package is not independently stopped")
        else:
            launch = self._remote(["/bin/launchctl", "print", "gui/{}/com.teslatlas.hub".format(reg["ssh"]["login_uid"])], 10, (0, 113))
            sockets = self._remote(["/usr/sbin/lsof", "-nP", "-iTCP:18480", "-sTCP:LISTEN", "-FnPT"], 10, (0, 1))
            if launch.returncode == 0 or (sockets.returncode == 0 and sockets.stdout):
                raise RuntimeError("installed macOS package is not independently stopped")
        return {"service_loaded": False, "listener_owner": None}

    def _inspect_macos_components(self, remote_package, manifest):
        expanded = remote_package + ".expanded"
        self._remote(["/usr/bin/test", "!", "-e", expanded], 10)
        # The deterministic expansion name becomes an owned cleanup resource
        # before pkgutil gets its first opportunity to create it.
        self._obligation("expansion-pending", remote_package, expanded, installer_state="not-started")
        self._active_install = {
            "remote_package": remote_package, "expanded": expanded,
            "remote_receipt": None, "installer_invoked": False,
            "installer_complete": True,
        }
        self._remote(["/usr/sbin/pkgutil", "--expand-full", remote_package, expanded], 60)
        paths = self._remote(["/usr/bin/find", expanded, "-type", "f", "-name", "PackageInfo", "-print"], 15).stdout.decode().splitlines()
        components = []
        service_dirs = []
        for path in paths:
            raw = self._remote(["/bin/cat", path], 10).stdout
            root = ET.fromstring(raw)
            identifier, version = root.attrib.get("identifier"), root.attrib.get("version")
            if not identifier or not version:
                raise RuntimeError("combined macOS PackageInfo lacks component identity")
            components.append({"identifier": identifier, "version": version})
            if identifier == "com.teslatlas.hub.service":
                service_dirs.append(str(Path(path).parent))
        if len(service_dirs) != 1 or len({item["identifier"] for item in components}) != len(components):
            raise RuntimeError("combined macOS package has ambiguous component identities")
        if not any(item["identifier"] == "com.teslatlas.hub.app" for item in components):
            raise RuntimeError("combined macOS package lacks required App component")
        template = service_dirs[0] + "/Scripts/com.teslatlas.hub.plist.in"
        digest = self._remote(["/usr/bin/shasum", "-a", "256", template], 10).stdout.decode().split()[0]
        if digest != manifest["package_scripts"]["launchagent_template"]["sha256"]:
            raise RuntimeError("archived LaunchAgent template differs from package manifest")
        return expanded, template, sorted(components, key=lambda item: item["identifier"]), digest

    def execute(self):
        self.deadline = Deadline(420)
        lease = HostLease(self.registered).acquire(420)
        try:
            try:
                if self.provider_identity is not None:
                    self.provider_identity.revalidate(self.deadline)
                return self._execute_locked()
            except BaseException as error:
                if self._active_install is not None and not self._failure_unwound:
                    active = self._active_install
                    try:
                        unwind = self._unwind_failed_install(**active)
                    except BaseException as unwind_error:
                        unwind = [{"resource":"unwind-orchestration","error":type(unwind_error).__name__}]
                    errors = [{"resource": "preparation", "error": type(error).__name__}] + unwind
                    try:
                        self._obligation(
                            "failed-handoff", active["remote_package"], active["expanded"], errors,
                            remote_receipt=active["remote_receipt"],
                            installer_state="complete" if active["installer_complete"] else "completion-unproved",
                        )
                    except BaseException as obligation_error:
                        errors.append({"resource":"local-obligation","error":type(obligation_error).__name__})
                    lease.quarantine(errors)
                raise
        finally:
            lease.release()

    def _obligation(self, phase, remote_package, expanded=None, errors=None, remote_receipt=None, installer_state=None):
        root = Path(self.registered.config["local_private_root"]) / self.registered.config["session_id"]
        root.mkdir(mode=0o700, parents=True, exist_ok=True)
        path = root / "preparation-obligation.json"
        value = {
            "schema_version": 1, "host_id": self.registered.config["host_id"],
            "package_sha256": self.registered.config["package"]["sha256"],
            "phase": phase, "remote_package": remote_package, "expanded": expanded,
            "remote_receipt": remote_receipt, "installer_state": installer_state,
            "errors": list(errors or []),
        }
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
        os.chmod(str(temporary), 0o600)
        with temporary.open("rb") as handle:
            os.fsync(handle.fileno())
        os.replace(str(temporary), str(path))
        directory = os.open(str(root), os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        return path

    def _unwind_failed_install(self, remote_package, expanded, installer_invoked, remote_receipt=None, installer_complete=True):
        errors = []
        # This recovery budget is independent of the failed installation's
        # already-consumed deadline. It owns only the package's fixed resources.
        saved = self.deadline
        self.deadline = Deadline(45)
        try:
            if installer_invoked and not installer_complete:
                # Losing the local SSH process says nothing about the remote
                # package manager or its scripts. A new writer must not race it.
                return [{"resource": "remote-installer", "error": "CompletionUnproved"}]
            if installer_invoked:
                try:
                    stop = installation_commands(self.registered.registration, remote_package)[0]
                    if self.registered.registration["provider"] == "lima-debian":
                        self._remote(stop, 15, (0, 5))
                    else:
                        self._remote(stop, 15, (0, 3, 113))
                    self._observe_stopped()
                except BaseException as error:
                    errors.append({"resource": "postinstall-service", "error": type(error).__name__})
            if expanded is not None:
                try:
                    self._remote(["/usr/bin/find", expanded, "-depth", "-delete"], 12)
                except BaseException as error:
                    errors.append({"resource": "expanded", "error": type(error).__name__})
            try:
                self._remote(["/bin/rm", "-f", remote_package], 8)
            except BaseException as error:
                errors.append({"resource": "remote-package", "error": type(error).__name__})
            if remote_receipt is not None:
                try:
                    self._remote(["/bin/rm", "-f", remote_receipt], 8)
                except BaseException as error:
                    errors.append({"resource": "remote-receipt", "error": type(error).__name__})
        finally:
            self.deadline = saved
        return errors

    def _execute_locked(self):
        cfg = self.registered.config
        reg = self.registered.registration
        preflight = self._preflight_absent()
        package = _hash_regular(cfg["package"], "session.package")
        manifest_path = _hash_regular(cfg["package_manifest"], "session.package_manifest")
        manifest = validate_package_manifest(json.loads(manifest_path.read_text(encoding="utf-8")))
        expected_path = "/usr/bin/teslatlas-hub" if reg["provider"] == "lima-debian" else "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
        if (manifest["package_sha256"], manifest["product_version"], manifest["os"], manifest["architecture"], manifest["hub_executable"]["path"], manifest["hub_executable"]["sha256"]) != (cfg["package"]["sha256"], cfg["expected"]["product_version"], cfg["expected"]["os"], cfg["expected"]["architecture"], expected_path, cfg["expected"]["hub_executable_sha256"]):
            raise ContractError("package manifest does not bind selected guest and package")
        extension = "deb" if reg["provider"] == "lima-debian" else "pkg"
        remote_package = "/tmp/teslatlas-package-{}.{}".format(cfg["package"]["sha256"], extension)
        obligation = self._obligation("transfer-pending", remote_package)
        installer_invoked = False
        self._active_install = {
            "remote_package": remote_package, "expanded": None,
            "remote_receipt": None, "installer_invoked": False,
            "installer_complete": True,
        }
        self._transfer(package.read_bytes(), remote_package)
        digest_argv = ["/usr/bin/sha256sum", remote_package] if reg["provider"] == "lima-debian" else ["/usr/bin/shasum", "-a", "256", remote_package]
        observed = self._remote(digest_argv, 30).stdout.decode("utf-8").split()[0]
        if observed != cfg["package"]["sha256"]:
            raise RuntimeError("guest package bytes differ after transfer")
        commands = installation_commands(reg, remote_package)
        components = []
        template_digest = None
        rendered_digest = None
        expanded = None
        template = None
        try:
            if reg["provider"] == "tart-macos":
                expanded, template, components, template_digest = self._inspect_macos_components(remote_package, manifest)
                self._active_install.update(expanded=expanded)
                self._obligation("installer-pending", remote_package, expanded, installer_state="not-started")
                status = self._remote(["/bin/launchctl", "print", "gui/{}/com.teslatlas.hub".format(reg["ssh"]["login_uid"])], 15, (0, 113))
                if status.returncode == 0:
                    self._remote(commands[0], 30)
            # Debian preflight proved that the unit is absent. There is no
            # pre-install stop command on that fresh state.
            self._obligation("installer-running", remote_package, expanded, installer_state="running-or-unknown")
            installer_invoked = True
            self._active_install.update(installer_invoked=True, installer_complete=False)
            # Status 255 is reserved by SSH for a transport failure. Any
            # returned 0..254 status proves the remote command completed.
            install = self._remote(commands[1], 300, tuple(range(255)))
            self._active_install["installer_complete"] = True
            self._obligation("installer-complete", remote_package, expanded, installer_state="complete")
            if install.returncode != 0:
                raise RuntimeError("remote installer failed with status {}".format(install.returncode))
        except BaseException:
            # execute() owns the one unwind/quarantine transition while the
            # canonical resource lease is still held.
            raise
        # Package scripts may start the service. The prepared target is always
        # handed to the session in independently verified stopped state.
        if reg["provider"] == "lima-debian":
            self._remote(commands[0], 30)
            package_state = self._remote(["/usr/bin/dpkg-query", "-W", "-f=${Version}", "teslatlas-hub"], 15).stdout.decode("utf-8")
        else:
            status = self._remote(["/bin/launchctl", "print", "gui/{}/com.teslatlas.hub".format(reg["ssh"]["login_uid"])], 15, (0, 113))
            if status.returncode == 0:
                self._remote(commands[0], 30)
            info = self._remote(["/usr/sbin/pkgutil", "--pkg-info-plist", "com.teslatlas.hub.service"], 15).stdout
            import plistlib
            package_state = plistlib.loads(info)["pkg-version"]
            installed_plist = "/Users/{}/Library/LaunchAgents/com.teslatlas.hub.plist".format(reg["ssh"]["user"])
            render_code = "from pathlib import Path; import sys,functools; t=Path(sys.argv[1]).read_text(); h=sys.argv[3]; r={'@SUPERVISOR@':'/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh','@CONFIG@':h+'/Library/Application Support/Teslatlas Hub/config.toml','@STDOUT@':h+'/Library/Logs/Teslatlas Hub/hub.out.log','@STDERR@':h+'/Library/Logs/Teslatlas Hub/hub.err.log','@BINARY@':'/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub'}; t=functools.reduce(lambda s,kv:s.replace(*kv),r.items(),t); raise SystemExit(0 if t.encode()==Path(sys.argv[2]).read_bytes() else 1)"
            self._remote([reg["guest"]["python"]["path"], "-B", "-c", render_code, template, installed_plist, "/Users/" + reg["ssh"]["user"]], 15)
            rendered_digest = self._remote(["/usr/bin/shasum", "-a", "256", installed_plist], 10).stdout.decode().split()[0]
        if package_state != manifest["package_manager_version"]:
            raise RuntimeError("installed package-manager version differs from package manifest")
        observed_binary = self._remote(digest_argv[:-1] + [expected_path] if reg["provider"] == "tart-macos" else ["/usr/bin/sha256sum", expected_path], 30).stdout.decode("utf-8").split()[0]
        if observed_binary != cfg["expected"]["hub_executable_sha256"]:
            raise RuntimeError("installed executable digest differs from package manifest")
        version_output = self._remote([expected_path, "--version"], 15).stdout.decode("utf-8")
        if cfg["expected"]["product_version"] not in version_output:
            raise RuntimeError("installed executable version differs from session")
        stopped_observation = self._observe_stopped()
        for member in manifest["payload_members"]:
            member_digest_argv = ["/usr/bin/sha256sum", member["path"]] if reg["provider"] == "lima-debian" else ["/usr/bin/shasum", "-a", "256", member["path"]]
            actual = self._remote(member_digest_argv, 30).stdout.decode("utf-8").split()[0]
            if actual != member["sha256"]:
                raise RuntimeError("installed payload member differs from package manifest")
        receipt = {
            "schema_version": 1,
            "package_sha256": cfg["package"]["sha256"],
            "package_manifest_sha256": cfg["package_manifest"]["sha256"],
            "payload_manifest_sha256": manifest["payload_manifest_sha256"],
            "installed_version": package_state,
            "provider_id": reg["provider_id"],
            "install_exit_code": install.returncode,
            "package_components": components,
            "launchagent_template_sha256": template_digest,
            "rendered_launchagent_sha256": rendered_digest,
            "preflight_sha256": hashlib.sha256(json.dumps(preflight, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
            "stopped_observation": stopped_observation,
        }
        receipt_raw = json.dumps(receipt, sort_keys=True, separators=(",", ":")).encode("utf-8")
        remote_receipt = "/tmp/teslatlas-install-receipt-{}.json".format(cfg["package"]["sha256"])
        self._active_install["remote_receipt"] = remote_receipt
        self._obligation(
            "receipt-transfer-pending", remote_package, expanded,
            remote_receipt=remote_receipt, installer_state="complete",
        )
        self._transfer(receipt_raw, remote_receipt)
        receipt_root = "/var/lib/teslatlas-hub/interop-package-receipts" if reg["provider"] == "lima-debian" else "/Library/Application Support/Teslatlas Hub/interop-package-receipts"
        self._remote(["/usr/bin/sudo", "-n", "/usr/bin/install", "-d", "-o", "root", "-g", "root" if reg["provider"] == "lima-debian" else "wheel", "-m", "0700", receipt_root], 15)
        final_receipt = receipt_root + "/" + cfg["package"]["sha256"] + ".json"
        self._remote(["/usr/bin/sudo", "-n", "/usr/bin/install", "-o", "root", "-g", "root" if reg["provider"] == "lima-debian" else "wheel", "-m", "0600", remote_receipt, final_receipt], 15)
        if expanded is not None:
            self._remote(["/usr/bin/find", expanded, "-depth", "-delete"], 30)
        self._remote(["/bin/rm", "-f", remote_package, remote_receipt], 15)
        local_root = Path(cfg["local_private_root"]) / cfg["session_id"]
        local_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        log = {
            "schema_version": 1,
            "host_id": cfg["host_id"],
            "package_sha256": cfg["package"]["sha256"],
            "manifest_sha256": cfg["package_manifest"]["sha256"],
            "installed_executable_sha256": observed_binary,
            "version_output_sha256": hashlib.sha256(version_output.encode("utf-8")).hexdigest(),
            "receipt_sha256": hashlib.sha256(receipt_raw).hexdigest(),
            "service_left_stopped": True,
            "preflight": preflight,
            "provider_tool_identity": self.provider_identity.as_dict() if self.provider_identity is not None else None,
            "registration_update": {"install_state": "installed", "permitted_service_uid": int(self._remote(["/usr/bin/id", "-u", reg["guest"]["permitted_service_user"]], 10).stdout)},
        }
        path = local_root / "preparation.json"
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(log, sort_keys=True, indent=2) + "\n", encoding="utf-8")
        os.chmod(str(temporary), 0o600)
        with temporary.open("rb") as handle:
            os.fsync(handle.fileno())
        os.replace(str(temporary), str(path))
        directory = os.open(str(local_root), os.O_RDONLY)
        try:
            os.fsync(directory)
            obligation.unlink()
            os.fsync(directory)
        finally:
            os.close(directory)
        self._active_install = None
        return log


def main(argv=None):
    parser = argparse.ArgumentParser(description="Install a frozen package in one registered disposable guest")
    parser.add_argument("--config", required=True)
    parser.add_argument("--inventory", required=True)
    parser.add_argument("--execute", action="store_true")
    args = parser.parse_args(argv)
    if not args.execute:
        parser.error("--execute is required; config validation never installs implicitly")
    config = json.loads(Path(args.config).read_text(encoding="utf-8"))
    inventory = json.loads(Path(args.inventory).read_text(encoding="utf-8"))
    registered = read_registered_config(config, inventory, required_install_state="preinstall")
    provider_identity = _verify_provider_tool(registered.registration)
    for key in ("executable", "config", "identity_file"):
        _hash_regular(registered.registration["ssh"][key], "registration.ssh." + key)
    _verify_known_host(registered.registration)
    preparer = HostPreparer(registered)
    preparer.provider_identity = provider_identity.revalidate()
    preparer.execute()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
