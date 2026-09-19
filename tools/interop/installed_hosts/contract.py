# SPDX-License-Identifier: AGPL-3.0-only
"""Strict, side-effect-free validation for the installed-host wire contract."""
import hashlib
import json
import re
import stat
import uuid
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from urllib.parse import parse_qsl, urlparse


MAX_JSON_BYTES = 262_144
MAX_EVENTS = 512
MAX_STRING = 4096
CONTROL_OPERATIONS = frozenset(("verify", "stop", "start", "pair", "revoke"))
PROVIDERS = frozenset(("tart-macos", "lima-debian"))
PREDECESSOR_HOSTS = frozenset(
    (
        "teslatlas-interop-macos13-20260905",
        "teslatlas-interop-debian13-arm64",
        "teslatlas-interop-debian13-amd64",
    )
)
PROTECTED_PATHS = (
    "/Users/admin/interop-upgrade-retained-20260905",
    "/home/bolyki.guest/interop-upgrade-retained-release",
    "/var/lib/teslatlas-hub/interop-upgrade-retained-v2026.36.1",
)
_HEX = re.compile(r"^[0-9a-f]{64}$")
_TOKEN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
_VERSION = re.compile(r"^[0-9]+(?:\.[0-9]+){1,3}(?:[-+][A-Za-z0-9._-]+)?$")


class ContractError(ValueError):
    """A private input failed the closed contract."""


@dataclass(frozen=True)
class RegisteredConfig:
    config: dict
    registration: dict
    registration_sha256: str


def _fail(path, message):
    raise ContractError("{}: {}".format(path, message))


def _object(value, path):
    if not isinstance(value, dict):
        _fail(path, "must be an object")
    return value


def _exact(value, required, path, optional=()):
    obj = _object(value, path)
    required = set(required)
    allowed = required | set(optional)
    missing = sorted(required - set(obj))
    extra = sorted(set(obj) - allowed)
    if missing:
        _fail(path, "missing fields: {}".format(", ".join(missing)))
    if extra:
        _fail(path, "unexpected fields: {}".format(", ".join(extra)))
    return obj


def _string(value, path, minimum=1, maximum=MAX_STRING):
    if not isinstance(value, str) or not minimum <= len(value) <= maximum:
        _fail(path, "must be a bounded string")
    if "\x00" in value or "\n" in value or "\r" in value:
        _fail(path, "contains forbidden control characters")
    return value


def _integer(value, path, minimum=0, maximum=2**63 - 1):
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        _fail(path, "must be an integer in range")
    return value


def _integer_literal(value, expected, path):
    _integer(value, path, minimum=expected, maximum=expected)
    return value


def _literal(value, allowed, path):
    if not isinstance(value, str) or value not in allowed:
        _fail(path, "must be one of {}".format(", ".join(sorted(allowed))))
    return value


def _digest(value, path):
    if not isinstance(value, str) or _HEX.fullmatch(value) is None:
        _fail(path, "must be a lowercase SHA-256 digest")
    return value


def _uuid(value, path):
    value = _string(value, path, maximum=36)
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError):
        _fail(path, "must be a canonical UUID")
    if str(parsed) != value or parsed.version not in (4,):
        _fail(path, "must be a canonical version-4 UUID")
    return value


def _token(value, path):
    if not isinstance(value, str) or _TOKEN.fullmatch(value) is None:
        _fail(path, "must be a constrained token")
    return value


def _canonical_path(value, path):
    value = _string(value, path, maximum=1024)
    pure = PurePosixPath(value)
    if not pure.is_absolute() or str(pure) != value or ".." in pure.parts:
        _fail(path, "must be a canonical absolute path")
    return value


def _not_protected(value, path):
    candidate = PurePosixPath(_canonical_path(value, path))
    for protected in PROTECTED_PATHS:
        protected_path = PurePosixPath(protected)
        if candidate == protected_path or protected_path in candidate.parents:
            _fail(path, "protected predecessor path")
    return str(candidate)


def validate_file_binding(value, path="binding"):
    obj = _exact(value, ("path", "sha256"), path)
    _canonical_path(obj["path"], path + ".path")
    _digest(obj["sha256"], path + ".sha256")
    return dict(obj)


def validate_descriptor(value):
    obj = _exact(
        value,
        ("schema_version", "kind", "broker_socket", "session_id", "registration_sha256"),
        "descriptor",
    )
    _integer_literal(obj["schema_version"], 1, "descriptor.schema_version")
    if obj["kind"] != "installed-host":
        _fail("descriptor.kind", "must equal installed-host")
    _canonical_path(obj["broker_socket"], "descriptor.broker_socket")
    _uuid(obj["session_id"], "descriptor.session_id")
    _digest(obj["registration_sha256"], "descriptor.registration_sha256")
    return dict(obj)


def validate_package_manifest(value):
    obj = _exact(
        value,
        (
            "schema_version", "kind", "package_sha256", "product_version", "os",
            "architecture", "package_manager_version", "store_schema_version", "hub_executable", "platform_payload", "payload_manifest_sha256",
            "payload_members", "package_scripts",
            "source_export_sha256", "source_manifest_sha256", "build_manifest_sha256",
        ),
        "package_manifest",
    )
    _integer_literal(obj["schema_version"], 1, "package_manifest.schema_version")
    if obj["kind"] != "installed-host-package":
        _fail("package_manifest", "must be installed-host-package schema 1")
    _digest(obj["package_sha256"], "package_manifest.package_sha256")
    _string(obj["product_version"], "package_manifest.product_version", maximum=128)
    _string(obj["package_manager_version"], "package_manifest.package_manager_version", maximum=128)
    _integer(obj["store_schema_version"], "package_manifest.store_schema_version", minimum=1, maximum=2**31 - 1)
    _literal(obj["os"], ("macOS", "Debian 13"), "package_manifest.os")
    _literal(obj["architecture"], ("arm64", "amd64"), "package_manifest.architecture")
    validate_file_binding(obj["hub_executable"], "package_manifest.hub_executable")
    members = obj["payload_members"]
    if not isinstance(members, list) or not 1 <= len(members) <= 512:
        _fail("package_manifest.payload_members", "must be a bounded non-empty array")
    checked_members = [validate_file_binding(item, "package_manifest.payload_members[]") for item in members]
    paths = [item["path"] for item in checked_members]
    if len(paths) != len(set(paths)) or obj["hub_executable"] not in checked_members:
        _fail("package_manifest.payload_members", "must uniquely include the installed Hub executable")
    for key in ("payload_manifest_sha256", "source_export_sha256", "source_manifest_sha256", "build_manifest_sha256"):
        _digest(obj[key], "package_manifest." + key)
    if obj["payload_manifest_sha256"] != payload_manifest_digest(checked_members):
        _fail("package_manifest.payload_manifest_sha256", "does not match exact payload members")
    platform = obj["platform_payload"]
    if obj["os"] == "Debian 13":
        platform = _exact(platform, ("hub_executable", "systemd_unit"), "package_manifest.platform_payload")
        if obj["package_scripts"] != {}:
            _fail("package_manifest.package_scripts", "Debian package script set must be empty")
        expected_paths = {
            "hub_executable": "/usr/bin/teslatlas-hub",
            "systemd_unit": "/usr/lib/systemd/system/teslatlas-hub.service",
        }
    else:
        platform = _exact(platform, ("hub_executable", "wrapper_script", "app_executable", "app_info_plist"), "package_manifest.platform_payload")
        scripts = _exact(obj["package_scripts"], ("launchagent_template",), "package_manifest.package_scripts")
        template = _exact(scripts["launchagent_template"], ("archive_path", "sha256"), "package_manifest.package_scripts.launchagent_template")
        if template["archive_path"] != "Scripts/com.teslatlas.hub.plist.in":
            _fail("package_manifest.package_scripts.launchagent_template.archive_path", "must bind the actual package script member")
        _digest(template["sha256"], "package_manifest.package_scripts.launchagent_template.sha256")
        expected_paths = {
            "hub_executable": "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub",
            "wrapper_script": "/Library/Application Support/Teslatlas Hub/libexec/run-hub-service.sh",
            "app_executable": "/Applications/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub",
            "app_info_plist": "/Applications/Teslatlas Hub.app/Contents/Info.plist",
        }
    for key, expected_path in expected_paths.items():
        binding = validate_file_binding(platform[key], "package_manifest.platform_payload." + key)
        if binding["path"] != expected_path or binding not in checked_members:
            _fail("package_manifest.platform_payload." + key, "must bind the fixed installed payload member")
    return dict(obj)


def payload_manifest_digest(members):
    normalized = sorted(({"path": item["path"], "sha256": item["sha256"]} for item in members), key=lambda item: item["path"])
    return hashlib.sha256(json.dumps(normalized, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()


def validate_control_request(value):
    obj = _object(value, "request")
    op = _literal(obj.get("op"), CONTROL_OPERATIONS, "request.op")
    required = ("schema_version", "session_id", "sequence", "challenge", "op")
    if op == "revoke":
        required += ("device_id",)
    obj = _exact(obj, required, "request")
    _integer_literal(obj["schema_version"], 1, "request.schema_version")
    _uuid(obj["session_id"], "request.session_id")
    _integer(obj["sequence"], "request.sequence", minimum=1, maximum=1_000_000)
    _digest(obj["challenge"], "request.challenge")
    if op == "revoke":
        _uuid(obj["device_id"], "request.device_id")
    return dict(obj)


def validate_invitation(value, path="invitation"):
    obj = _exact(
        value,
        ("pairingId", "secret", "expiresAtMs", "endpoint", "tlsPin", "pairingUri"),
        path,
    )
    _uuid(obj["pairingId"], path + ".pairingId")
    _string(obj["secret"], path + ".secret", maximum=512)
    _integer(obj["expiresAtMs"], path + ".expiresAtMs", maximum=2**63 - 1)
    if obj["endpoint"] != "https://127.0.0.1:18480":
        _fail(path + ".endpoint", "must use the fixed installed-host TLS endpoint")
    _digest(obj["tlsPin"], path + ".tlsPin")
    pairing_uri = _string(obj["pairingUri"], path + ".pairingUri", maximum=4096)
    parsed = urlparse(pairing_uri)
    query = parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True)
    expected = [
        ("endpoint", obj["endpoint"]),
        ("pairing_id", obj["pairingId"]),
        ("secret", obj["secret"]),
        ("tls_pin", obj["tlsPin"]),
    ]
    if (parsed.scheme, parsed.netloc, parsed.path, parsed.params, parsed.fragment) != ("teslatlas-hub", "pair", "", "", "") or query != expected:
        _fail(path + ".pairingUri", "does not exactly bind the invitation fields")
    return dict(obj)


def validate_registration(value):
    obj = _exact(
        value,
        ("schema_version", "install_state", "host_id", "provider", "provider_id", "provider_tool", "ssh", "guest", "lease", "protected_paths"),
        "registration",
    )
    _integer_literal(obj["schema_version"], 1, "registration.schema_version")
    install_state = _literal(obj["install_state"], ("preinstall", "installed"), "registration.install_state")
    host_id = _token(obj["host_id"], "registration.host_id")
    provider_id = _token(obj["provider_id"], "registration.provider_id")
    if host_id in PREDECESSOR_HOSTS or provider_id in PREDECESSOR_HOSTS:
        _fail("registration.host_id", "retained predecessor guest is not eligible")
    provider = _literal(obj["provider"], PROVIDERS, "registration.provider")
    provider_tool = validate_file_binding(obj["provider_tool"], "registration.provider_tool")
    fixed_provider_tool = {
        "tart-macos": "/Users/bolyki/.codex/artifacts/teslatlas-interop/2026-09-05-execution/tooling/tart-2.36.0/tart.app/Contents/MacOS/tart",
        "lima-debian": "/opt/homebrew/bin/limactl",
    }[provider]
    if provider_tool["path"] != fixed_provider_tool:
        _fail("registration.provider_tool.path", "is not the reviewed provider executable")
    ssh = _exact(
        obj["ssh"],
        ("executable", "config", "identity_file", "known_hosts", "host_alias", "hostname", "host_key_sha256", "port", "user", "login_uid"),
        "registration.ssh",
    )
    for key in ("executable", "config", "identity_file", "known_hosts"):
        validate_file_binding(ssh[key], "registration.ssh." + key)
    if ssh["executable"]["path"] != "/usr/bin/ssh":
        _fail("registration.ssh.executable.path", "must use the reviewed system OpenSSH client")
    _token(ssh["host_alias"], "registration.ssh.host_alias")
    _token(ssh["hostname"], "registration.ssh.hostname")
    _digest(ssh["host_key_sha256"], "registration.ssh.host_key_sha256")
    _integer(ssh["port"], "registration.ssh.port", minimum=1, maximum=65535)
    _token(ssh["user"], "registration.ssh.user")
    _integer(ssh["login_uid"], "registration.ssh.login_uid", maximum=2**31 - 1)
    guest = _exact(
        obj["guest"],
        (
            "machine_identity_sha256", "hardware_identity_sha256", "hypervisor_backend",
            "hypervisor_evidence_sha256", "image_identity_sha256", "os", "architecture",
            "native_or_emulated", "python", "python_version", "python_toml_module",
            "python_process",
            "permitted_service_user", "permitted_service_uid",
        ),
        "registration.guest",
    )
    for key in ("machine_identity_sha256", "hardware_identity_sha256", "hypervisor_evidence_sha256", "image_identity_sha256"):
        _digest(guest[key], "registration.guest." + key)
    _string(guest["hypervisor_backend"], "registration.guest.hypervisor_backend", maximum=128)
    os_name = _literal(guest["os"], ("macOS", "Debian 13"), "registration.guest.os")
    architecture = _literal(guest["architecture"], ("arm64", "amd64"), "registration.guest.architecture")
    _literal(guest["native_or_emulated"], ("native", "emulated"), "registration.guest.native_or_emulated")
    validate_file_binding(guest["python"], "registration.guest.python")
    validate_file_binding(guest["python_process"], "registration.guest.python_process")
    version = _string(guest["python_version"], "registration.guest.python_version", maximum=32)
    if _VERSION.fullmatch(version) is None or tuple(int(x) for x in version.split("-")[0].split("+")[0].split(".")[:2]) < (3, 9):
        _fail("registration.guest.python_version", "Python 3.9 or later is required")
    _literal(guest["python_toml_module"], ("tomllib", "tomli"), "registration.guest.python_toml_module")
    _token(guest["permitted_service_user"], "registration.guest.permitted_service_user")
    if guest["permitted_service_uid"] is not None:
        _integer(guest["permitted_service_uid"], "registration.guest.permitted_service_uid", maximum=2**31 - 1)
    if os_name == "macOS" and (provider != "tart-macos" or architecture != "arm64"):
        _fail("registration.provider", "provider/platform/service binding mismatch")
    if os_name == "Debian 13" and provider != "lima-debian":
        _fail("registration.provider", "provider/platform/service binding mismatch")
    if provider == "lima-debian":
        if ssh["host_alias"] != "lima-" + provider_id:
            _fail("registration.ssh.host_alias", "must be the registered Lima alias")
        if guest["permitted_service_user"] != "teslatlas" or (install_state == "installed" and guest["permitted_service_uid"] is None) or guest["permitted_service_uid"] == ssh["login_uid"]:
            _fail("registration.guest.permitted_service_user", "must bind the distinct packaged teslatlas account")
        if install_state == "preinstall" and guest["permitted_service_uid"] is not None:
            _fail("registration.guest.permitted_service_uid", "must be null until the package-created account is observed")
    elif guest["permitted_service_user"] != ssh["user"] or guest["permitted_service_uid"] != ssh["login_uid"]:
        _fail("registration.guest.permitted_service_user", "must bind the console LaunchAgent account")
    lease = _exact(obj["lease"], ("lease_id", "resource_id", "expires_at_unix"), "registration.lease")
    _token(lease["lease_id"], "registration.lease.lease_id")
    if _token(lease["resource_id"], "registration.lease.resource_id") != provider_id:
        _fail("registration.lease.resource_id", "must bind provider_id")
    _integer(lease["expires_at_unix"], "registration.lease.expires_at_unix", minimum=1)
    protected = obj["protected_paths"]
    if not isinstance(protected, list) or len(protected) != len(PROTECTED_PATHS):
        _fail("registration.protected_paths", "must contain the fixed refusal paths")
    validated = [_canonical_path(item, "registration.protected_paths[]") for item in protected]
    if set(validated) != set(PROTECTED_PATHS):
        _fail("registration.protected_paths", "must contain the fixed refusal paths")
    return dict(obj)


def validate_session_config(value):
    obj = _exact(
        value,
        (
            "schema_version", "kind", "run_id", "cell_id", "adapter_id", "client_id", "session_id", "host_id",
            "host_registration", "controller_bundle", "package", "package_manifest", "expected",
            "seed", "profile", "scenario", "local_private_root", "guest_run_id",
            "lifetime_seconds", "allowed_origins",
        ),
        "session",
    )
    _integer_literal(obj["schema_version"], 1, "session.schema_version")
    if obj["kind"] != "installed-host":
        _fail("session.kind", "must equal installed-host")
    _token(obj["run_id"], "session.run_id")
    cell_id = _token(obj["cell_id"], "session.cell_id")
    adapters = ("protocol_actual_hub", "typescript_node", "typescript_browser", "swift", "home_assistant", "edge_v2")
    adapter_id = _literal(obj["adapter_id"], adapters, "session.adapter_id")
    client_id = _literal(obj["client_id"], adapters, "session.client_id")
    if adapter_id != client_id:
        _fail("session.adapter_id", "must equal the authoritative matrix client identity")
    _uuid(obj["session_id"], "session.session_id")
    host_id = _token(obj["host_id"], "session.host_id")
    if host_id in PREDECESSOR_HOSTS:
        _fail("session.host_id", "retained predecessor guest is not eligible")
    for key in ("host_registration", "controller_bundle", "package", "package_manifest", "seed", "profile", "scenario"):
        validate_file_binding(obj[key], "session." + key)
    expected = _exact(
        obj["expected"],
        ("os", "architecture", "native_or_emulated", "service_mode", "product_version", "hub_executable_sha256"),
        "session.expected",
    )
    os_name = _literal(expected["os"], ("macOS", "Debian 13"), "expected.os")
    architecture = _literal(expected["architecture"], ("arm64", "amd64"), "expected.architecture")
    _literal(expected["native_or_emulated"], ("native", "emulated"), "expected.native_or_emulated")
    mode = _literal(expected["service_mode"], ("installed-app-launchagent", "installed-deb-systemd"), "expected.service_mode")
    _string(expected["product_version"], "expected.product_version", maximum=128)
    _digest(expected["hub_executable_sha256"], "expected.hub_executable_sha256")
    if (os_name, mode) not in (("macOS", "installed-app-launchagent"), ("Debian 13", "installed-deb-systemd")):
        _fail("expected.service_mode", "provider/platform/service binding mismatch")
    if os_name == "macOS" and architecture != "arm64":
        _fail("expected.architecture", "provider/platform/service binding mismatch")
    target = "macos_arm64" if os_name == "macOS" else "debian13_" + architecture
    if cell_id != adapter_id + "__" + target:
        _fail("session.cell_id", "does not exactly match adapter/client/platform target")
    _not_protected(obj["local_private_root"], "session.local_private_root")
    guest_run = _string(obj["guest_run_id"], "session.guest_run_id", maximum=260)
    guest_path = PurePosixPath(guest_run)
    if guest_path.is_absolute() or ".." in guest_path.parts or len(guest_path.parts) != 2 or any(_TOKEN.fullmatch(p) is None for p in guest_path.parts):
        _fail("session.guest_run_id", "must be a two-component constrained relative path")
    _integer(obj["lifetime_seconds"], "session.lifetime_seconds", minimum=1, maximum=3600)
    origins = obj["allowed_origins"]
    if not isinstance(origins, list) or not 1 <= len(origins) <= 16 or len(set(map(str, origins))) != len(origins):
        _fail("session.allowed_origins", "must be a unique bounded array")
    for index, origin in enumerate(origins):
        value = _string(origin, "session.allowed_origins[{}]".format(index), maximum=256)
        match = re.fullmatch(r"http://(127\.0\.0\.1|localhost):([1-9][0-9]{0,4})", value)
        if match is None or int(match.group(2)) > 65535:
            _fail("session.allowed_origins[{}]".format(index), "must be a canonical loopback HTTP origin")
    return dict(obj)


def validate_edge_session_config(value, *, adapter_id, client_id, cell_id):
    """Validate the additive private session-v2 envelope used only by Edge.

    This is deliberately separate from :func:`validate_session_config`: the
    installed-host v1 object remains exact and no other adapter can select the
    Edge fixture acquisition path.
    """
    if adapter_id != "edge_v2" or client_id != "edge_v2" or not isinstance(cell_id, str) or not re.fullmatch(
        r"edge_v2__(macos_arm64|debian13_amd64|debian13_arm64)", cell_id
    ):
        _fail("session.schema_version", "session v2 is Edge-only")
    obj = _exact(
        value,
        ("schema_version", "run_id", "edge_fixture", "scenario"),
        "edge_session",
    )
    _integer_literal(obj["schema_version"], 2, "edge_session.schema_version")
    _token(obj["run_id"], "edge_session.run_id")
    validate_file_binding(obj["edge_fixture"], "edge_session.edge_fixture")
    validate_file_binding(obj["scenario"], "edge_session.scenario")
    return dict(obj)


def _read_bounded_regular_json(path, label):
    target = Path(path)
    info = target.lstat()
    if not stat.S_ISREG(info.st_mode):
        _fail(label, "must be a regular file, not a symlink or special file")
    if info.st_size > MAX_JSON_BYTES:
        _fail(label, "exceeds bounded JSON size")
    raw = target.read_bytes()
    if len(raw) != info.st_size:
        _fail(label, "changed while reading")

    def unique_object(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                _fail(label, "duplicate JSON key")
            value[key] = item
        return value

    try:
        return raw, json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ContractError("{}: invalid UTF-8 JSON".format(label)) from error


def read_edge_session_config(binding, *, adapter_id, client_id, cell_id):
    """Read and validate one owner-only, hash-bound Edge session-v2 file."""
    checked = validate_file_binding(binding, "edge_session_binding")
    target = Path(checked["path"])
    info = target.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        _fail("edge_session_binding.path", "must be a single-link regular file")
    if stat.S_IMODE(info.st_mode) & 0o077:
        _fail("edge_session_binding.path", "must be owner-only")
    raw, value = _read_bounded_regular_json(target, "edge_session")
    if hashlib.sha256(raw).hexdigest() != checked["sha256"]:
        _fail("edge_session_binding.sha256", "digest changed")
    return validate_edge_session_config(
        value, adapter_id=adapter_id, client_id=client_id, cell_id=cell_id,
    )


def read_registered_config(value, registration_inventory, required_install_state="installed"):
    cfg = validate_session_config(value)
    if not isinstance(registration_inventory, dict):
        _fail("registration_inventory", "must be an independently verified mapping")
    expected_digest = registration_inventory.get(cfg["host_id"])
    if expected_digest is None:
        _fail("session.host_id", "not present in verified registration inventory")
    _digest(expected_digest, "registration_inventory.digest")
    if expected_digest != cfg["host_registration"]["sha256"]:
        _fail("session.host_registration.sha256", "does not match verified registration inventory")
    raw, registration_value = _read_bounded_regular_json(cfg["host_registration"]["path"], "host_registration")
    actual_digest = hashlib.sha256(raw).hexdigest()
    if actual_digest != cfg["host_registration"]["sha256"]:
        _fail("host_registration.sha256", "does not match exact registration bytes")
    reg = validate_registration(registration_value)
    if required_install_state is not None and reg["install_state"] != required_install_state:
        _fail("registration.install_state", "does not match the requested controller phase")
    if reg["host_id"] != cfg["host_id"]:
        _fail("host_registration.host_id", "does not match selected host")
    expected = cfg["expected"]
    guest = reg["guest"]
    if (expected["os"], expected["architecture"], expected["native_or_emulated"]) != (
        guest["os"], guest["architecture"], guest["native_or_emulated"]
    ):
        _fail("session.expected", "does not match registered guest identity")
    if (reg["provider"], expected["service_mode"]) not in (
        ("tart-macos", "installed-app-launchagent"),
        ("lima-debian", "installed-deb-systemd"),
    ):
        _fail("session.expected", "provider/platform/service binding mismatch")
    return RegisteredConfig(cfg, reg, actual_digest)


_SECRET_KEYS = frozenset(
    ("invitation", "expired_invitation", "secret", "bearer", "tls_private_key", "raw_config", "argv", "raw_argv", "credentials")
)


def public_observation(value):
    """Return a deep public copy with all secret-bearing keys removed."""
    if isinstance(value, dict):
        return {key: public_observation(item) for key, item in value.items() if key not in _SECRET_KEYS}
    if isinstance(value, list):
        return [public_observation(item) for item in value]
    return value


def _boolean(value, path):
    if not isinstance(value, bool):
        _fail(path, "must be a boolean")
    return value


def validate_process_identity(value, path="process"):
    obj = _exact(
        value,
        ("pid", "uid", "parent_pid", "boot_id", "start_identity", "executable_path", "executable_sha256", "argv_sha256"),
        path,
    )
    _integer(obj["pid"], path + ".pid", minimum=1, maximum=2**31 - 1)
    _integer(obj["uid"], path + ".uid", maximum=2**31 - 1)
    _integer(obj["parent_pid"], path + ".parent_pid", maximum=2**31 - 1)
    _string(obj["boot_id"], path + ".boot_id", maximum=256)
    _string(obj["start_identity"], path + ".start_identity", maximum=256)
    _canonical_path(obj["executable_path"], path + ".executable_path")
    _digest(obj["executable_sha256"], path + ".executable_sha256")
    _digest(obj["argv_sha256"], path + ".argv_sha256")
    return dict(obj)


def _generation_tuple(observation):
    hub = observation["service"]["hub"]
    return hub["boot_id"], hub["start_identity"]


def generation_changed(before, after):
    """Compare kernel generation identity; PID differences alone do not count."""
    return _generation_tuple(before) != _generation_tuple(after)


def _process_tree_digest(*identities):
    raw = json.dumps(identities, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(raw).hexdigest()


def validate_installed_observation(value, registered):
    cfg = registered.config
    reg = registered.registration
    obj = _exact(
        value,
        ("schema_version", "status", "session_id", "sequence", "challenge", "observer", "host", "package", "service", "config", "listener", "tls", "discovery"),
        "proof",
    )
    _integer_literal(obj["schema_version"], 1, "proof.schema_version")
    if obj["status"] != "verified":
        _fail("proof", "must be schema 1 verified observation")
    if obj["session_id"] != cfg["session_id"]:
        _fail("proof.session_id", "does not match owned session")
    _integer(obj["sequence"], "proof.sequence", minimum=1, maximum=1_000_000)
    _digest(obj["challenge"], "proof.challenge")
    observer = _exact(obj["observer"], ("bundle_sha256", "process"), "proof.observer")
    if _digest(observer["bundle_sha256"], "proof.observer.bundle_sha256") != cfg["controller_bundle"]["sha256"]:
        _fail("proof.observer.bundle_sha256", "does not match controller bundle")
    observer_process = validate_process_identity(observer["process"], "proof.observer.process")
    if observer_process["executable_path"] != reg["guest"]["python_process"]["path"] or observer_process["executable_sha256"] != reg["guest"]["python_process"]["sha256"]:
        _fail("proof.observer.process", "does not match registered Python runtime")
    if observer_process["uid"] != reg["ssh"]["login_uid"]:
        _fail("proof.observer.process.uid", "does not match registered controller user")
    host = _exact(
        obj["host"],
        ("host_id", "machine_identity_sha256", "os", "os_version", "architecture", "kernel", "native_or_emulated", "hypervisor_evidence_sha256"),
        "proof.host",
    )
    _string(host["os_version"], "proof.host.os_version", maximum=128)
    _string(host["kernel"], "proof.host.kernel", maximum=256)
    expected_host = (
        cfg["host_id"], reg["guest"]["machine_identity_sha256"], cfg["expected"]["os"],
        cfg["expected"]["architecture"], cfg["expected"]["native_or_emulated"],
        reg["guest"]["hypervisor_evidence_sha256"],
    )
    actual_host = (
        host["host_id"], _digest(host["machine_identity_sha256"], "proof.host.machine_identity_sha256"),
        _literal(host["os"], ("macOS", "Debian 13"), "proof.host.os"),
        _literal(host["architecture"], ("arm64", "amd64"), "proof.host.architecture"),
        _literal(host["native_or_emulated"], ("native", "emulated"), "proof.host.native_or_emulated"),
        _digest(host["hypervisor_evidence_sha256"], "proof.host.hypervisor_evidence_sha256"),
    )
    if actual_host != expected_host:
        _fail("proof.host", "does not match registered and expected host identity")
    package = _exact(
        obj["package"],
        ("sha256", "manifest_sha256", "installed_version", "payload_manifest_sha256", "installation_receipt_sha256"),
        "proof.package",
    )
    for key in ("sha256", "manifest_sha256", "payload_manifest_sha256", "installation_receipt_sha256"):
        _digest(package[key], "proof.package." + key)
    _string(package["installed_version"], "proof.package.installed_version", maximum=128)
    if package["sha256"] != cfg["package"]["sha256"] or package["manifest_sha256"] != cfg["package_manifest"]["sha256"]:
        _fail("proof.package", "does not match candidate package bytes and manifest")
    if cfg["expected"]["product_version"] not in package["installed_version"]:
        _fail("proof.package.installed_version", "does not contain expected product version")
    service_common = (
        "mode", "target", "definition_sha256", "state", "generation", "post_probe_generation",
        "supervisor", "hub", "process_tree_sha256",
    )
    if cfg["expected"]["os"] == "Debian 13":
        platform_fields = ("invocation_id", "control_group", "fragment_sha256", "drop_in_manifest_sha256", "result", "exec_main_code", "exec_main_status")
    else:
        platform_fields = ("plist_sha256", "wrapper_script_sha256", "loaded_state", "app_receipt_sha256", "app_process")
    service = _exact(obj["service"], service_common + platform_fields, "proof.service")
    if service["mode"] != cfg["expected"]["service_mode"] or service["state"] != "running":
        _fail("proof.service", "does not describe the expected running installed service")
    for key in ("definition_sha256", "process_tree_sha256"):
        _digest(service[key], "proof.service." + key)
    supervisor = validate_process_identity(service["supervisor"], "proof.service.supervisor")
    hub = validate_process_identity(service["hub"], "proof.service.hub")
    stable_identities = (hub,) if cfg["expected"]["os"] == "Debian 13" else (supervisor, hub)
    if service["process_tree_sha256"] != _process_tree_digest(*stable_identities):
        _fail("proof.service.process_tree_sha256", "does not match observed stable process identities")
    service_uid = reg["guest"]["permitted_service_uid"]
    if supervisor["uid"] != service_uid or hub["uid"] != service_uid:
        _fail("proof.service.hub.uid", "does not match registered service UID")
    generation = "{}:{}".format(hub["boot_id"], hub["start_identity"])
    if service["generation"] != generation or service["post_probe_generation"] != generation:
        _fail("proof.service.generation", "changed during observation or was not derived from kernel identity")
    expected_executable = "/usr/bin/teslatlas-hub" if cfg["expected"]["os"] == "Debian 13" else "/Library/Application Support/Teslatlas Hub/bin/teslatlas-hub"
    if hub["executable_path"] != expected_executable or hub["executable_sha256"] != cfg["expected"]["hub_executable_sha256"]:
        _fail("proof.service.hub", "does not match fixed installed executable")
    if cfg["expected"]["os"] == "Debian 13":
        if service["target"] != "teslatlas-hub.service" or supervisor != hub:
            _fail("proof.service", "systemd supervisor and Hub must be the same MainPID")
        if service["control_group"] != "/system.slice/teslatlas-hub.service":
            _fail("proof.service.control_group", "does not match fixed unit cgroup")
        if not isinstance(service["invocation_id"], str) or re.fullmatch(r"[0-9a-f]{32}", service["invocation_id"]) is None:
            _fail("proof.service.invocation_id", "must be systemd InvocationID hex")
        for key in ("fragment_sha256", "drop_in_manifest_sha256"):
            _digest(service[key], "proof.service." + key)
        if service["definition_sha256"] != service["fragment_sha256"] or service["drop_in_manifest_sha256"] != hashlib.sha256(b"[]").hexdigest():
            _fail("proof.service", "does not bind the packaged fragment with no drop-ins")
        _string(service["result"], "proof.service.result", maximum=64)
        raw_exec_code = _string(service["exec_main_code"], "proof.service.exec_main_code", maximum=64)
        if not raw_exec_code.isdigit():
            _fail("proof.service.exec_main_code", "must be the numeric systemd machine property")
        _integer(service["exec_main_status"], "proof.service.exec_main_status", maximum=255)
        expected_config = "/etc/teslatlas-hub/config.toml"
        expected_data = "/var/lib/teslatlas-hub/interop-matrix/" + cfg["guest_run_id"] + "/hub"
    else:
        expected_target = "gui/{}/com.teslatlas.hub".format(reg["ssh"]["login_uid"])
        if service["target"] != expected_target:
            _fail("proof.service.target", "does not match fixed LaunchAgent target")
        if supervisor["executable_path"] != "/bin/bash":
            _fail("proof.service.supervisor", "does not match the wrapper's fixed interpreter")
        if hub["parent_pid"] != supervisor["pid"] or hub["pid"] == supervisor["pid"]:
            _fail("proof.service.hub", "is not the wrapper's direct Hub child")
        for key in ("plist_sha256", "wrapper_script_sha256", "app_receipt_sha256"):
            _digest(service[key], "proof.service." + key)
        if service["loaded_state"] != "loaded":
            _fail("proof.service.loaded_state", "must be loaded")
        app = validate_process_identity(service["app_process"], "proof.service.app_process")
        if app["uid"] != reg["ssh"]["login_uid"] or app["executable_path"] != "/Applications/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub":
            _fail("proof.service.app_process", "does not prove the fixed installed App launch")
        app_digest = hashlib.sha256(json.dumps(app, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()
        if service["definition_sha256"] != service["plist_sha256"] or service["app_receipt_sha256"] != app_digest:
            _fail("proof.service", "does not bind the packaged plist and observed App launch")
        expected_config = "/Users/{}/Library/Application Support/Teslatlas Hub/config.toml".format(reg["ssh"]["user"])
        expected_data = "/Users/{}/Library/Application Support/Teslatlas Hub/InteropMatrix/{}/hub".format(reg["ssh"]["user"], cfg["guest_run_id"])
    config = _exact(obj["config"], ("path", "sha256", "data_dir", "store_id", "store_schema_version", "scenario_sha256", "seed_sha256"), "proof.config")
    if _canonical_path(config["path"], "proof.config.path") != expected_config or _canonical_path(config["data_dir"], "proof.config.data_dir") != expected_data:
        _fail("proof.config", "does not use the fixed installed path and fresh matrix store")
    _token(config["store_id"], "proof.config.store_id")
    _integer(config["store_schema_version"], "proof.config.store_schema_version", minimum=1, maximum=2**31 - 1)
    for key in ("sha256", "scenario_sha256", "seed_sha256"):
        _digest(config[key], "proof.config." + key)
    if config["scenario_sha256"] != cfg["scenario"]["sha256"] or config["seed_sha256"] != cfg["seed"]["sha256"]:
        _fail("proof.config", "does not bind scenario and seed inputs")
    listener = _exact(obj["listener"], ("host", "port", "owner_pid", "socket_identity", "observed_at_monotonic_ns"), "proof.listener")
    _integer(listener["port"], "proof.listener.port", minimum=1, maximum=65535)
    _integer(listener["owner_pid"], "proof.listener.owner_pid", minimum=1, maximum=2**31 - 1)
    if listener["host"] != "127.0.0.1" or listener["port"] != 18480 or listener["owner_pid"] != hub["pid"]:
        _fail("proof.listener", "is not the fixed Hub loopback listener owned by the Hub generation")
    _string(listener["socket_identity"], "proof.listener.socket_identity", maximum=256)
    _integer(listener["observed_at_monotonic_ns"], "proof.listener.observed_at_monotonic_ns", minimum=1)
    tls = _exact(obj["tls"], ("endpoint", "certificate_der_sha256", "verified_chain", "verified_hostname", "redirect_count"), "proof.tls")
    redirect_count = _integer(tls["redirect_count"], "proof.tls.redirect_count", maximum=0)
    if tls["endpoint"] != "https://127.0.0.1:18480" or not _boolean(tls["verified_chain"], "proof.tls.verified_chain") or not _boolean(tls["verified_hostname"], "proof.tls.verified_hostname") or redirect_count != 0:
        _fail("proof.tls", "must be normal verified TLS with no redirects")
    _digest(tls["certificate_der_sha256"], "proof.tls.certificate_der_sha256")
    discovery = _exact(obj["discovery"], ("hub_id", "product_version", "response_sha256", "profile_id", "profile_sha256", "validated_response_set_sha256"), "proof.discovery")
    _token(discovery["hub_id"], "proof.discovery.hub_id")
    if discovery["product_version"] != cfg["expected"]["product_version"] or discovery["profile_id"] != "hub-http-v1@1.0.0" or discovery["profile_sha256"] != cfg["profile"]["sha256"]:
        _fail("proof.discovery", "does not match expected product and tested profile")
    for key in ("response_sha256", "profile_sha256", "validated_response_set_sha256"):
        _digest(discovery[key], "proof.discovery." + key)
    if config["store_id"] != discovery["hub_id"]:
        _fail("proof.config.store_id", "does not match discovered Hub installation identity")
    return public_observation(obj)
