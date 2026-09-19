# SPDX-License-Identifier: AGPL-3.0-only
"""Pinned SSH stdio and byte-preserving TLS-forward transport."""
import base64
import hashlib
import json
import os
import re
import select
import shlex
import ssl
import stat
import subprocess
import tarfile
from pathlib import Path
from dataclasses import asdict, dataclass

from .contract import MAX_JSON_BYTES, ContractError
from .bounded import Deadline, IncrementalLineReader, run_capped, write_all, durable_bytes, cleanup_error
from ._common import PROFILE_MEMBERS, normal_tls_probe, validate_profile_bundle


GUEST_BUNDLE_MEMBERS = frozenset(
    (
        "installed_hosts/__init__.py",
        "installed_hosts/contract.py",
        "installed_hosts/_common.py",
        "installed_hosts/bounded.py",
        "installed_hosts/guest.py",
        "installed_hosts/lease.py",
        "installed_hosts/linux.py",
        "installed_hosts/macos.py",
        "installed_hosts/session.py",
    )
)


def build_ssh_argv(registration):
    ssh = registration["ssh"]
    return [
        ssh["executable"]["path"],
        "-F", ssh["config"]["path"],
        "-S", "none",
        "-i", ssh["identity_file"]["path"],
        "-p", str(ssh["port"]),
        "-o", "BatchMode=yes",
        "-o", "IdentitiesOnly=yes",
        "-o", "ForwardAgent=no",
        "-o", "ClearAllForwardings=yes",
        "-o", "StrictHostKeyChecking=yes",
        "-o", "UserKnownHostsFile=" + ssh["known_hosts"]["path"],
        "{}@{}".format(ssh["user"], ssh["host_alias"]),
    ]


def build_session_ssh_argv(registration):
    """Use no inherited SSH configuration, retaining one explicit reviewed forward."""
    ssh = registration["ssh"]
    return [
        ssh["executable"]["path"], "-F", "/dev/null", "-S", "none",
        "-i", ssh["identity_file"]["path"], "-p", str(ssh["port"]),
        "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes", "-o", "ForwardAgent=no",
        "-o", "StrictHostKeyChecking=yes", "-o", "UserKnownHostsFile=" + ssh["known_hosts"]["path"],
        "-o", "HostKeyAlias=" + ssh["host_alias"], "-o", "ExitOnForwardFailure=yes",
        "-L", "127.0.0.1:18480:127.0.0.1:18480",
        "{}@{}".format(ssh["user"], ssh["hostname"]),
    ]


def remote_session_paths(registration, session_id):
    if not isinstance(session_id, str) or len(session_id) != 36 or any(c not in "0123456789abcdef-" for c in session_id):
        raise ContractError("session_id is not a canonical UUID path component")
    root = "/tmp/teslatlas-installed-host-" + session_id
    return {
        "root": root,
        "archive": root + "/controller.tar",
        "session": root + "/session.json",
        "entrypoint": root + "/installed_hosts/guest.py",
    }


def _hash_regular(binding, label):
    path = Path(binding["path"])
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode):
        raise ContractError("{} must be a regular non-symlink file".format(label))
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if digest != binding["sha256"]:
        raise ContractError("{}.sha256 does not match exact bytes".format(label))
    return path


_PROVIDER_TOOL_PATHS = {
    "lima-debian": "/opt/homebrew/bin/limactl",
    "tart-macos": "/Users/bolyki/.codex/artifacts/teslatlas-interop/2026-09-05-execution/tooling/tart-2.36.0/tart.app/Contents/MacOS/tart",
}
_LIMA_LINK = re.compile(r"\.\./Cellar/lima/[0-9]+\.[0-9]+\.[0-9]+(?:_[0-9]+)?/bin/limactl")
_PROVIDER_MAX_BYTES = 128 * 1024 * 1024


def _provider_stat(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def _provider_directories(path, deadline):
    rows = []
    for directory in reversed(path.parents):
        deadline.remaining()
        info = directory.lstat()
        if not stat.S_ISDIR(info.st_mode):
            raise ContractError("provider executable parent must be an ordinary directory")
        rows.append((str(directory), info.st_dev, info.st_ino, info.st_mode))
    return tuple(rows)


@dataclass(frozen=True)
class ProviderToolIdentity:
    provider: str
    invocation_path: str
    resolved_path: str
    link_text: object
    sha256: str
    alias_identity: tuple
    target_identity: tuple
    directory_identities: tuple

    def as_dict(self):
        return asdict(self)

    def revalidate(self, deadline=None):
        current = _verify_provider_tool({"provider": self.provider,
            "provider_tool": {"path": self.invocation_path, "sha256": self.sha256}}, deadline)
        if current != self:
            raise ContractError("provider tool identity changed after observation")
        return current


def _verify_provider_tool(registration, deadline=None):
    """Admit only the fixed Tart file or single ordinary Homebrew Lima alias.

    The original alias remains invocation identity: Lima uses it for helper
    discovery. This provider-specific exception never changes _hash_regular.
    """
    deadline = Deadline(deadline.remaining(10)) if deadline is not None else Deadline(10)
    provider = registration.get("provider")
    binding = registration.get("provider_tool")
    if not isinstance(provider, str) or provider not in _PROVIDER_TOOL_PATHS or not isinstance(binding, dict) or set(binding) != {"path", "sha256"} or binding.get("path") != _PROVIDER_TOOL_PATHS[provider] or not isinstance(binding.get("sha256"), str) or re.fullmatch(r"[0-9a-f]{64}", binding["sha256"]) is None:
        raise ContractError("provider tool is not the fixed registered executable identity")
    path = Path(binding["path"])
    try:
        directories = _provider_directories(path, deadline)
        alias_info = path.lstat()
        link = None
        target = path
        if provider == "lima-debian":
            if not stat.S_ISLNK(alias_info.st_mode):
                raise ContractError("Lima provider requires its ordinary Homebrew alias")
            link = os.readlink(str(path))
            if _LIMA_LINK.fullmatch(link) is None:
                raise ContractError("Lima provider alias has an unexpected Homebrew target")
            target = path.parent.parent / link[3:]
        elif not stat.S_ISREG(alias_info.st_mode):
            raise ContractError("Tart provider must be a regular non-symlink executable")
        directories += _provider_directories(target, deadline)
        target_info = target.lstat()
        if not stat.S_ISREG(target_info.st_mode) or not target_info.st_mode & 0o111:
            raise ContractError("provider target must be a regular non-symlink executable")
        if not 0 < target_info.st_size <= _PROVIDER_MAX_BYTES:
            raise ContractError("provider executable exceeds its bounded byte size")
        deadline.remaining()
        fd = os.open(str(target), os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as handle:
            if _provider_stat(os.fstat(handle.fileno())) != _provider_stat(target_info):
                raise ContractError("provider target changed before hashing")
            digest = hashlib.sha256()
            size = 0
            while True:
                deadline.remaining()
                chunk = handle.read(1024 * 1024)
                if not chunk:
                    break
                size += len(chunk)
                if size > _PROVIDER_MAX_BYTES:
                    raise ContractError("provider executable grew beyond its byte bound")
                digest.update(chunk)
            if _provider_stat(os.fstat(handle.fileno())) != _provider_stat(target_info):
                raise ContractError("provider target changed while hashing")
        deadline.remaining()
        if size != target_info.st_size or digest.hexdigest() != binding["sha256"]:
            raise ContractError("provider executable digest differs from registration")
        if _provider_stat(path.lstat()) != _provider_stat(alias_info) or _provider_stat(target.lstat()) != _provider_stat(target_info) or (link is not None and os.readlink(str(path)) != link):
            raise ContractError("provider alias or target changed during observation")
        if _provider_directories(path, deadline) + _provider_directories(target, deadline) != directories:
            raise ContractError("provider parent directory changed during observation")
        return ProviderToolIdentity(provider, binding["path"], str(target), link,
            digest.hexdigest(), _provider_stat(alias_info), _provider_stat(target_info), directories)
    except OSError as error:
        raise ContractError("provider executable identity cannot be observed: {}".format(type(error).__name__)) from error


def _verify_known_host(registration):
    ssh = registration["ssh"]
    path = _hash_regular(ssh["known_hosts"], "registration.ssh.known_hosts")
    lines = [line for line in path.read_text(encoding="utf-8").splitlines() if line and not line.startswith("#")]
    if len(lines) != 1:
        raise ContractError("known_hosts must contain exactly one pinned host key")
    fields = lines[0].split()
    if len(fields) < 3:
        raise ContractError("known_hosts line is incomplete")
    accepted_hosts = {ssh["host_alias"], "[{}]:{}".format(ssh["host_alias"], ssh["port"])}
    if fields[0] not in accepted_hosts:
        raise ContractError("known_hosts does not bind the registered alias and port")
    try:
        key_blob = base64.b64decode(fields[2], validate=True)
    except ValueError as error:
        raise ContractError("known_hosts public key is invalid base64") from error
    if hashlib.sha256(key_blob).hexdigest() != ssh["host_key_sha256"]:
        raise ContractError("known_hosts key does not match registered host-key digest")


def _validate_bundle(path):
    with tarfile.open(str(path), "r:*") as archive:
        members = archive.getmembers()
        names = {member.name for member in members}
        if names != GUEST_BUNDLE_MEMBERS or len(members) != len(names):
            raise ContractError("controller bundle members do not match the reviewed guest set")
        digests = {}
        for member in members:
            if not member.isfile() or member.size > MAX_JSON_BYTES * 4:
                raise ContractError("controller bundle contains a non-regular or oversized member")
            handle = archive.extractfile(member)
            if handle is None:
                raise ContractError("controller bundle member cannot be read")
            digests[member.name] = hashlib.sha256(handle.read()).hexdigest()
        return digests


class SSHTransport:
    """One pinned guest controller process plus an SSH TLS byte tunnel."""

    def __init__(self):
        self.registered = None
        self.paths = None
        self.process = None
        self.stderr_file = None
        self._closed = False
        self.public_certificate_path = None
        self.public_certificate_der_sha256 = None
        self.bundle_members = None
        self._reply_reader = None
        self.local_transport_evidence = None
        self.local_process_identity = None
        self._root_created = False
        self._setup_complete = False
        self._interrupted = False
        self._locally_forced = False
        self._writer_absent = False
        self._retention_complete = False
        self._root_disposed = False
        self._listener_clear = False
        self._mutation_unproved = False
        self.guest_failure = None

    def _argv(self, *options):
        base = build_ssh_argv(self.registered.registration)
        return base[:-1] + list(options) + [base[-1]]

    def writer_complete(self):
        original = self.process is None or (
            self.process.poll() is not None and 0 <= self.process.returncode < 255
            and not self._locally_forced
        )
        return original and not self._mutation_unproved

    def _run_remote(self, command, timeout, input_bytes=None, deadline=None, writer=False):
        command_deadline = Deadline(min(timeout, deadline.remaining())) if deadline is not None else Deadline(timeout)
        if writer:
            if not self.writer_complete():
                raise RuntimeError("latest remote writer completion is unproved")
            self._mutation_unproved = True
        # Status 255 and signals cannot establish remote completion. Errors that
        # lose output/completion deliberately leave the writer obligation set.
        result = run_capped(self._argv() + [command], command_deadline, input_bytes=input_bytes,
                            maximum=MAX_JSON_BYTES, allowed_status=tuple(range(255)))
        if writer:
            self._mutation_unproved = False
        if result.returncode != 0:
            raise RuntimeError("remote command failed with status {}".format(result.returncode))
        return result.stdout

    def _transfer_base64(self, local_bytes, remote_path, mode, deadline=None):
        decoder = "/usr/bin/base64 -D" if self.registered.registration["provider"] == "tart-macos" else "/usr/bin/base64 -d"
        command = "umask 077; {} > {}; /bin/chmod {} {}".format(
            decoder, shlex.quote(remote_path), mode, shlex.quote(remote_path)
        )
        self._run_remote(command, 120, base64.b64encode(local_bytes), deadline=deadline, writer=True)

    def _verify_remote_bundle_members(self, deadline=None):
        reg = self.registered.registration
        digest_tool = "/usr/bin/shasum -a 256" if reg["provider"] == "tart-macos" else "/usr/bin/sha256sum"
        paths = [self.paths["root"] + "/" + name for name in sorted(GUEST_BUNDLE_MEMBERS)]
        output = self._run_remote("{} {}".format(digest_tool, " ".join(shlex.quote(path) for path in paths)), 30, deadline=deadline)
        lines = output.decode("utf-8").splitlines()
        if len(lines) != len(paths):
            raise RuntimeError("guest controller member digest output is incomplete")
        observed = {}
        for name, line in zip(sorted(GUEST_BUNDLE_MEMBERS), lines):
            fields = line.split()
            if not fields:
                raise RuntimeError("guest controller member digest output is invalid")
            observed[name] = fields[0]
        if observed != self.bundle_members:
            raise RuntimeError("guest controller extracted members differ from reviewed archive")

    def open(self, registered, deadline=None):
        deadline = deadline or Deadline(30)
        if not isinstance(deadline, Deadline):
            raise TypeError("SSH transport open requires a bounded Deadline")
        deadline.remaining()
        self.registered = registered
        reg = registered.registration
        for key in ("executable", "config", "identity_file"):
            _hash_regular(reg["ssh"][key], "registration.ssh." + key)
        _verify_known_host(reg)
        archive = _hash_regular(registered.config["controller_bundle"], "session.controller_bundle")
        self.bundle_members = _validate_bundle(archive)
        self.paths = remote_session_paths(reg, registered.config["session_id"])
        stale = run_capped(
            ["/usr/sbin/lsof", "-nP", "-iTCP:18480", "-sTCP:LISTEN", "-FnPT"],
            Deadline(min(5, deadline.remaining())), maximum=MAX_JSON_BYTES, allowed_status=(0, 1),
        )
        if stale.returncode == 0 and stale.stdout:
            raise RuntimeError("local TLS forward port already has a listener")
        root = shlex.quote(self.paths["root"])
        root_mode = "0711" if reg["provider"] == "lima-debian" else "0700"
        self._run_remote("umask 077; test ! -e {0}; /bin/mkdir -m {1} {0}".format(root, root_mode), 30, deadline=deadline, writer=True)
        self._root_created = True
        try:
            self._transfer_base64(archive.read_bytes(), self.paths["archive"], "0600", deadline=deadline)
            digest_tool = "/usr/bin/shasum -a 256" if reg["provider"] == "tart-macos" else "/usr/bin/sha256sum"
            output = self._run_remote("{} {}; /usr/bin/tar -xf {} -C {}".format(digest_tool, shlex.quote(self.paths["archive"]), shlex.quote(self.paths["archive"]), root), 60, deadline=deadline, writer=True)
            observed_digest = output.decode("utf-8").split()[0] if output else ""
            if observed_digest != registered.config["controller_bundle"]["sha256"]:
                raise RuntimeError("guest controller archive digest mismatch")
            self._verify_remote_bundle_members()
            guest_inputs = {}
            seed_mode = "0555" if reg["provider"] == "lima-debian" else "0500"
            for key, mode in (("seed", seed_mode), ("scenario", "0600"), ("package_manifest", "0600")):
                local = Path(registered.config[key]["path"])
                remote_path = self.paths["root"] + "/" + key
                self._transfer_base64(local.read_bytes(), remote_path, mode, deadline=deadline)
                guest_inputs[key] = remote_path
            profile_sum = Path(registered.config["profile"]["path"])
            validate_profile_bundle(profile_sum)
            remote_profile = self.paths["root"] + "/profile"
            self._run_remote("/bin/mkdir -m 0700 {} {}/examples".format(shlex.quote(remote_profile), shlex.quote(remote_profile)), 15, deadline=deadline, writer=True)
            for name in sorted(PROFILE_MEMBERS):
                self._transfer_base64((profile_sum.parent / name).read_bytes(), remote_profile + "/" + name, "0600", deadline=deadline)
            guest_inputs["profile"] = remote_profile + "/SHA256SUMS"
            receipt_root = "/Library/Application Support/Teslatlas Hub/interop-package-receipts" if reg["provider"] == "tart-macos" else "/var/lib/teslatlas-hub/interop-package-receipts"
            private = {
                "schema_version": 1,
                "session": registered.config,
                "registration": reg,
                "registration_json_b64": base64.b64encode(Path(registered.config["host_registration"]["path"]).read_bytes()).decode("ascii"),
                "registration_sha256": registered.registration_sha256,
                "controller_bundle_sha256": registered.config["controller_bundle"]["sha256"],
                "controller_root": self.paths["root"],
                "guest_inputs": guest_inputs,
                "installation_receipt": receipt_root + "/" + registered.config["package"]["sha256"] + ".json",
                "ownership_path": self.paths["root"] + "/ownership.json",
            }
            self._transfer_base64(json.dumps(private, sort_keys=True, separators=(",", ":")).encode("utf-8"), self.paths["session"], "0600", deadline=deadline)
            python = shlex.quote(reg["guest"]["python"]["path"])
            entrypoint = shlex.quote(self.paths["entrypoint"])
            session = shlex.quote(self.paths["session"])
            remote = "exec {} -B {} --session {}".format(python, entrypoint, session)
            local_root = Path(registered.config["local_private_root"]) / registered.config["session_id"]
            self.stderr_file = (local_root / "guest-controller.stderr.log").open("xb")
            os.chmod(str(local_root / "guest-controller.stderr.log"), 0o600)
            session_argv = build_session_ssh_argv(reg) + [remote]
            self.process = subprocess.Popen(
                session_argv,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=self.stderr_file,
                bufsize=0,
            )
            from .macos import _process as observe_local_process
            self.local_process_identity = observe_local_process(self.process.pid, session_argv, deadline=deadline)
            self._reply_reader = IncrementalLineReader(self.process.stdout.fileno(), MAX_JSON_BYTES)
            greeting = self._read_reply(deadline)
            pem = self._run_remote("/bin/cat {}".format(shlex.quote(self.paths["root"] + "/public-ca.pem")), 15, deadline=deadline)
            try:
                der = ssl.PEM_cert_to_DER_cert(pem.decode("ascii"))
            except (UnicodeDecodeError, ValueError) as error:
                raise RuntimeError("guest public certificate is not a valid PEM certificate") from error
            certificate = local_root / "public-ca.pem"
            certificate.write_bytes(pem)
            os.chmod(str(certificate), 0o600)
            self.public_certificate_path = str(certificate)
            self.public_certificate_der_sha256 = hashlib.sha256(der).hexdigest()
            self._setup_complete = True
            return greeting
        except BaseException:
            try:
                self.close(preserve_recovery=True)
            except BaseException:
                pass
            raise

    def _read_reply(self, timeout):
        if self.process is None or self.process.stdout is None:
            raise RuntimeError("guest controller is not open")
        deadline = timeout if isinstance(timeout, Deadline) else Deadline(timeout)
        raw = self._reply_reader.read(deadline)
        if not raw or len(raw) > MAX_JSON_BYTES or not raw.endswith(b"\n"):
            raise RuntimeError("guest controller returned an invalid bounded frame")
        try:
            return json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("guest controller returned invalid UTF-8 JSON") from error

    def exchange(self, request, timeout):
        if self.process is None or self.process.stdin is None or self.process.poll() is not None:
            raise RuntimeError("guest controller process is unavailable")
        raw = json.dumps(request, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
        if len(raw) > MAX_JSON_BYTES:
            raise ContractError("guest request exceeds bounded frame")
        deadline = Deadline(timeout)
        write_all(self.process.stdin.fileno(), raw, deadline)
        return self._read_reply(deadline.remaining())

    def verify_local_tunnel(self, proof, timeout):
        """Independently observe the SSH PID, its listener, and TLS discovery."""
        deadline = timeout if isinstance(timeout, Deadline) else Deadline(timeout)
        if self.process is None or self.process.poll() is not None:
            raise RuntimeError("SSH tunnel process is unavailable")
        from .macos import _process as observe_local_process
        if observe_local_process(self.process.pid, deadline=deadline) != self.local_process_identity:
            raise RuntimeError("local SSH tunnel generation changed")
        result = run_capped(
            ["/usr/sbin/lsof", "-nP", "-a", "-p", str(self.process.pid), "-iTCP:18480", "-sTCP:LISTEN", "-FnPT"],
            Deadline(min(deadline.remaining(), 5)), maximum=MAX_JSON_BYTES,
        )
        lines = result.stdout.decode("utf-8").splitlines()
        if "p{}".format(self.process.pid) not in lines or not any(line == "n127.0.0.1:18480" for line in lines):
            raise RuntimeError("local TLS listener is not owned by the SSH tunnel process")
        tls = normal_tls_probe(
            self.public_certificate_path, self.registered.config["expected"]["product_version"],
            deadline=deadline, profile_path=self.registered.config["profile"]["path"],
        )
        if tls["certificate_der_sha256"] != proof["tls"]["certificate_der_sha256"] or tls["hub_id"] != proof["discovery"]["hub_id"] or tls["response_sha256"] != proof["discovery"]["response_sha256"] or tls["validated_response_set_sha256"] != proof["discovery"]["validated_response_set_sha256"]:
            raise RuntimeError("local tunnel TLS/discovery differs from guest observation")
        self.local_transport_evidence = {
            "ssh_process": self.local_process_identity, "listener": "127.0.0.1:18480",
            "certificate_der_sha256": tls["certificate_der_sha256"],
            "discovery_response_sha256": tls["response_sha256"],
            "validated_response_set_sha256": tls["validated_response_set_sha256"],
        }
        return dict(self.local_transport_evidence)

    def verify_stopped(self, registered, timeout):
        if not self.writer_complete():
            raise RuntimeError("latest remote writer completion is unproved")
        reg = registered.registration
        deadline = Deadline(timeout)
        budget_ms = max(1, min(45_000, int(deadline.remaining() * 1000)))
        remote = "{} -B {} --verify-stopped {} --budget-ms {}".format(
            shlex.quote(reg["guest"]["python"]["path"]),
            shlex.quote(self.paths["entrypoint"]),
            shlex.quote(self.paths["session"]),
            budget_ms,
        )
        raw = self._run_remote(remote, deadline.remaining(), deadline=deadline)
        if len(raw) > MAX_JSON_BYTES:
            raise RuntimeError("stopped observation exceeds bounded output")
        try:
            return json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise RuntimeError("stopped observer returned invalid JSON") from error

    def recover_stop(self, registered, timeout):
        if not self.writer_complete():
            raise RuntimeError("latest remote writer completion is unproved")
        reg = registered.registration
        deadline = Deadline(timeout)
        budget_ms = max(1, min(45_000, int(deadline.remaining() * 1000)))
        remote = "{} -B {} --recover-stop {} --budget-ms {}".format(shlex.quote(reg["guest"]["python"]["path"]), shlex.quote(self.paths["entrypoint"]), shlex.quote(self.paths["session"]), budget_ms)
        raw = self._run_remote(remote, deadline.remaining(), deadline=deadline, writer=True)
        return json.loads(raw.decode("utf-8"))

    def interrupt(self, timeout=3):
        if self.process is None or self.process.poll() is not None:
            return
        self._interrupted = True
        self._locally_forced = True
        if self.process.stdin is not None and not self.process.stdin.closed:
            self.process.stdin.close()
        self.process.terminate()
        try:
            self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=timeout)

    def finish_writer(self, timeout):
        """Close guest input and prove the original remote command has ended."""
        if self.process is None:
            return False
        if self.process.stdin is not None and not self.process.stdin.closed:
            try:
                self.process.stdin.close()
            except OSError:
                pass
        if self.process.poll() is None:
            try:
                self.process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                return False
        self._writer_absent = self.writer_complete()
        return self._writer_absent

    def _reap_process(self, timeout):
        errors = []
        forced = False
        process = self.process
        if process is None:
            return errors
        if process.stdin is not None and not process.stdin.closed:
            try:
                process.stdin.close()
            except OSError as error:
                errors.append(error)
        if process.poll() is None:
            first = max(0.01, timeout * 0.4)
            try:
                process.wait(timeout=first)
            except subprocess.TimeoutExpired:
                forced = True
                self._locally_forced = True
                process.terminate()
                try:
                    process.wait(timeout=max(0.01, timeout * 0.25))
                except subprocess.TimeoutExpired:
                    process.kill()
                    try:
                        process.wait(timeout=max(0.01, timeout * 0.25))
                    except subprocess.TimeoutExpired as error:
                        errors.append(error)
        if process.poll() is not None:
            self._writer_absent = self.writer_complete()
        for handle in (process.stdin, process.stdout, process.stderr):
            if handle is not None and not handle.closed:
                try:
                    handle.close()
                except OSError as error:
                    errors.append(error)
        if forced:
            errors.append(RuntimeError("SSH guest controller required forced termination"))
        return errors

    def close(self, timeout=45, preserve_recovery=False):
        if self._closed:
            return
        deadline = Deadline(timeout)
        cleanup_errors = []
        cleanup_errors.extend(cleanup_error("process-reap", e) for e in self._reap_process(deadline.remaining(timeout * 0.35)))
        if self.stderr_file is not None and not self.stderr_file.closed:
            try:
                self.stderr_file.close()
            except BaseException as error:
                cleanup_errors.append(cleanup_error("stderr-close", error))
        retention_ok = not self._setup_complete or self._retention_complete
        if self.registered is not None and self.paths is not None:
            if self._setup_complete and not self._retention_complete:
                try:
                    deadline.remaining()
                    reg = self.registered.registration
                    digest_tool = "/usr/bin/shasum -a 256" if reg["provider"] == "tart-macos" else "/usr/bin/sha256sum"
                    observed = self._run_remote("{} {}".format(digest_tool, shlex.quote(self.paths["archive"])), 15, deadline=deadline).decode("utf-8").split()[0]
                    if observed != self.registered.config["controller_bundle"]["sha256"]:
                        raise RuntimeError("guest controller bundle changed during session")
                    self._verify_remote_bundle_members(deadline)
                    guest_journal = self._run_remote("/bin/cat {}".format(shlex.quote(self.paths["root"] + "/guest.journal.jsonl")), 15, deadline=deadline)
                    entries = [json.loads(line) for line in guest_journal.decode("utf-8").splitlines()]
                    if not entries or entries[0].get("operation") != "preflight" or entries[-1].get("kind") not in ("closed", "cleanup-failure"):
                        raise RuntimeError("guest journal lacks preflight or terminal cleanup record")
                    bytecode = self._run_remote("/usr/bin/find {} \\( -name __pycache__ -o -name '*.pyc' \\) -print".format(shlex.quote(self.paths["root"])), 10, deadline=deadline)
                    if bytecode:
                        raise RuntimeError("guest controller created Python bytecode")
                    local = Path(self.registered.config["local_private_root"]) / self.registered.config["session_id"] / "guest-controller.journal.jsonl"
                    durable_bytes(local, guest_journal)
                    ownership = self._run_remote("/bin/cat {}".format(shlex.quote(self.paths["root"] + "/ownership.json")), 5, deadline=deadline)
                    retained = local.parent / "guest-controller.ownership.json"
                    durable_bytes(retained, ownership)
                    retention_ok = True
                    self._retention_complete = not preserve_recovery
                except BaseException as error:
                    cleanup_errors.append(cleanup_error("evidence-retention", error))
            if self._root_created and not self._root_disposed and not preserve_recovery and retention_ok and self.writer_complete():
                try:
                    deadline.remaining()
                    files = [self.paths["archive"], self.paths["session"]]
                    files.extend(self.paths["root"] + "/" + member for member in sorted(GUEST_BUNDLE_MEMBERS))
                    files.extend(self.paths["root"] + "/" + name for name in ("seed", "scenario", "package_manifest", "config.toml", "public-ca.pem", "guest.journal.jsonl", "ownership.json", "ownership.tmp", "ownership.json.tmp"))
                    files.extend(self.paths["root"] + "/profile/" + name for name in sorted(PROFILE_MEMBERS))
                    members = " ".join(shlex.quote(path) for path in files)
                    command = "test ! -L {root}; if test -d {root}; then /bin/rm -f -- {members}; /usr/bin/find {root} -depth -type d -empty -delete; fi; test ! -e {root}".format(
                        members=members, root=shlex.quote(self.paths["root"])
                    )
                    self._run_remote(command, 30, deadline=deadline, writer=True)
                    self._root_disposed = True
                except BaseException as error:
                    cleanup_errors.append(cleanup_error("remote-disposal", error))
        if not self._listener_clear:
            try:
                stale = run_capped(
                    ["/usr/sbin/lsof", "-nP", "-iTCP:18480", "-sTCP:LISTEN", "-FnPT"],
                    Deadline(deadline.remaining(timeout * 0.15)), maximum=MAX_JSON_BYTES, allowed_status=(0, 1),
                )
                if stale.returncode == 0 and stale.stdout:
                    raise RuntimeError("local SSH TLS listener survived transport cleanup")
                self._listener_clear = True
            except BaseException as error:
                cleanup_errors.append(cleanup_error("local-listener", error))
        if cleanup_errors:
            error = RuntimeError("transport cleanup failed: {}".format(",".join(item["resource"] + ":" + item["error"] for item in cleanup_errors)))
            error.cleanup_errors = cleanup_errors
            raise error
        if self.process is not None and self.process.returncode != 0:
            self.guest_failure = {"resource": "guest-exit", "error": "GuestExitStatus", "status": self.process.returncode}
        if not self.writer_complete():
            raise RuntimeError("latest remote writer completion is unproved")
        disposed = not self._root_created or self._root_disposed
        if not preserve_recovery and retention_ok and disposed and self._listener_clear:
            self._closed = True
