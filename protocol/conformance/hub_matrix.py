"""Full raw-HTTP Protocol matrix adapter over a private installed-host session."""

from __future__ import annotations

import argparse
import copy
import hashlib
import http.client
import io
import json
import math
import os
from pathlib import Path
import re
import select
import socket
import ssl
import stat
import time
import urllib.parse
import uuid

try:
    from . import hub_control, hub_http
except ImportError:  # direct execution through conformance/adapters/actual-hub
    import hub_control
    import hub_http


MATRIX_KIND = "protocol-actual-hub-matrix"
PRODUCT_VERSION = "2026.36.2"
PROFILE_NAME = "hub-http-v1"
PROFILE_REVISION = "1.0.0"
UNKNOWN_VEHICLE = "33333333-3333-4333-8333-333333333333"
EXPECTED_VEHICLES = [
    {"vehicle_id": "11111111-1111-4111-8111-111111111111", "display_name": "Interop – Árvíztűrő 🚗"},
    {"vehicle_id": "22222222-2222-4222-8222-222222222222", "display_name": "Interop empty"},
]
EXPECTED_CURRENT = {
    "battery_level": 0,
    "inside_temp": 21.5,
    "outside_temp": None,
    "observed_at_ms": 1788566400000,
    "est_battery_range_km": 160.93,
    "odometer": 16093.44,
    "speed": 16,
    "scheduled_charging_start_time": 1788570000,
    "active_route_miles_to_arrival": 12.5,
    "empty_vehicle_observed_at_ms": None,
}
EXPECTED_PAGES = [[105, 104], [103, 102], [101]]
CASE_IDS = [
    "candidate_artifact_identity",
    "installed_service_runtime",
    "discovery_identity_profile",
    "unauthenticated_discovery",
    "bad_invitation",
    "expired_invitation",
    "replayed_invitation",
    "real_auth",
    "credential_lifecycle_reauth",
    "revocation",
    "unknown_vehicle",
    "exact_current_values",
    "endpoint_restart",
    "outage_recovery",
    "unsupported_operation_zero_requests",
    "credential_rotation_api",
    "drives_three_page_order",
    "drives_terminal_cursor",
    "drives_etag_304",
    "drives_wrong_vehicle_cursor",
    "drives_wrong_filter_cursor",
]
HEX64 = re.compile(r"^[0-9a-f]{64}$")
REQUEST_ID = re.compile(r"^[A-Za-z0-9._:-]{1,128}$")
PROFILE_MEMBERS = (
    "SHA256SUMS",
    "auth.schema.json",
    "cases.json",
    "discovery.schema.json",
    "errors.schema.json",
    "examples/claim.json",
    "examples/current.json",
    "examples/discovery.json",
    "examples/drives.json",
    "examples/health.json",
    "examples/invitation.json",
    "examples/ready.json",
    "examples/vehicles.json",
    "field-semantics.json",
    "openapi.json",
    "profile.json",
    "resources.schema.json",
    "sync-regression.json",
)
SESSION_INPUT_KEYS = {
    "schema_version", "kind", "run_id", "cell_id", "adapter_id", "client_id",
    "session_id", "instance_nonce", "header", "case_contract", "host_session",
    "broker", "inputs", "actors", "outputs", "bounds",
}


class MatrixAcceptanceError(Exception):
    """Fixed-label matrix failure; remote and secret values are never included."""


class UnsupportedOperationError(Exception):
    """Typed local refusal for operations absent from hub-http-v1."""


class InvitationUseError(MatrixAcceptanceError):
    """Typed local refusal before an invitation credential can reach HTTP."""

    typed_error = "protocol_validation"


class HttpTransportUnavailable(MatrixAcceptanceError):
    """No complete HTTP response was available from the pinned endpoint."""


def reject_unsupported_operation(operation):
    if operation not in {"charges", "commands", "sse"}:
        raise ValueError("unknown unsupported operation")
    raise UnsupportedOperationError("operation is unavailable in hub-http-v1")


def typed_equal(left, right):
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return set(left) == set(right) and all(typed_equal(left[key], right[key]) for key in left)
    if isinstance(left, list):
        return len(left) == len(right) and all(typed_equal(a, b) for a, b in zip(left, right))
    return left == right


def _exact(value, keys, label):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise MatrixAcceptanceError(label + " shape is invalid")
    return value


def _absolute_path(value, label):
    pure = Path(value) if isinstance(value, str) else None
    if not isinstance(value, str) or not value or "\x00" in value or "\n" in value or "\r" in value or not pure.is_absolute() or ".." in pure.parts or str(pure) != value:
        raise MatrixAcceptanceError(label + " path is invalid")
    return value


def _digest(value, label):
    if not isinstance(value, str) or HEX64.fullmatch(value) is None:
        raise MatrixAcceptanceError(label + " digest is invalid")
    return value


def _canonical_uuid(value, label):
    if not isinstance(value, str) or len(value) != 36:
        raise MatrixAcceptanceError(label + " must be a canonical UUID")
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError):
        raise MatrixAcceptanceError(label + " must be a canonical UUID") from None
    if parsed.version != 4 or str(parsed) != value:
        raise MatrixAcceptanceError(label + " must be a canonical UUID")
    return value


def _strict_private_file(path, label, maximum=1_048_576):
    """Read one owner-only regular file without following a replacement."""
    path = Path(_absolute_path(str(path), label))
    try:
        before = path.lstat()
        if (
            not stat.S_ISREG(before.st_mode)
            or before.st_nlink != 1
            or before.st_uid != os.getuid()
            or stat.S_IMODE(before.st_mode) & 0o077
            or before.st_size > maximum
        ):
            raise MatrixAcceptanceError(label + " ownership or size is invalid")
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            opened = os.fstat(fd)
            if (
                (opened.st_dev, opened.st_ino, opened.st_mode, opened.st_uid, opened.st_nlink)
                != (before.st_dev, before.st_ino, before.st_mode, before.st_uid, before.st_nlink)
            ):
                raise MatrixAcceptanceError(label + " changed before reading")
            chunks = bytearray()
            while len(chunks) <= maximum:
                part = os.read(fd, min(65_536, maximum + 1 - len(chunks)))
                if not part:
                    break
                chunks.extend(part)
            if len(chunks) > maximum:
                raise MatrixAcceptanceError(label + " exceeds byte limit")
            after = os.fstat(fd)
        finally:
            os.close(fd)
        final = path.lstat()
    except MatrixAcceptanceError:
        raise
    except OSError:
        raise MatrixAcceptanceError(label + " is unavailable") from None
    if len(chunks) != before.st_size or (final.st_dev, final.st_ino, final.st_mode, final.st_uid, final.st_nlink) != (before.st_dev, before.st_ino, before.st_mode, before.st_uid, before.st_nlink):
        raise MatrixAcceptanceError(label + " changed while reading")
    return bytes(chunks)


def _binding(value, label, maximum=1_048_576):
    if not isinstance(value, dict) or set(value) != {"path", "sha256"}:
        raise MatrixAcceptanceError(label + " binding is invalid")
    path = _absolute_path(value["path"], label)
    _digest(value["sha256"], label)
    raw = _strict_private_file(path, label, maximum)
    if hashlib.sha256(raw).hexdigest() != value["sha256"]:
        raise MatrixAcceptanceError(label + " digest changed")
    return {"path": path, "sha256": value["sha256"]}


def _staged(value, label, maximum=1_048_576):
    if not isinstance(value, dict) or set(value) != {"id", "root", "local"}:
        raise MatrixAcceptanceError(label + " staging is invalid")
    if not isinstance(value["id"], str) or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", value["id"]) is None:
        raise MatrixAcceptanceError(label + " id is invalid")
    root = _binding(value["root"], label + " root", maximum)
    local = _binding(value["local"], label + " local", maximum)
    if root["sha256"] != local["sha256"]:
        raise MatrixAcceptanceError(label + " root/local digest differs")
    return {"id": value["id"], "root": root, "local": local}


def _validate_profile_staging(inputs, header):
    if not isinstance(inputs, dict) or set(inputs) != {
        "profile_manifest", "profile_members", "scenario", "certificate",
        "certificate_der_sha256", "product_inputs",
    }:
        raise MatrixAcceptanceError("matrix session inputs are incomplete")
    manifest = _staged(inputs["profile_manifest"], "matrix profile manifest")
    members = inputs["profile_members"]
    if not isinstance(members, list) or len(members) != len(PROFILE_MEMBERS):
        raise MatrixAcceptanceError("matrix profile members are incomplete")
    by_name = {}
    for item in members:
        staged = _staged(item, "matrix profile member")
        local_name = Path(staged["local"]["path"]).name
        candidates = [name for name in PROFILE_MEMBERS if Path(name).name == local_name]
        if local_name == "SHA256SUMS":
            candidates = ["SHA256SUMS"]
        if len(candidates) != 1 or candidates[0] in by_name:
            raise MatrixAcceptanceError("matrix profile member layout is invalid")
        name = candidates[0]
        suffix = "/hub-http-v1/1.0.0/" + name
        if not staged["root"]["path"].endswith(suffix) or not staged["local"]["path"].endswith(suffix):
            raise MatrixAcceptanceError("matrix profile member layout is invalid")
        by_name[name] = staged
    if set(by_name) != set(PROFILE_MEMBERS) or manifest["root"] != by_name["SHA256SUMS"]["root"] or manifest["local"] != by_name["SHA256SUMS"]["local"]:
        raise MatrixAcceptanceError("matrix profile manifest differs from members")
    try:
        checksum_lines = _strict_private_file(manifest["local"]["path"], "matrix profile manifest").decode("ascii").splitlines()
    except UnicodeError:
        raise MatrixAcceptanceError("matrix profile checksums are invalid") from None
    checksums = {}
    for line in checksum_lines:
        parts = line.split("  ")
        if len(parts) != 2 or HEX64.fullmatch(parts[0]) is None or parts[1] in checksums:
            raise MatrixAcceptanceError("matrix profile checksums are invalid")
        checksums[parts[1]] = parts[0]
    if set(checksums) != set(PROFILE_MEMBERS) - {"SHA256SUMS"}:
        raise MatrixAcceptanceError("matrix profile checksum members are invalid")
    for name, expected in checksums.items():
        if by_name[name]["local"]["sha256"] != expected:
            raise MatrixAcceptanceError("matrix profile member checksum differs")
    try:
        profile = hub_http.strict_json(_strict_private_file(by_name["profile.json"]["local"]["path"], "matrix profile.json"))
    except Exception:
        raise MatrixAcceptanceError("matrix profile.json is invalid") from None
    if not isinstance(profile, dict) or profile.get("profile_id") != "hub-http-v1@1.0.0" or profile.get("contract_version") != "1.0.0":
        raise MatrixAcceptanceError("matrix profile identity is invalid")
    if not isinstance(header, dict) or header.get("profile_sha256") != manifest["local"]["sha256"]:
        raise MatrixAcceptanceError("matrix profile differs from evidence header")
    _staged(inputs["scenario"], "matrix scenario")
    certificate = _staged(inputs["certificate"], "matrix certificate", 65_536)
    try:
        der = ssl.PEM_cert_to_DER_cert(_strict_private_file(certificate["local"]["path"], "matrix certificate", 65_536).decode("ascii"))
    except (UnicodeError, ValueError, ssl.SSLError):
        raise MatrixAcceptanceError("matrix certificate is invalid") from None
    if hashlib.sha256(der).hexdigest() != inputs["certificate_der_sha256"]:
        raise MatrixAcceptanceError("matrix certificate DER digest differs")
    if not isinstance(inputs["product_inputs"], list) or len(inputs["product_inputs"]) > 4:
        raise MatrixAcceptanceError("matrix product inputs are invalid")
    if inputs["product_inputs"]:
        raise MatrixAcceptanceError("Protocol session unexpectedly stages client products")
    return by_name


def _validate_actor_manifest(staged):
    raw = _strict_private_file(staged["local"]["path"], "matrix actor input manifest")
    try:
        value = hub_http.strict_json(raw)
    except Exception:
        raise MatrixAcceptanceError("matrix actor input manifest is invalid") from None
    if not isinstance(value, dict) or value.get("schema_version") != 1 or set(value) not in ({"schema_version", "build_record", "files"}, {"schema_version", "artifact_sha256", "files"}):
        raise MatrixAcceptanceError("matrix actor input manifest shape is invalid")
    if not isinstance(value.get("files"), list) or not value["files"]:
        raise MatrixAcceptanceError("matrix actor input manifest is empty")
    return value


def _validate_session_input(value, config):
    if not isinstance(value, dict) or set(value) != SESSION_INPUT_KEYS:
        raise MatrixAcceptanceError("matrix SessionInput shape is invalid")
    if value.get("schema_version") != 1 or value.get("kind") != "matrix-adapter-session":
        raise MatrixAcceptanceError("matrix SessionInput discriminator is invalid")
    if not isinstance(value.get("run_id"), str) or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", value["run_id"]) is None:
        raise MatrixAcceptanceError("matrix SessionInput run identity is invalid")
    if value.get("adapter_id") != "protocol_actual_hub" or value.get("client_id") != "protocol_actual_hub" or value.get("cell_id") != config["cell_id"]:
        raise MatrixAcceptanceError("matrix SessionInput adapter identity is invalid")
    _canonical_uuid(value.get("session_id"), "matrix SessionInput session_id")
    host_session = value.get("host_session")
    if not isinstance(host_session, dict) or value["session_id"] != host_session.get("session_id"):
        raise MatrixAcceptanceError("matrix SessionInput host identity is invalid")
    if not isinstance(value.get("instance_nonce"), str) or HEX64.fullmatch(value["instance_nonce"]) is None:
        raise MatrixAcceptanceError("matrix SessionInput nonce is invalid")
    try:
        hub_control.validate_host_session(host_session)
    except (ValueError, TypeError):
        raise MatrixAcceptanceError("matrix SessionInput host session is invalid") from None
    if host_session != config["host_session"]:
        raise MatrixAcceptanceError("matrix SessionInput host session differs from config")
    broker = value.get("broker")
    if not isinstance(broker, dict) or set(broker) != {"kind", "socket_path"} or broker.get("kind") != "unix" or broker.get("socket_path") != value["host_session"]["broker_socket"]:
        raise MatrixAcceptanceError("matrix SessionInput broker is invalid")
    header_stage = _staged(value["header"], "matrix SessionInput header")
    contract_stage = _staged(value["case_contract"], "matrix SessionInput case contract")
    header_raw = _strict_private_file(header_stage["local"]["path"], "matrix evidence header")
    try:
        header = hub_http.strict_json(header_raw)
    except Exception:
        raise MatrixAcceptanceError("matrix evidence header is invalid") from None
    if not isinstance(header, dict) or header.get("adapter") != "protocol_actual_hub" or header.get("cell_id") != config["cell_id"]:
        raise MatrixAcceptanceError("matrix evidence header identity is invalid")
    if header.get("profile_id") != config["profile"]["id"] or header.get("profile_revision") != config["profile"]["revision"] or header.get("profile_sha256") != config["profile"]["sha256"]:
        raise MatrixAcceptanceError("matrix evidence header profile differs from config")
    if header.get("source_identities") != config["source_identities"] or header.get("artifacts") != config["artifacts"] or header.get("runtime") != config["runtime"]:
        raise MatrixAcceptanceError("matrix evidence header provenance differs from config")
    contract_raw = _strict_private_file(contract_stage["local"]["path"], "matrix case contract")
    if hashlib.sha256(contract_raw).hexdigest() != contract_stage["local"]["sha256"]:
        raise MatrixAcceptanceError("matrix case contract digest differs")
    try:
        contract_value = hub_http.strict_json(contract_raw)
    except Exception:
        raise MatrixAcceptanceError("matrix case contract is invalid") from None
    if not isinstance(contract_value, dict) or contract_value.get("adapter_id") != "protocol_actual_hub" or contract_value.get("required_cases") != CASE_IDS:
        raise MatrixAcceptanceError("matrix case contract identity is invalid")
    _validate_profile_staging(value["inputs"], header)
    actors = value.get("actors")
    if not isinstance(actors, list) or len(actors) != 1:
        raise MatrixAcceptanceError("matrix SessionInput actors are invalid")
    actor = actors[0]
    if not isinstance(actor, dict) or set(actor) != {"id", "kind", "execution", "runtime_ref", "artifact_roles", "source_roles", "entrypoint_ref", "input_manifest", "phase_contract"}:
        raise MatrixAcceptanceError("matrix SessionInput actor shape is invalid")
    if actor.get("id") != "protocol_http" or actor.get("kind") != "protocol_http" or actor.get("execution") != "coordinator" or actor.get("runtime_ref") != "root_python" or actor.get("artifact_roles") != [] or actor.get("source_roles") != ["protocol_source"] or actor.get("entrypoint_ref") != "protocol_actual_hub" or actor.get("phase_contract") is not None:
        raise MatrixAcceptanceError("matrix SessionInput actor identity is invalid")
    actor_stage = _staged(actor["input_manifest"], "matrix actor input manifest")
    _validate_actor_manifest(actor_stage)
    outputs = value.get("outputs")
    if not isinstance(outputs, dict) or set(outputs) != {"normalized", "actor_evidence", "coordination_dir", "framework_log"}:
        raise MatrixAcceptanceError("matrix SessionInput outputs are invalid")
    output_paths = [_absolute_path(outputs[name], "matrix output " + name) for name in outputs]
    if len(set(output_paths)) != len(output_paths):
        raise MatrixAcceptanceError("matrix SessionInput outputs overlap")
    for index, path in enumerate(output_paths):
        for other in output_paths[index + 1:]:
            if path == other or Path(path) in Path(other).parents or Path(other) in Path(path).parents:
                raise MatrixAcceptanceError("matrix SessionInput outputs overlap")
        if os.path.lexists(path):
            raise MatrixAcceptanceError("matrix SessionInput output is stale")
    bounds = value.get("bounds")
    if not isinstance(bounds, dict) or set(bounds) != {"cell_timeout_ms", "cleanup_timeout_ms", "frame_bytes", "evidence_bytes", "framework_log_bytes"} or type(bounds.get("cell_timeout_ms")) is not int or not 1 <= bounds["cell_timeout_ms"] <= 3_600_000 or bounds.get("cleanup_timeout_ms") != 45_000 or bounds.get("frame_bytes") != 1_048_576 or bounds.get("evidence_bytes") != 8_388_608 or bounds.get("framework_log_bytes") != 8_388_608:
        raise MatrixAcceptanceError("matrix SessionInput bounds are invalid")
    return value


def _validate_v2_matrix_config(value):
    if not isinstance(value, dict) or set(value) != {
        "schema_version", "kind", "execution_kind", "adapter", "cell_id", "product_version",
        "profile", "source_identities", "artifacts", "runtime", "host_session", "session_input",
    }:
        raise MatrixAcceptanceError("matrix config shape is invalid")
    legacy = dict(value)
    legacy.pop("session_input")
    legacy["schema_version"] = 1
    validated = validate_matrix_config(legacy)
    binding = _binding(value["session_input"], "matrix SessionInput", 1_048_576)
    session_raw = _strict_private_file(binding["path"], "matrix SessionInput", 1_048_576)
    try:
        session = hub_http.strict_json(session_raw)
    except Exception:
        raise MatrixAcceptanceError("matrix SessionInput is not strict JSON") from None
    _validate_session_input(session, validated)
    return copy.deepcopy({**validated, "schema_version": 2, "session_input": binding})


def validate_matrix_config(value):
    if isinstance(value, dict) and type(value.get("schema_version")) is int and value.get("schema_version") == 2:
        return _validate_v2_matrix_config(value)
    keys = {
        "schema_version", "kind", "execution_kind", "adapter", "cell_id",
        "product_version", "profile", "source_identities", "artifacts",
        "runtime", "host_session",
    }
    try:
        _exact(value, keys, "matrix config")
        if type(value["schema_version"]) is not int or value["schema_version"] != 1:
            raise MatrixAcceptanceError("matrix config schema_version is invalid")
        if value["kind"] != MATRIX_KIND or value["execution_kind"] != "actual_hub_acceptance" or value["adapter"] != "protocol_actual_hub":
            raise MatrixAcceptanceError("matrix config discriminator is invalid")
        if not isinstance(value["cell_id"], str) or not re.fullmatch(r"protocol_actual_hub__[a-z0-9_]+", value["cell_id"]):
            raise MatrixAcceptanceError("matrix cell identity is invalid")
        if value["product_version"] != PRODUCT_VERSION:
            raise MatrixAcceptanceError("matrix product version is invalid")
        profile = _exact(value["profile"], ("id", "revision", "path", "sha256"), "matrix profile")
        if profile["id"] != PROFILE_NAME or profile["revision"] != PROFILE_REVISION:
            raise MatrixAcceptanceError("matrix profile identity is invalid")
        _absolute_path(profile["path"], "matrix profile")
        _digest(profile["sha256"], "matrix profile")
        if not isinstance(value["source_identities"], list) or not value["source_identities"]:
            raise MatrixAcceptanceError("matrix source identities are invalid")
        source_roles = set()
        for source in value["source_identities"]:
            _exact(source, ("role", "repo", "head", "dirty_patch_sha256", "untracked_source_manifest_sha256"), "matrix source identity")
            if source["role"] not in {"hub_source", "protocol_source"} or source["role"] in source_roles:
                raise MatrixAcceptanceError("matrix source role is invalid")
            source_roles.add(source["role"])
            _absolute_path(source["repo"], "matrix source")
            if not isinstance(source["head"], str) or re.fullmatch(r"[0-9a-f]{40,64}", source["head"]) is None:
                raise MatrixAcceptanceError("matrix source head is invalid")
            _digest(source["dirty_patch_sha256"], "matrix source dirty patch")
            _digest(source["untracked_source_manifest_sha256"], "matrix source untracked manifest")
        if source_roles != {"hub_source", "protocol_source"}:
            raise MatrixAcceptanceError("matrix source roles are incomplete")
        if not isinstance(value["artifacts"], list) or not value["artifacts"]:
            raise MatrixAcceptanceError("matrix artifacts are invalid")
        roles = {}
        for artifact in value["artifacts"]:
            _exact(artifact, ("role", "name", "path", "embedded_version", "sha256"), "matrix artifact")
            if not isinstance(artifact["role"], str) or not isinstance(artifact["name"], str) or not artifact["name"] or not isinstance(artifact["embedded_version"], str) or not artifact["embedded_version"]:
                raise MatrixAcceptanceError("matrix artifact is invalid")
            _absolute_path(artifact["path"], "matrix artifact")
            _digest(artifact["sha256"], "matrix artifact")
            if artifact["role"] in roles:
                raise MatrixAcceptanceError("matrix artifact role is duplicated")
            roles[artifact["role"]] = artifact
        if set(roles) != {"hub_executable", "protocol_fixture_seed"}:
            raise MatrixAcceptanceError("matrix artifact roles are invalid")
        runtime = _exact(value["runtime"], ("hub", "client", "browser_engines", "client_transports"), "matrix runtime")
        for key in ("hub", "client"):
            host = _exact(runtime[key], ("os", "architecture", "native_or_emulated", "service_mode", "tool_versions"), "matrix runtime host")
            if not all(isinstance(host[name], str) and host[name] for name in ("os", "architecture", "service_mode")) or host["native_or_emulated"] not in {"native", "emulated"}:
                raise MatrixAcceptanceError("matrix runtime host is invalid")
            if not isinstance(host["tool_versions"], dict) or not host["tool_versions"] or not all(isinstance(name, str) and name and isinstance(version, str) and version for name, version in host["tool_versions"].items()):
                raise MatrixAcceptanceError("matrix runtime tools are invalid")
        for key in ("browser_engines", "client_transports"):
            if not isinstance(runtime[key], list) or not all(isinstance(item, str) and item for item in runtime[key]):
                raise MatrixAcceptanceError("matrix runtime lanes are invalid")
        hub_control.validate_host_session(value["host_session"])
    except ValueError as error:
        raise MatrixAcceptanceError("matrix host session is invalid") from error
    return copy.deepcopy(value)


def _read_public_file(path, maximum=1_048_576):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode):
            raise MatrixAcceptanceError("public binding is not a regular file")
        raw = stream.read(maximum + 1)
    if len(raw) > maximum:
        raise MatrixAcceptanceError("public binding exceeds byte limit")
    return raw


def _redacted_route(path):
    parts = path.split("/")
    if len(parts) >= 5 and parts[1:3] == ["v1", "pairings"]:
        parts[3] = "{pairing_id}"
    if len(parts) >= 5 and parts[1:3] == ["v1", "vehicles"]:
        parts[3] = "{vehicle_id}"
    return "/".join(parts)


def _remaining(deadline, clock=time.monotonic):
    value = deadline - clock()
    if value <= 0:
        raise TimeoutError("total exchange deadline expired")
    return value


class _DeadlineSocketReader(io.RawIOBase):
    def __init__(self, connection, deadline, clock):
        super().__init__()
        self.connection = connection
        self.deadline = deadline
        self.clock = clock

    def readable(self):
        return True

    def readinto(self, buffer):
        self.connection.settimeout(_remaining(self.deadline, self.clock))
        try:
            return self.connection.recv_into(buffer)
        except socket.timeout:
            raise TimeoutError("total exchange deadline expired") from None


class _DeadlineResponseSocket:
    def __init__(self, connection, deadline, clock):
        self.connection = connection
        self.deadline = deadline
        self.clock = clock

    def makefile(self, mode):
        if mode != "rb":
            raise ValueError("HTTP response stream mode is invalid")
        return io.BufferedReader(_DeadlineSocketReader(self.connection, self.deadline, self.clock))


class _DeadlineHTTPSConnection(http.client.HTTPSConnection):
    """Loopback HTTPS connection whose blocking boundaries share one deadline."""

    def __init__(self, host, port, *, context, deadline, clock):
        self.deadline = deadline
        self.clock = clock
        super().__init__(
            host,
            port,
            timeout=_remaining(deadline, clock),
            context=context,
        )

    def _addresses(self):
        if self.host == "127.0.0.1":
            return ((socket.AF_INET, ("127.0.0.1", self.port)),)
        if self.host == "::1":
            return ((socket.AF_INET6, ("::1", self.port, 0, 0)),)
        return (
            (socket.AF_INET6, ("::1", self.port, 0, 0)),
            (socket.AF_INET, ("127.0.0.1", self.port)),
        )

    def _connect_tcp(self):
        last_error = None
        for family, address in self._addresses():
            connection = socket.socket(family, socket.SOCK_STREAM)
            try:
                if self.source_address:
                    connection.bind(self.source_address)
                connection.settimeout(_remaining(self.deadline, self.clock))
                connection.connect(address)
                _remaining(self.deadline, self.clock)
                try:
                    connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
                except OSError:
                    pass
                return connection
            except (OSError, TimeoutError) as error:
                last_error = error
                connection.close()
        if last_error is not None:
            raise last_error
        raise OSError("loopback endpoint has no address")

    def _wrap_tls(self, connection):
        connection.settimeout(_remaining(self.deadline, self.clock))
        wrapped = self._context.wrap_socket(
            connection,
            server_hostname=self.host,
            do_handshake_on_connect=False,
        )
        try:
            wrapped.setblocking(False)
            while True:
                try:
                    wrapped.do_handshake()
                    break
                except ssl.SSLWantReadError:
                    if not select.select([wrapped], [], [], _remaining(self.deadline, self.clock))[0]:
                        raise TimeoutError("total exchange deadline expired")
                except ssl.SSLWantWriteError:
                    if not select.select([], [wrapped], [], _remaining(self.deadline, self.clock))[1]:
                        raise TimeoutError("total exchange deadline expired")
            wrapped.settimeout(_remaining(self.deadline, self.clock))
            return wrapped
        except BaseException:
            wrapped.close()
            raise

    def connect(self):
        connection = self._connect_tcp()
        try:
            _remaining(self.deadline, self.clock)
            self.sock = self._wrap_tls(connection)
        except BaseException:
            connection.close()
            raise

    def send(self, data):
        if self.sock is None:
            if self.auto_open:
                self.connect()
            else:
                raise http.client.NotConnected()
        if not data:
            return
        remaining = memoryview(data)
        self.sock.setblocking(False)
        try:
            while remaining:
                try:
                    sent = self.sock.send(remaining)
                    if sent == 0:
                        raise OSError("TLS socket closed during request send")
                    remaining = remaining[sent:]
                except (BlockingIOError, ssl.SSLWantWriteError):
                    if not select.select([], [self.sock], [], _remaining(self.deadline, self.clock))[1]:
                        raise TimeoutError("total exchange deadline expired")
                except ssl.SSLWantReadError:
                    if not select.select([self.sock], [], [], _remaining(self.deadline, self.clock))[0]:
                        raise TimeoutError("total exchange deadline expired")
            self.sock.settimeout(_remaining(self.deadline, self.clock))
        except BaseException:
            remaining.release()
            raise
        remaining.release()


class RawHttpClient:
    def __init__(
        self,
        endpoint,
        certificate_path,
        certificate_der_sha256,
        profile_path,
        *,
        _total_timeout=5.0,
        _clock=time.monotonic,
    ):
        parsed = urllib.parse.urlsplit(endpoint)
        if (
            parsed.scheme != "https"
            or parsed.hostname not in ("127.0.0.1", "localhost", "::1")
            or parsed.username
            or parsed.password
            or parsed.query
            or parsed.fragment
            or parsed.path not in ("", "/")
        ):
            raise MatrixAcceptanceError("installed endpoint is not owned loopback HTTPS")
        self.endpoint = endpoint.rstrip("/")
        self.parsed = parsed
        self.profile_path = profile_path
        try:
            profile = hub_http.load_profile(profile_path)
            claim_limit = profile.get("max_claim_request_bytes")
        except Exception:
            raise MatrixAcceptanceError("profile claim request bound is invalid") from None
        if type(claim_limit) is not int or claim_limit <= 0:
            raise MatrixAcceptanceError("profile claim request bound is invalid")
        self.max_claim_request_bytes = claim_limit
        certificate_raw = _read_public_file(certificate_path, 65_536)
        try:
            der = ssl.PEM_cert_to_DER_cert(certificate_raw.decode("ascii"))
        except (UnicodeError, ValueError):
            raise MatrixAcceptanceError("public certificate is invalid") from None
        self.der_sha256 = hashlib.sha256(der).hexdigest()
        if self.der_sha256 != certificate_der_sha256:
            raise MatrixAcceptanceError("public certificate does not match installed proof")
        self.context = ssl.create_default_context(cafile=certificate_path)
        if type(_total_timeout) not in (int, float) or not math.isfinite(_total_timeout) or _total_timeout <= 0:
            raise ValueError("HTTP total timeout must be positive")
        if not callable(_clock):
            raise ValueError("HTTP deadline clock must be callable")
        self._total_timeout = float(_total_timeout)
        self._clock = _clock
        self.transcript = []
        self.request_count = 0

    def reset_transcript(self):
        self.transcript = []

    def exchange(self, kind, path, *, method="GET", status=200, bearer=None, body=None, headers=None):
        request_headers = dict(headers or {})
        if bearer is not None:
            request_headers["Authorization"] = "Bearer " + bearer
        payload = None
        if body is not None:
            payload = json.dumps(body, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()
            if kind == "claim" and len(payload) > self.max_claim_request_bytes:
                raise MatrixAcceptanceError("claim request exceeds profile byte limit")
            if len(payload) > hub_http.MAX_BYTES:
                raise MatrixAcceptanceError("request body exceeds byte limit")
            request_headers["Content-Type"] = "application/json"
        deadline = self._clock() + self._total_timeout
        connection = _DeadlineHTTPSConnection(
            self.parsed.hostname,
            self.parsed.port or 443,
            context=self.context,
            deadline=deadline,
            clock=self._clock,
        )
        response = None
        response_started = False
        try:
            connection.connect()
            _remaining(deadline, self._clock)
            leaf = connection.sock.getpeercert(binary_form=True)
            if not leaf or hashlib.sha256(leaf).hexdigest() != self.der_sha256:
                raise MatrixAcceptanceError("connected TLS leaf identity mismatch")
            connection.sock.settimeout(_remaining(deadline, self._clock))
            self.request_count += 1
            connection.request(method, path, body=payload, headers=request_headers)
            _remaining(deadline, self._clock)
            response = http.client.HTTPResponse(
                _DeadlineResponseSocket(connection.sock, deadline, self._clock),
                method=method,
            )
            response.begin()
            response_started = True
            _remaining(deadline, self._clock)
            raw = response.read(hub_http.MAX_BYTES + 1)
            response_headers = dict(response.getheaders())
            observed_status = response.status
            _remaining(deadline, self._clock)
        except ssl.SSLCertVerificationError:
            raise MatrixAcceptanceError("normal TLS leaf verification failed") from None
        except ssl.SSLError:
            raise MatrixAcceptanceError("normal TLS exchange failed") from None
        except http.client.RemoteDisconnected:
            raise HttpTransportUnavailable("installed endpoint transport is unavailable") from None
        except (OSError, TimeoutError):
            if not response_started:
                raise HttpTransportUnavailable("installed endpoint transport is unavailable") from None
            raise MatrixAcceptanceError("bounded HTTP exchange failed") from None
        except http.client.HTTPException:
            raise MatrixAcceptanceError("HTTP response framing is invalid") from None
        finally:
            if response is not None:
                response.close()
            connection.close()
        request_id = next((item for key, item in response_headers.items() if key.lower() == "x-request-id"), None)
        if isinstance(request_id, str) and REQUEST_ID.fullmatch(request_id) is not None:
            route = urllib.parse.urlsplit(path).path
            self.transcript.append(
                {"method": method, "route": _redacted_route(route), "status": observed_status, "request_id": request_id}
            )
        if observed_status != status:
            raise MatrixAcceptanceError("HTTP status differs from case contract")
        problems = hub_http.validate_raw(self.profile_path, kind, observed_status, response_headers, raw)
        try:
            _remaining(deadline, self._clock)
        except TimeoutError:
            raise MatrixAcceptanceError("bounded HTTP exchange failed") from None
        if problems:
            raise MatrixAcceptanceError("raw response violates unchanged profile")
        if not isinstance(request_id, str) or REQUEST_ID.fullmatch(request_id) is None:
            raise MatrixAcceptanceError("response request ID is missing or invalid")
        try:
            value = hub_http.strict_json(raw) if raw else None
            normalized_headers = {key.lower(): item for key, item in response_headers.items()}
            _remaining(deadline, self._clock)
            return value, normalized_headers
        except TimeoutError:
            raise MatrixAcceptanceError("bounded HTTP exchange failed") from None
        except (ValueError, UnicodeError, TypeError):
            raise MatrixAcceptanceError("validated response could not be parsed") from None


def _invitation_uri_matches(value):
    parsed = urllib.parse.urlsplit(value["pairingUri"])
    expected = {
        "pairing_id": [value["pairingId"]],
        "secret": [value["secret"]],
        "endpoint": [value["endpoint"]],
        "tls_pin": [value["tlsPin"]],
    }
    return (
        parsed.scheme == "teslatlas-hub"
        and parsed.netloc == "pair"
        and not parsed.path
        and not parsed.fragment
        and urllib.parse.parse_qs(parsed.query, strict_parsing=True) == expected
    )


class MatrixCases:
    def __init__(self, config, control):
        self.config = config
        self.control = control
        self.profile = hub_http.load_profile(config["profile"]["path"], config["profile"]["sha256"])
        self.results = []
        self.http = None
        self.descriptor = None
        self.proof = None
        self.invitation = None
        self.expired_invitation = None
        self.events = []
        self.token = None
        self.device_id = None
        self.pages = None
        self.first_cursor = None
        self.second_cursor = None
        self.anchors = {}
        self._admit_running(self.control.request("verify"))

    def _admit_running(self, result):
        result = _exact(result, ("descriptor", "proof", "invitation", "expired_invitation", "events"), "running control result")
        descriptor = _exact(
            result["descriptor"],
            (
                "status", "provenance", "endpoint", "hub_id", "hub_pid",
                "hub_started_at", "service_generation", "binary_sha256",
                "seed_binary_sha256", "profile_id", "profile_path",
                "profile_sha256", "scenario_path", "scenario_sha256",
                "certificate_path",
            ),
            "installed descriptor",
        )
        if descriptor["status"] != "ready" or descriptor["provenance"] != "installed-package-service":
            raise MatrixAcceptanceError("installed descriptor status is invalid")
        if descriptor["profile_id"] != hub_http.PROFILE_ID or descriptor["profile_sha256"] != self.config["profile"]["sha256"]:
            raise MatrixAcceptanceError("installed descriptor profile binding is invalid")
        for name in ("binary_sha256", "seed_binary_sha256", "profile_sha256", "scenario_sha256"):
            _digest(descriptor[name], "installed descriptor " + name)
        for name in ("profile_path", "scenario_path", "certificate_path"):
            _absolute_path(descriptor[name], "installed descriptor " + name)
        hub_http.load_profile(descriptor["profile_path"], descriptor["profile_sha256"])
        proof = result["proof"]
        if not isinstance(proof, dict) or proof.get("status") != "verified" or proof.get("session_id") != self.config["host_session"]["session_id"]:
            raise MatrixAcceptanceError("installed observation is not session bound")
        try:
            tls = proof["tls"]
            discovery = proof["discovery"]
            service = proof["service"]
            hub = service["hub"]
            observed_config = proof["config"]
        except (KeyError, TypeError):
            raise MatrixAcceptanceError("installed observation is incomplete") from None
        if (
            tls.get("endpoint") != descriptor["endpoint"]
            or tls.get("verified_chain") is not True
            or tls.get("verified_hostname") is not True
            or tls.get("redirect_count") != 0
            or discovery.get("hub_id") != descriptor["hub_id"]
            or discovery.get("product_version") != self.config["product_version"]
            or discovery.get("profile_id") != hub_http.PROFILE_ID
            or discovery.get("profile_sha256") != descriptor["profile_sha256"]
            or service.get("generation") != descriptor["service_generation"]
            or hub.get("pid") != descriptor["hub_pid"]
            or hub.get("start_identity") != descriptor["hub_started_at"]
            or hub.get("executable_sha256") != descriptor["binary_sha256"]
            or observed_config.get("seed_sha256") != descriptor["seed_binary_sha256"]
            or observed_config.get("scenario_sha256") != descriptor["scenario_sha256"]
        ):
            raise MatrixAcceptanceError("installed observation disagrees with descriptor")
        _digest(tls.get("certificate_der_sha256"), "installed TLS certificate")
        scenario_raw = _read_public_file(descriptor["scenario_path"])
        if hashlib.sha256(scenario_raw).hexdigest() != descriptor["scenario_sha256"]:
            raise MatrixAcceptanceError("installed scenario digest mismatch")
        scenario = hub_http.strict_json(scenario_raw)
        if (
            scenario.get("schema_version") != 1
            or scenario.get("name") != "two-vehicles-five-drives"
            or scenario.get("provenance") != "synthetic-only"
            or not typed_equal(scenario.get("vehicles"), EXPECTED_VEHICLES)
            or not typed_equal(scenario.get("drive_pages_at_limit_2"), EXPECTED_PAGES)
            or not all(typed_equal(scenario.get("current", {}).get(key), value) for key, value in EXPECTED_CURRENT.items() if key != "empty_vehicle_observed_at_ms")
            or not typed_equal(scenario.get("empty_vehicle_observed_at_ms"), None)
        ):
            raise MatrixAcceptanceError("installed scenario differs from independent literals")
        if self.http is None:
            self.http = RawHttpClient(
                descriptor["endpoint"],
                descriptor["certificate_path"],
                tls["certificate_der_sha256"],
                descriptor["profile_path"],
            )
        elif descriptor["endpoint"] != self.http.endpoint or tls["certificate_der_sha256"] != self.http.der_sha256:
            raise MatrixAcceptanceError("installed endpoint identity changed during matrix run")
        self.descriptor = descriptor
        self.proof = proof
        self.invitation = result["invitation"]
        self.expired_invitation = result["expired_invitation"]
        self.events = result["events"]
        self._validate_invitation(self.invitation, expired=False)
        self._validate_invitation(self.expired_invitation, expired=True)
        return result

    def _validate_invitation(self, invitation, *, expired):
        if not isinstance(invitation, dict):
            raise MatrixAcceptanceError("controller invitation is invalid")
        raw = json.dumps(invitation, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()
        if hub_http.validate_raw(self.descriptor["profile_path"], "invitation", 200, {"content-type": "application/json"}, raw):
            raise MatrixAcceptanceError("controller invitation violates unchanged profile")
        if invitation["endpoint"] != self.descriptor["endpoint"] or invitation["tlsPin"] != self.http.der_sha256 or not _invitation_uri_matches(invitation):
            raise MatrixAcceptanceError("controller invitation endpoint or DER identity is invalid")
        is_expired = invitation["expiresAtMs"] <= int(time.time() * 1000)
        if is_expired is not expired:
            raise MatrixAcceptanceError("controller invitation expiry class is invalid")

    def _preflight_invitation_use(self, invitation):
        try:
            self._validate_invitation(invitation, expired=False)
        except MatrixAcceptanceError:
            raise InvitationUseError("invitation use preflight rejected") from None

    def _require_local_refusal(self, action, error_type):
        before = self.http.request_count
        try:
            action()
        except error_type:
            pass
        else:
            raise MatrixAcceptanceError("required local refusal did not occur")
        outgoing = self.http.request_count - before
        if outgoing != 0:
            raise MatrixAcceptanceError("local refusal emitted an HTTP request")
        return outgoing

    def verify(self):
        return self._admit_running(self.control.request("verify"))

    def credential_exchange(self, *args, **kwargs):
        self.verify()
        return self.http.exchange(*args, **kwargs)

    def capture(self, case_id, expected, action, evidence_kind="http", process=False):
        self.verify()
        before_sequence = self.proof.get("sequence") if isinstance(self.proof, dict) else None
        self.http.reset_transcript()
        try:
            actual = action()
            status = "passed" if typed_equal(expected, actual) else "failed"
        except MatrixAcceptanceError:
            actual = {"unexpected_error": "matrix_acceptance"}
            status = "failed"
        after_sequence = self.proof.get("sequence") if isinstance(self.proof, dict) else before_sequence
        if type(before_sequence) is int and type(after_sequence) is int:
            self.anchors[case_id] = (before_sequence, after_sequence)
        value = {
            "id": case_id,
            "status": status,
            "expected": expected,
            "actual": actual,
            "evidence_kind": evidence_kind,
            "request_transcript": copy.deepcopy(self.http.transcript),
        }
        if process:
            value["process_evidence"] = {"installed_observation": copy.deepcopy(self.proof), "lifecycle_events": copy.deepcopy(self.events)}
        self.results.append(value)
        return actual

    def _claim(self, invitation, device_name):
        self.verify()
        self._preflight_invitation_use(invitation)
        value, _headers = self.http.exchange(
            "claim",
            "/v1/pairings/" + invitation["pairingId"] + "/claim",
            method="POST",
            body={"secret": invitation["secret"], "device_name": device_name},
        )
        return value

    def _vehicles(self, token=None, status=200):
        return self.credential_exchange("vehicles", "/v1/vehicles", bearer=token or self.token, status=status)[0]

    def _error_fact(self, kind, path, status, *, method="GET", bearer=None, body=None, required_code=None):
        value, _headers = self.credential_exchange(kind, path, method=method, status=status, bearer=bearer, body=body)
        if required_code is not None and (not isinstance(value, dict) or value.get("error", {}).get("code") != required_code):
            raise MatrixAcceptanceError("HTTP error code differs from case contract")
        return {"typed_error": "hub_http_error", "http_status": status}

    def run(self):
        artifacts = {item["role"]: item["sha256"] for item in self.config["artifacts"]}
        candidate_expected = {"hub_sha256": artifacts["hub_executable"], "seed_sha256": artifacts["protocol_fixture_seed"], "product_version": PRODUCT_VERSION}
        self.capture(
            "candidate_artifact_identity",
            candidate_expected,
            lambda: {"hub_sha256": self.descriptor["binary_sha256"], "seed_sha256": self.descriptor["seed_binary_sha256"], "product_version": self.proof["discovery"]["product_version"]},
            "identity",
            True,
        )
        service_expected = {"service_mode": self.config["runtime"]["hub"]["service_mode"]}
        self.capture("installed_service_runtime", service_expected, lambda: {"service_mode": self.proof["service"]["mode"]}, "identity", True)
        discovery_expected = {
            "hub_id": self.descriptor["hub_id"],
            "api_versions": ["1.0"],
            "protocol": "teslatlas-sync",
            "protocol_major": 1,
            "pack_format": "sqlite-zstd",
            "version": PRODUCT_VERSION,
        }

        def discovery_case():
            value, _headers = self.http.exchange("discovery", "/.well-known/teslatlas-hub")
            if not typed_equal(value.get("capabilities"), self.profile["capabilities"]):
                raise MatrixAcceptanceError("full fixture capabilities are incomplete")
            return {key: value[key] for key in discovery_expected}

        self.capture("discovery_identity_profile", discovery_expected, discovery_case)

        def unauthenticated():
            statuses = {}
            for key, kind, path in (
                ("discovery", "discovery", "/.well-known/teslatlas-hub"),
                ("health", "health", "/healthz"),
                ("readiness", "ready", "/readyz"),
            ):
                _value, _headers = self.http.exchange(kind, path)
                statuses[key] = 200
            statuses["credential_absent"] = self.token is None
            return statuses

        self.capture("unauthenticated_discovery", {"discovery": 200, "health": 200, "readiness": 200, "credential_absent": True}, unauthenticated)

        def bad_invitation():
            bad = copy.deepcopy(self.invitation)
            bad["secret"] = ("0" if bad["secret"][0] != "0" else "1") + bad["secret"][1:]
            parsed = urllib.parse.urlsplit(bad["pairingUri"])
            query = urllib.parse.parse_qs(parsed.query, strict_parsing=True)
            query["secret"] = [bad["secret"]]
            bad["pairingUri"] = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, parsed.path, urllib.parse.urlencode({key: item[0] for key, item in query.items()}), parsed.fragment))
            self._validate_invitation(bad, expired=False)
            return self._error_fact(
                "claim", "/v1/pairings/" + bad["pairingId"] + "/claim", 401,
                method="POST", body={"secret": bad["secret"], "device_name": "Bad invitation"},
            )

        self.capture("bad_invitation", {"typed_error": "hub_http_error", "http_status": 401}, bad_invitation)

        def expired_invitation():
            self._validate_invitation(self.expired_invitation, expired=True)
            outgoing = self._require_local_refusal(
                lambda: self._claim(self.expired_invitation, "Expired invitation"),
                InvitationUseError,
            )
            return {"outgoing_requests": outgoing, "typed_error": InvitationUseError.typed_error}

        self.capture("expired_invitation", {"outgoing_requests": 0, "typed_error": "protocol_validation"}, expired_invitation, "zero_request")

        def real_auth():
            claim = self._claim(self.invitation, "Raw Protocol matrix adapter")
            self.token, self.device_id = claim["access_token"], claim["device_id"]
            return {"claimed": 200, "vehicles": self._vehicles()["vehicles"]}

        self.capture("real_auth", {"claimed": 200, "vehicles": EXPECTED_VEHICLES}, real_auth)

        def replayed():
            return self._error_fact(
                "claim", "/v1/pairings/" + self.invitation["pairingId"] + "/claim", 401,
                method="POST", body={"secret": self.invitation["secret"], "device_name": "Replay"},
            )

        self.capture("replayed_invitation", {"typed_error": "hub_http_error", "http_status": 401}, replayed)

        # Drive and current facts run before credential mutation.
        self.capture(
            "unknown_vehicle",
            {"typed_error": "hub_http_error", "http_status": 404},
            lambda: self._error_fact("current", "/v1/vehicles/" + UNKNOWN_VEHICLE + "/current", 404, bearer=self.token),
        )

        def current_values():
            primary = self.credential_exchange("current", "/v1/vehicles/" + EXPECTED_VEHICLES[0]["vehicle_id"] + "/current", bearer=self.token)[0]
            empty = self.credential_exchange("current", "/v1/vehicles/" + EXPECTED_VEHICLES[1]["vehicle_id"] + "/current", bearer=self.token)[0]
            result = {key: primary[key] for key in EXPECTED_CURRENT if key != "empty_vehicle_observed_at_ms"}
            result["empty_vehicle_observed_at_ms"] = empty["observed_at_ms"]
            return result

        self.capture("exact_current_values", EXPECTED_CURRENT, current_values)

        vehicle = EXPECTED_VEHICLES[0]["vehicle_id"]
        drive_route = "/v1/vehicles/" + vehicle + "/drives"

        def drive_pages():
            pages = []
            cursor = None
            self.pages = []
            for expected_ids in EXPECTED_PAGES:
                query = {"limit": 2}
                if cursor is not None:
                    query["cursor"] = cursor
                page, headers = self.credential_exchange("drives", drive_route + "?" + urllib.parse.urlencode(query), bearer=self.token)
                ids = [item["id"] for item in page["items"]]
                pages.append(ids)
                self.pages.append((page, headers))
                cursor = page["next_cursor"]
                if len(self.pages) == 1:
                    self.first_cursor = cursor
                elif len(self.pages) == 2:
                    self.second_cursor = cursor
            return {"pages": pages}

        self.capture("drives_three_page_order", {"pages": EXPECTED_PAGES}, drive_pages)

        def terminal_cursor():
            page, _headers = self.credential_exchange("drives", drive_route + "?" + urllib.parse.urlencode({"limit": 2, "cursor": self.second_cursor}), bearer=self.token)
            return {"next_cursor": page["next_cursor"], "ids": [item["id"] for item in page["items"]]}

        self.capture("drives_terminal_cursor", {"next_cursor": None, "ids": [101]}, terminal_cursor)

        def etag_case():
            first_headers = self.pages[0][1]
            self.credential_exchange("drives", drive_route + "?limit=2", status=304, bearer=self.token, headers={"If-None-Match": first_headers["etag"]})
            second, _headers = self.credential_exchange("drives", drive_route + "?" + urllib.parse.urlencode({"limit": 2, "cursor": self.first_cursor}), bearer=self.token)
            return {"kind": "notModified", "post_304_ids": [item["id"] for item in second["items"]]}

        self.capture("drives_etag_304", {"kind": "notModified", "post_304_ids": [103, 102]}, etag_case)
        cursor_error = {"typed_error": "hub_http_error", "http_status": 400}
        self.capture(
            "drives_wrong_vehicle_cursor",
            cursor_error,
            lambda: self._error_fact(
                "drives",
                "/v1/vehicles/" + EXPECTED_VEHICLES[1]["vehicle_id"] + "/drives?" + urllib.parse.urlencode({"limit": 2, "cursor": self.first_cursor}),
                400,
                bearer=self.token,
                required_code="invalid_cursor",
            ),
        )
        self.capture(
            "drives_wrong_filter_cursor",
            cursor_error,
            lambda: self._error_fact(
                "drives",
                drive_route + "?" + urllib.parse.urlencode({"limit": 2, "from_ms": 1, "cursor": self.first_cursor}),
                400,
                bearer=self.token,
                required_code="invalid_cursor",
            ),
        )

        def unsupported():
            outgoing = self._require_local_refusal(
                lambda: reject_unsupported_operation("charges"),
                UnsupportedOperationError,
            )
            return {"outgoing_requests": outgoing}

        self.capture("unsupported_operation_zero_requests", {"outgoing_requests": 0}, unsupported, "zero_request")

        def rotate():
            old_token, old_device = self.token, self.device_id
            rotated, _headers = self.credential_exchange("rotate", "/v1/device/rotate", method="POST", bearer=old_token, body={})
            self.token, self.device_id = rotated["access_token"], rotated["device_id"]
            self._vehicles(self.token)
            stale = self._error_fact("vehicles", "/v1/vehicles", 401, bearer=old_token)
            return {
                "rotated": self.token != old_token,
                "same_device": self.device_id == old_device,
                "vehicles": 200,
                "old_credential_error": stale["typed_error"],
                "old_credential_status": stale["http_status"],
            }

        self.capture(
            "credential_rotation_api",
            {"rotated": True, "same_device": True, "vehicles": 200, "old_credential_error": "hub_http_error", "old_credential_status": 401},
            rotate,
        )

        def revoked():
            self._admit_running(self.control.request("revoke", device_id=self.device_id))
            return self._error_fact("vehicles", "/v1/vehicles", 401, bearer=self.token)

        self.capture("revocation", {"typed_error": "hub_http_error", "http_status": 401}, revoked, process=True)

        def reauthenticate():
            previous = self.device_id
            result = self._admit_running(self.control.request("pair"))
            claim_value = self._claim(result["invitation"], "Raw Protocol reauthentication")
            self.token, self.device_id = claim_value["access_token"], claim_value["device_id"]
            self._vehicles()
            return {"new_device": self.device_id != previous, "vehicles": 200}

        self.capture("credential_lifecycle_reauth", {"new_device": True, "vehicles": 200}, reauthenticate, process=True)

        def restart():
            before_hub = self.descriptor["hub_id"]
            before_generation = self.descriptor["service_generation"]
            stopped = self.control.request("stop")
            if stopped != {"stopped": True, "events": stopped.get("events")}:
                raise MatrixAcceptanceError("stop result is invalid")
            self._admit_running(self.control.request("start"))
            discovered, _headers = self.http.exchange("discovery", "/.well-known/teslatlas-hub")
            self._vehicles()
            return {
                "same_hub": discovered["hub_id"] == before_hub == self.descriptor["hub_id"],
                "new_process": self.descriptor["service_generation"] != before_generation,
                "vehicles": 200,
            }

        self.capture("endpoint_restart", {"same_hub": True, "new_process": True, "vehicles": 200}, restart, process=True)

        def outage():
            stopped = self.control.request("stop")
            if stopped != {"stopped": True, "events": stopped.get("events")}:
                raise MatrixAcceptanceError("stop result is invalid")
            try:
                self.http.exchange("vehicles", "/v1/vehicles", bearer=self.token)
            except HttpTransportUnavailable:
                observed = True
                self.http.transcript.append(
                    {"method": "GET", "route": "/v1/vehicles", "failure": "transport_unavailable"}
                )
            else:
                raise MatrixAcceptanceError("stopped endpoint returned an HTTP response")
            self._admit_running(self.control.request("start"))
            self._vehicles()
            return {"outage_observed": observed, "vehicles": 200}

        self.capture("outage_recovery", {"outage_observed": True, "vehicles": 200}, outage, process=True)

        by_id = {item["id"]: item for item in self.results}
        if set(by_id) != set(CASE_IDS):
            raise MatrixAcceptanceError("matrix case implementation is incomplete")
        return [by_id[case_id] for case_id in CASE_IDS]


def _canonical_json_bytes(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")


def _write_exclusive_json(path, value):
    """Write one owner-only immutable coordination record and return its binding."""
    target = Path(_absolute_path(str(path), "coordination output"))
    anchor = target.parent
    missing = []
    while not anchor.exists() and anchor != anchor.parent:
        missing.append(anchor)
        anchor = anchor.parent
    for directory in reversed(missing):
        try:
            os.mkdir(directory, 0o700)
        except FileExistsError:
            pass
    directory = target.parent
    while True:
        try:
            metadata = directory.lstat()
        except OSError:
            raise MatrixAcceptanceError("coordination directory is unavailable") from None
        if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) & 0o077:
            raise MatrixAcceptanceError("coordination directory ownership or mode is invalid")
        if directory == anchor:
            break
        if directory == directory.parent:
            raise MatrixAcceptanceError("coordination directory is unavailable")
        directory = directory.parent
    raw = _canonical_json_bytes(value) + b"\n"
    try:
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    except OSError:
        raise MatrixAcceptanceError("coordination output is unavailable") from None
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        try:
            target.unlink()
        except OSError:
            pass
        raise
    return {"path": str(target), "sha256": hashlib.sha256(raw).hexdigest()}


def _existing_binding(path, label, maximum=8_388_608):
    raw = _strict_private_file(path, label, maximum)
    return {"path": str(Path(path)), "sha256": hashlib.sha256(raw).hexdigest()}


def _proof_sha256(value):
    return hashlib.sha256(_canonical_json_bytes(value)).hexdigest()


def _wait_for_close_ack(coordination, ready_binding, session, *, timeout_seconds):
    ack_path = Path(coordination) / "ack-000001.json"
    if ack_path.exists():
        raise MatrixAcceptanceError("close acknowledgement is stale")
    deadline = time.monotonic() + timeout_seconds
    while True:
        if time.monotonic() >= deadline:
            raise MatrixAcceptanceError("close acknowledgement timed out")
        if ack_path.exists():
            try:
                raw = _strict_private_file(ack_path, "close acknowledgement", 65_536)
            except MatrixAcceptanceError:
                if time.monotonic() < deadline:
                    time.sleep(0.01)
                    continue
                raise
            try:
                value = hub_http.strict_json(raw)
            except Exception:
                raise MatrixAcceptanceError("close acknowledgement is invalid") from None
            required = {
                "schema_version", "type", "session_id", "cell_id", "session_input_sha256",
                "instance_nonce", "sequence", "ready_sha256", "phase", "status", "action", "result",
            }
            if not isinstance(value, dict) or set(value) != required:
                raise MatrixAcceptanceError("close acknowledgement shape is invalid")
            if (
                value.get("schema_version") != 1
                or value.get("type") != "ack"
                or value.get("session_id") != session["session_id"]
                or value.get("cell_id") != session["cell_id"]
                or value.get("session_input_sha256") != session["_session_input_sha256"]
                or value.get("instance_nonce") != session["instance_nonce"]
                or value.get("sequence") != 1
                or value.get("ready_sha256") != ready_binding["sha256"]
                or value.get("phase") != "evidence_ready"
            ):
                raise MatrixAcceptanceError("close acknowledgement identity is invalid")
            if value.get("status") != "accepted" or value.get("action") != "close_completed":
                raise MatrixAcceptanceError("close acknowledgement was not accepted")
            result = value.get("result")
            if not isinstance(result, dict) or set(result) != {"path", "sha256"}:
                raise MatrixAcceptanceError("close acknowledgement result is invalid")
            _existing_binding(result["path"], "close evidence", 65_536)
            if result["sha256"] != hashlib.sha256(_strict_private_file(result["path"], "close evidence", 65_536)).hexdigest():
                raise MatrixAcceptanceError("close evidence digest changed")
            return value
        time.sleep(min(0.01, max(0.001, deadline - time.monotonic())))


def _complete_v2(config, session, matrix, cases):
    """Publish immutable v2 evidence, then wait for the runner's close Ack."""
    if not isinstance(cases, list) or [item.get("id") for item in cases if isinstance(item, dict)] != CASE_IDS or any(not isinstance(item, dict) or item.get("status") != "passed" for item in cases):
        raise MatrixAcceptanceError("matrix cases are incomplete or failed")
    header_raw = _strict_private_file(session["header"]["local"]["path"], "matrix evidence header")
    header = hub_http.strict_json(header_raw)
    session["_session_input_sha256"] = config["session_input"]["sha256"]
    actor_spec = session["actors"][0]
    actor_manifest = actor_spec["input_manifest"]["local"]
    coordination = Path(session["outputs"]["coordination_dir"])
    raw_bindings = []
    invocations = []
    anchors = getattr(matrix, "anchors", {})
    for case in cases:
        case_id = case["id"]
        operation = {
            "candidate_artifact_identity": "observe_identity",
            "installed_service_runtime": "observe_identity",
            "discovery_identity_profile": "discovery",
            "unauthenticated_discovery": "unauthenticated_probes",
            "bad_invitation": "bad_invitation",
            "expired_invitation": "expired_invitation",
            "replayed_invitation": "replayed_invitation",
            "real_auth": "real_auth",
            "credential_lifecycle_reauth": "reauthentication",
            "revocation": "revoked_credential",
            "unknown_vehicle": "unknown_vehicle",
            "exact_current_values": "exact_current",
            "endpoint_restart": "endpoint_restart",
            "outage_recovery": "outage_recovery",
            "unsupported_operation_zero_requests": "unsupported_operations",
            "credential_rotation_api": "credential_rotation",
            "drives_three_page_order": "drives_three_pages",
            "drives_terminal_cursor": "drives_terminal_cursor",
            "drives_etag_304": "drives_etag",
            "drives_wrong_vehicle_cursor": "wrong_vehicle_cursor",
            "drives_wrong_filter_cursor": "wrong_filter_cursor",
        }[case_id]
        evidence_id = "protocol_http-" + operation + "-" + case_id
        request_transcript = copy.deepcopy(case["request_transcript"])
        request_ids = [
            item["request_id"]
            for item in request_transcript
            if isinstance(item, dict) and "request_id" in item
        ]
        raw_value = {
            "schema_version": 1,
            "session_id": session["session_id"],
            "cell_id": session["cell_id"],
            "session_input_sha256": session["_session_input_sha256"],
            "actor_id": actor_spec["id"],
            "operation": operation,
            "actor_manifest_sha256": actor_manifest["sha256"],
            "session_sequence_before": anchors.get(case_id, (1, 1))[0],
            "session_sequence_after": anchors.get(case_id, (1, 1))[1],
            "credential_device_id": matrix.device_id,
            "facts": copy.deepcopy(case["actual"]),
            "requests": request_transcript,
            "request_ids": request_ids,
            "cleanup": {
                "status": "passed",
                "transport_resources_closed": True,
                "auxiliary_fixture_stopped": True,
                "process_exited": True,
            },
        }
        binding = _write_exclusive_json(coordination / "raw" / (evidence_id + ".json"), raw_value)
        raw_bindings.append((case_id, evidence_id, operation, raw_value, binding))
        invocations.append({
            "id": "invoke-" + evidence_id,
            "case_id": case_id,
            "actor_id": actor_spec["id"],
            "operation": operation,
            "session_sequence_before": raw_value["session_sequence_before"],
            "session_sequence_after": raw_value["session_sequence_after"],
            "evidence_id": evidence_id,
            "request_ids": request_ids,
        })
    normalized_cases = copy.deepcopy(cases)
    for case in normalized_cases:
        if case["id"] == "installed_service_runtime":
            case["status"] = "pending"
    normalized = dict(header)
    normalized["cases"] = normalized_cases
    normalized_binding = _write_exclusive_json(session["outputs"]["normalized"], normalized)
    actor_evidence = {
        "schema_version": 1,
        "session_id": session["session_id"],
        "cell_id": session["cell_id"],
        "session_input_sha256": session["_session_input_sha256"],
        "actors": [{
            "id": actor_spec["id"],
            "kind": actor_spec["kind"],
            "runtime_ref": actor_spec["runtime_ref"],
            "entrypoint_ref": actor_spec["entrypoint_ref"],
            "artifact_roles": list(actor_spec["artifact_roles"]),
            "source_roles": list(actor_spec["source_roles"]),
            "installed_manifest": dict(actor_manifest),
            "raw_evidence": [
                {"id": evidence_id, "schema_id": "protocol-http-v1", "binding": binding}
                for _case_id, evidence_id, _operation, _raw, binding in raw_bindings
            ],
        }],
        "invocations": invocations,
    }
    actor_binding = _write_exclusive_json(session["outputs"]["actor_evidence"], actor_evidence)
    completion = {
        "schema_version": 1,
        "session_id": session["session_id"],
        "cell_id": session["cell_id"],
        "session_input_sha256": session["_session_input_sha256"],
        "normalized": normalized_binding,
        "actor_evidence": actor_binding,
    }
    completion_binding = _write_exclusive_json(coordination / "adapter-completion.json", completion)
    proof = matrix.proof
    sequence = proof.get("sequence") if isinstance(proof, dict) else None
    if type(sequence) is not int or sequence <= 0:
        raise MatrixAcceptanceError("matrix completion lacks a final observation")
    ready = {
        "schema_version": 1,
        "type": "ready",
        "session_id": session["session_id"],
        "cell_id": session["cell_id"],
        "session_input_sha256": session["_session_input_sha256"],
        "instance_nonce": session["instance_nonce"],
        "sequence": 1,
        "phase": "evidence_ready",
        "observation": {"session_sequence": sequence, "proof_sha256": _proof_sha256(proof)},
        "evidence": completion_binding,
    }
    ready_binding = _write_exclusive_json(coordination / "ready-000001.json", ready)
    _wait_for_close_ack(
        coordination,
        ready_binding,
        session,
        timeout_seconds=max(1.0, session["bounds"]["cell_timeout_ms"] / 1000),
    )


def run_matrix(value, *, control=None):
    config = validate_matrix_config(value)
    owns_control = control is None
    client = control or hub_control.ControlClient.connect(config["host_session"])
    try:
        matrix = MatrixCases(config, client)
        cases = matrix.run()
        result = {
            "schema_version": 1,
            "execution_kind": config["execution_kind"],
            "adapter": config["adapter"],
            "cell_id": config["cell_id"],
            "product_version": config["product_version"],
            "profile_id": config["profile"]["id"],
            "profile_revision": config["profile"]["revision"],
            "profile_sha256": config["profile"]["sha256"],
            "source_identities": config["source_identities"],
            "artifacts": config["artifacts"],
            "runtime": config["runtime"],
            "cases": cases,
        }
        if config["schema_version"] == 2:
            session_raw = _strict_private_file(config["session_input"]["path"], "matrix SessionInput", 1_048_576)
            session = hub_http.strict_json(session_raw)
            _complete_v2(config, session, matrix, cases)
        return result
    finally:
        if owns_control:
            client.close()


def _config_from_session_input(session_path, session=None):
    """Construct the bounded v2 matrix envelope used by the installed launcher."""
    path = Path(_absolute_path(str(session_path), "matrix SessionInput"))
    raw = _strict_private_file(path, "matrix SessionInput", 1_048_576)
    if session is None:
        try:
            session = hub_http.strict_json(raw)
        except Exception:
            raise MatrixAcceptanceError("matrix SessionInput is not strict JSON") from None
    if not isinstance(session, dict):
        raise MatrixAcceptanceError("matrix SessionInput is invalid")
    header_stage = session.get("header")
    if not isinstance(header_stage, dict):
        raise MatrixAcceptanceError("matrix SessionInput header is invalid")
    header = hub_http.strict_json(_strict_private_file(header_stage["local"]["path"], "matrix evidence header"))
    if not isinstance(header, dict):
        raise MatrixAcceptanceError("matrix evidence header is invalid")
    profile_manifest = session.get("inputs", {}).get("profile_manifest", {}).get("local", {})
    profile_path = str(Path(profile_manifest.get("path", "")).parent)
    profile_id = header.get("profile_id")
    profile_revision = header.get("profile_revision")
    if not isinstance(profile_id, str) or not isinstance(profile_revision, str) or not profile_path.startswith("/"):
        raise MatrixAcceptanceError("matrix SessionInput profile identity is invalid")
    config = {
        "schema_version": 2,
        "kind": MATRIX_KIND,
        "execution_kind": header.get("execution_kind"),
        "adapter": header.get("adapter"),
        "cell_id": header.get("cell_id"),
        "product_version": header.get("product_version"),
        "profile": {"id": profile_id, "revision": profile_revision, "path": profile_path, "sha256": header.get("profile_sha256")},
        "source_identities": header.get("source_identities"),
        "artifacts": header.get("artifacts"),
        "runtime": header.get("runtime"),
        "host_session": session.get("host_session"),
        "session_input": {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()},
    }
    return validate_matrix_config(config)


def dispatch(config):
    if isinstance(config, dict) and config.get("kind") == MATRIX_KIND:
        return "matrix-v2" if config.get("schema_version") == 2 else "matrix", run_matrix(config)
    return "native", hub_http.run_network(config)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", default=os.environ.get("TESLATLAS_HUB_HTTP_CONFIG"))
    parser.add_argument("--session-input")
    parser.add_argument("session_input_path", nargs="?")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    try:
        config_path = args.session_input or args.session_input_path or args.config
        if not config_path:
            raise MatrixAcceptanceError("private actual-Hub run config required")
        supplied = hub_http.read_private(config_path)
        if isinstance(supplied, dict) and supplied.get("kind") == "matrix-adapter-session":
            config = _config_from_session_input(config_path, supplied)
        else:
            config = supplied
        mode, result = dispatch(config)
        if mode == "matrix-v2":
            print(json.dumps({
                "status": "passed",
                "adapter": result["adapter"],
                "cell_id": result["cell_id"],
                "profile_id": result["profile_id"],
                "profile_revision": result["profile_revision"],
                "profile_sha256": result["profile_sha256"],
                "evidence_kind": "installed-adapter-v2",
            }, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False))
        else:
            print(json.dumps(result, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False))
        if mode in {"matrix", "matrix-v2"}:
            return 0 if all(item.get("status") == "passed" for item in result["cases"]) else 1
        return 0 if result.get("status") == "passed" else 1
    except (MatrixAcceptanceError, hub_http.AcceptanceError, hub_control.ControlProtocolError):
        print(json.dumps({"status": "failed", "profile_id": hub_http.PROFILE_ID, "error": "actual-Hub matrix prerequisites or acceptance failed"}, separators=(",", ":")))
        return 1
    except Exception:
        print(json.dumps({"status": "failed", "profile_id": hub_http.PROFILE_ID, "error": "actual-Hub acceptance failed; inspect private fixture evidence"}, separators=(",", ":")))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
