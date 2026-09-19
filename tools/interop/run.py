#!/usr/bin/env python3
"""Run the mandatory Teslatlas Hub ecosystem compatibility matrix.

The private config is an owner-only JSON file. Its schema is documented in
``matrix_runner/CONFIG.md``. Commands are always passed to ``Popen`` as argv;
the runner never invokes a shell. A required matrix row is successful only
when the freshly executed adapter returns every independently named case and
all source, artifact, profile, runtime, and expected/actual bindings match.
"""

import argparse
import hashlib
import importlib
import json
import math
import os
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys
import tarfile
import time
from datetime import datetime, timezone

if __package__:
    from .matrix_runner import source_evidence as _source_evidence
    from .installed_hosts.contract import read_registered_config as _read_registered_config
    _MATRIX_RUNNER_PACKAGE = f"{__package__}.matrix_runner"
else:
    _INTEROP_ROOT = str(Path(__file__).resolve().parent)
    if _INTEROP_ROOT not in sys.path:
        sys.path.insert(0, _INTEROP_ROOT)
    from matrix_runner import source_evidence as _source_evidence
    from installed_hosts.contract import read_registered_config as _read_registered_config
    _MATRIX_RUNNER_PACKAGE = "matrix_runner"


MAX_CONFIG_BYTES = 1024 * 1024
MAX_EVIDENCE_BYTES = 8 * 1024 * 1024
HEX64 = re.compile(r"^[0-9a-f]{64}$")
CONFIG_FIELDS = {
    "schema_version",
    "execution_kind",
    "matrix_sha256",
    "product_version",
    "profile",
    "jobs",
}
CONFIG_V2_FIELDS = CONFIG_FIELDS | {"cohort_inputs"}
JOB_FIELDS = {
    "cell_id",
    "adapter",
    "argv",
    "cwd",
    "environment_file",
    "evidence_path",
    "evidence_mode",
    "timeout_seconds",
    "max_output_bytes",
    "command_files",
    "source_identities",
    "artifacts",
    "runtime",
}
INSTALLED_JOB_FIELDS = JOB_FIELDS | {"installed_session", "client_execution"}
WORKSPACE_ROOT = Path(__file__).resolve().parents[3]
ACTUAL_SOURCE_ROOTS = {
    "hub_source": WORKSPACE_ROOT / "hub",
    "protocol_source": WORKSPACE_ROOT / "teslatlas-protocol",
    "typescript_sdk_source": WORKSPACE_ROOT / "teslatlas-sdk-typescript",
    "swift_sdk_source": WORKSPACE_ROOT / "teslatlas-sdk-swift",
    "home_assistant_source": WORKSPACE_ROOT / "teslatlas-home-assistant",
    "edge_source": WORKSPACE_ROOT / "teslatlas-edge",
    "app_source": WORKSPACE_ROOT / "app",
}
SOURCE_FIELDS = {
    "role",
    "repo",
    "head",
    "dirty_patch_sha256",
    "untracked_source_manifest_sha256",
}
ARTIFACT_FIELDS = {"role", "name", "path", "embedded_version", "sha256"}
RUNTIME_FIELDS = {"hub", "client", "browser_engines", "client_transports"}
RUNTIME_HOST_FIELDS = {
    "os", "architecture", "native_or_emulated", "service_mode", "tool_versions"
}
PROFILE_FIELDS = {"id", "revision", "path", "sha256"}
NORMALIZED_EVIDENCE_FIELDS = {
    "schema_version",
    "execution_kind",
    "adapter",
    "cell_id",
    "product_version",
    "profile_id",
    "profile_revision",
    "profile_sha256",
    "source_identities",
    "artifacts",
    "runtime",
    "cases",
}
CASE_FIELDS = {
    "id",
    "status",
    "expected",
    "actual",
    "evidence_kind",
    "request_transcript",
    "process_evidence",
    "not_applicable_reason",
}
REQUEST_FIELDS = {"method", "route", "status", "request_id"}
FORBIDDEN_LAUNCHERS = {"sh", "bash", "zsh", "fish", "dash", "echo", "env"}
ACTUAL_KINDS = {"actual_hub_acceptance", "interim_actual_protocol_smoke"}
SOURCE_ROLES = {
    "protocol_actual_hub": {"hub_source", "protocol_source"},
    "typescript_node": {"hub_source", "protocol_source", "typescript_sdk_source"},
    "typescript_browser": {"hub_source", "protocol_source", "typescript_sdk_source"},
    "swift": {"hub_source", "protocol_source", "swift_sdk_source"},
    "home_assistant": {"hub_source", "protocol_source", "home_assistant_source"},
    "edge_v2": {"hub_source", "protocol_source", "edge_source"},
}
ARTIFACT_ROLES = {
    "protocol_actual_hub": {"hub_executable", "protocol_fixture_seed"},
    "typescript_node": {"hub_executable", "typescript_sdk_tarball"},
    "typescript_browser": {"hub_executable", "typescript_sdk_tarball"},
    "swift": {"hub_executable", "swift_sdk_product"},
    "home_assistant": {"hub_executable", "home_assistant_integration_archive"},
    "edge_v2": {"hub_executable", "edge_executable"},
}
ZERO_REQUEST_CASES = {
    "expired_invitation", "drives_wrong_vehicle_cursor",
    "drives_wrong_filter_cursor", "unsupported_operation_zero_requests",
}
OPTIONAL_PREFLIGHT_CASES = {"bad_invitation"}
IDENTITY_CASES = {
    "candidate_artifact_identity", "installed_service_runtime",
    "edge_linux_runtime", "edge_source_owner_exclusivity",
    "installed_home_assistant_runtime",
}
ASSERTION_KEYS = {
    "candidate_artifact_identity": {"hub_sha256", "tarball_sha256", "package_version", "installed_members"},
    "installed_service_runtime": {"service_mode"},
    "discovery_identity_profile": {"hub_id", "api_versions", "protocol", "protocol_major", "pack_format", "version"},
    "unauthenticated_discovery": {"discovery", "health", "readiness", "credential_absent"},
    "expired_invitation": {"outgoing_requests", "typed_error"},
    "bad_invitation": {"typed_error", "http_status"},
    "replayed_invitation": {"typed_error", "http_status"},
    "unknown_vehicle": {"typed_error", "http_status"},
    "revocation": {"typed_error", "http_status"},
    "real_auth": {"claimed", "vehicles"},
    "credential_lifecycle_reauth": {"new_device", "vehicles"},
    "exact_current_values": {"battery_level", "inside_temp", "outside_temp", "observed_at_ms", "est_battery_range_km", "odometer", "speed", "scheduled_charging_start_time", "active_route_miles_to_arrival", "empty_vehicle_observed_at_ms"},
    "endpoint_restart": {"same_hub", "new_process", "vehicles"},
    "outage_recovery": {"outage_observed", "vehicles"},
    "unsupported_operation_zero_requests": {"outgoing_requests"},
    "credential_rotation_api": {"rotated", "same_device", "vehicles", "old_credential_error", "old_credential_status"},
    "drives_three_page_order": {"pages"},
    "drives_terminal_cursor": {"next_cursor", "ids"},
    "drives_etag_304": {"kind", "post_304_ids"},
    "drives_wrong_vehicle_cursor": {"outgoing_requests", "typed_error"},
    "drives_wrong_filter_cursor": {"outgoing_requests", "typed_error"},
    "real_browser_cors": {"preflight_succeeded", "cross_origin"},
    "browser_normal_tls_validation": {"trusted_succeeded", "untrusted_error"},
    "native_macos_transport": {"os", "transport", "trusted_tls"},
    "native_linux_transport": {"os", "transport", "trusted_tls"},
    "transport_cancellation": {"cancelled", "typed_error", "bounded"},
    "transport_body_limit": {"limit_bytes", "oversize_rejected", "typed_error"},
    "credential_loss_reauthentication": {"credential_loss_observed", "reauth_started", "reauth_completed"},
    "installed_home_assistant_runtime": {"home_assistant_version", "integration_version", "iot_class"},
    "polling_transport_zero_sse": {"polling_requests", "sse_requests", "clean_unload"},
    "edge_linux_runtime": {"os", "architecture", "native_or_emulated"},
    "edge_mtls_bearer_identity": {"mtls_verified", "bearer_identity_verified"},
    "edge_no_v1_downgrade": {"v2_attempted", "v1_requests"},
    "edge_source_owner_exclusivity": {"selected_source", "active_ingestion_authorities"},
    "edge_uninterrupted_delivery_parity": {"public_current_equal", "history_equal"},
    "edge_restart_delivery_parity": {"public_current_equal", "history_equal", "restart_observed"},
    "edge_duplicate_reenqueue_dedup": {"duplicate_reenqueued", "duplicate_rows_added"},
    "edge_gap_interleaving_later_ack_rejected": {"later_ack_rejected", "contiguous_prefix_preserved"},
    "edge_unsupported_event_bounded_disposition": {"unsupported_event_disposition", "bounded"},
    "edge_conflicting_identity_blocks_ack": {"conflict_observed", "ack_blocked"},
    "edge_transaction_fault_redelivery": {"fault_injected", "ack_absent", "redelivered"},
    "edge_lost_ack_body_redelivery": {"ack_committed", "body_lost", "redelivered_without_duplicate"},
    "edge_pending_publication_offline_recovery": {"offline_commit_durable", "publication_recovered"},
    "edge_clean_shutdown_resume": {"clean_shutdown", "resume_contiguous"},
}
SUPPORTED_ACTUAL_CASE_ADAPTERS = {"typescript_node", "typescript_browser"}
SCENARIO_VEHICLES = [
    {"vehicle_id": "11111111-1111-4111-8111-111111111111", "display_name": "Interop – Árvíztűrő 🚗"},
    {"vehicle_id": "22222222-2222-4222-8222-222222222222", "display_name": "Interop empty"},
]
SCENARIO_CURRENT = {
    "battery_level": 0, "inside_temp": 21.5, "outside_temp": None,
    "observed_at_ms": 1788566400000, "est_battery_range_km": 160.93,
    "odometer": 16093.44, "speed": 16,
    "scheduled_charging_start_time": 1788570000,
    "active_route_miles_to_arrival": 12.5, "empty_vehicle_observed_at_ms": None,
}
EXACT_CASE_EXPECTED = {
    "unauthenticated_discovery": {"discovery": 200, "health": 200, "readiness": 200, "credential_absent": True},
    "expired_invitation": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
    "bad_invitation": {"typed_error": "hub_http_error", "http_status": 401},
    "real_auth": {"claimed": 200, "vehicles": SCENARIO_VEHICLES},
    "replayed_invitation": {"typed_error": "hub_http_error", "http_status": 401},
    "unknown_vehicle": {"typed_error": "hub_http_error", "http_status": 404},
    "exact_current_values": SCENARIO_CURRENT,
    "drives_three_page_order": {"pages": [[105, 104], [103, 102], [101]]},
    "drives_terminal_cursor": {"next_cursor": None, "ids": [101]},
    "drives_etag_304": {"kind": "notModified", "post_304_ids": [103, 102]},
    "drives_wrong_vehicle_cursor": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
    "drives_wrong_filter_cursor": {"outgoing_requests": 0, "typed_error": "protocol_validation"},
    "unsupported_operation_zero_requests": {"outgoing_requests": 0},
    "credential_rotation_api": {"rotated": True, "same_device": True, "vehicles": 200, "old_credential_error": "hub_http_error", "old_credential_status": 401},
    "revocation": {"typed_error": "hub_http_error", "http_status": 401},
    "credential_lifecycle_reauth": {"new_device": True, "vehicles": 200},
    "endpoint_restart": {"same_hub": True, "new_process": True, "vehicles": 200},
    "outage_recovery": {"outage_observed": True, "vehicles": 200},
    "real_browser_cors": {"preflight_succeeded": True, "cross_origin": True},
    "browser_normal_tls_validation": {"trusted_succeeded": True, "untrusted_error": "ERR_CERT_AUTHORITY_INVALID"},
}
HTTP_RULES = {
    "discovery_identity_profile": ("GET", re.compile(r"^/\.well-known/")),
    "unauthenticated_discovery": ("GET", re.compile(r"^/(\.well-known/|healthz$|readyz$)")),
    "bad_invitation": ("POST", re.compile(r"^/v1/pairings/[^/]+/claim$")),
    "replayed_invitation": ("POST", re.compile(r"^/v1/pairings/[^/]+/claim$")),
    "real_auth": (None, re.compile(r"^/v1/(pairings/[^/]+/claim|vehicles)$")),
    "unknown_vehicle": ("GET", re.compile(r"^/v1/vehicles/[^/]+/current$")),
    "exact_current_values": ("GET", re.compile(r"^/v1/vehicles/[^/]+/current$")),
    "credential_rotation_api": (None, re.compile(r"^/v1/(device/rotate|vehicles)$")),
    "drives_three_page_order": ("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$")),
    "drives_terminal_cursor": ("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$")),
    "drives_etag_304": ("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$")),
    "revocation": ("GET", re.compile(r"^/v1/vehicles$")),
    "credential_lifecycle_reauth": (None, re.compile(r"^/v1/(pairings/[^/]+/claim|vehicles)$")),
    "endpoint_restart": (None, re.compile(r"^/(\.well-known/|v1/vehicles$)")),
    "outage_recovery": ("GET", re.compile(r"^/v1/vehicles$")),
    "real_browser_cors": ("OPTIONS", re.compile(r"^/v1/")),
    "browser_normal_tls_validation": ("GET", re.compile(r"^/")),
    "polling_transport_zero_sse": ("GET", re.compile(r"^/v1/")),
}
HTTP_STATUS_RULES = {
    "discovery_identity_profile": [("GET", re.compile(r"^/\.well-known/"), {200})],
    "unauthenticated_discovery": [
        ("GET", re.compile(r"^/\.well-known/"), {200}),
        ("GET", re.compile(r"^/healthz$"), {200}), ("GET", re.compile(r"^/readyz$"), {200}),
    ],
    "bad_invitation": [("POST", re.compile(r"^/v1/pairings/[^/]+/claim$"), {401})],
    "replayed_invitation": [("POST", re.compile(r"^/v1/pairings/[^/]+/claim$"), {401})],
    "real_auth": [
        ("POST", re.compile(r"^/v1/pairings/[^/]+/claim$"), {200}),
        ("GET", re.compile(r"^/v1/vehicles$"), {200}),
    ],
    "unknown_vehicle": [("GET", re.compile(r"^/v1/vehicles/[^/]+/current$"), {404})],
    "exact_current_values": [("GET", re.compile(r"^/v1/vehicles/[^/]+/current$"), {200})],
    "credential_rotation_api": [
        ("POST", re.compile(r"^/v1/device/rotate$"), {200}),
        ("GET", re.compile(r"^/v1/vehicles$"), {200}),
        ("GET", re.compile(r"^/v1/vehicles$"), {401}),
    ],
    "drives_three_page_order": [("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$"), {200})],
    "drives_terminal_cursor": [("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$"), {200})],
    "drives_etag_304": [
        ("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$"), {304}),
        ("GET", re.compile(r"^/v1/vehicles/[^/]+/drives$"), {200}),
    ],
    "revocation": [("GET", re.compile(r"^/v1/vehicles$"), {401})],
    "credential_lifecycle_reauth": [
        ("POST", re.compile(r"^/v1/pairings/[^/]+/claim$"), {200}),
        ("GET", re.compile(r"^/v1/vehicles$"), {200}),
    ],
    "endpoint_restart": [("GET", re.compile(r"^/v1/vehicles$"), {200})],
    "outage_recovery": [("GET", re.compile(r"^/v1/vehicles$"), {200})],
    "real_browser_cors": [("OPTIONS", re.compile(r"^/v1/"), set(range(200, 300)))],
    "browser_normal_tls_validation": [("GET", re.compile(r"^/"), {200})],
}
REQUIRED_TRUE_FACTS = {
    "native_macos_transport": {"trusted_tls"}, "native_linux_transport": {"trusted_tls"},
    "transport_cancellation": {"cancelled", "bounded"},
    "transport_body_limit": {"oversize_rejected"},
    "credential_loss_reauthentication": {"credential_loss_observed", "reauth_started", "reauth_completed"},
    "polling_transport_zero_sse": {"clean_unload"},
    "edge_mtls_bearer_identity": {"mtls_verified", "bearer_identity_verified"},
    "edge_no_v1_downgrade": {"v2_attempted"},
    "edge_uninterrupted_delivery_parity": {"public_current_equal", "history_equal"},
    "edge_restart_delivery_parity": {"public_current_equal", "history_equal", "restart_observed"},
    "edge_duplicate_reenqueue_dedup": {"duplicate_reenqueued"},
    "edge_gap_interleaving_later_ack_rejected": {"later_ack_rejected", "contiguous_prefix_preserved"},
    "edge_unsupported_event_bounded_disposition": {"bounded"},
    "edge_conflicting_identity_blocks_ack": {"conflict_observed", "ack_blocked"},
    "edge_transaction_fault_redelivery": {"fault_injected", "ack_absent", "redelivered"},
    "edge_lost_ack_body_redelivery": {"ack_committed", "body_lost", "redelivered_without_duplicate"},
    "edge_pending_publication_offline_recovery": {"offline_commit_durable", "publication_recovered"},
    "edge_clean_shutdown_resume": {"clean_shutdown", "resume_contiguous"},
}
REQUIRED_ZERO_FACTS = {
    "polling_transport_zero_sse": {"sse_requests"},
    "edge_no_v1_downgrade": {"v1_requests"},
    "edge_duplicate_reenqueue_dedup": {"duplicate_rows_added"},
}


class MatrixError(Exception):
    """Uses fixed diagnostic labels so private values never reach public output."""


class PendingCapability(MatrixError):
    """A truthful unsupported integration boundary; no child may be started."""


def utc_now():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _typed_equal(left, right):
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return set(left) == set(right) and all(_typed_equal(left[key], right[key]) for key in left)
    if isinstance(left, list):
        return len(left) == len(right) and all(_typed_equal(a, b) for a, b in zip(left, right))
    return left == right


def _normalize_arch(value):
    if not isinstance(value, str):
        return None
    return {"arm64": "arm64", "aarch64": "arm64", "x64": "amd64", "x86_64": "amd64", "amd64": "amd64"}.get(value)


def _local_host_identity():
    import platform
    if sys.platform == "darwin":
        os_name = "macOS"
    elif sys.platform.startswith("linux"):
        release = platform.freedesktop_os_release()
        os_name = "Debian 13" if release.get("ID") == "debian" and release.get("VERSION_ID") == "13" else release.get("PRETTY_NAME", "Linux")
    else:
        os_name = sys.platform
    return os_name, _normalize_arch(platform.machine())


def _require_local_hub_runtime(runtime):
    local_os, local_arch = _local_host_identity()
    if (runtime.get("os"), runtime.get("architecture")) != (local_os, local_arch):
        raise PendingCapability("pending: foreign-host artifact verifier is unavailable")


def _require_cell_adapter(job, cell):
    if job.get("adapter") != cell.get("adapter"):
        raise MatrixError("job adapter does not match matrix cell")


def _node_launcher_identity(job, client, descriptor):
    executable = Path(job["argv"][0])
    expected_hash = client.get("actual_node_sha256")
    expected_version = client.get("actual_node_version")
    descriptor_hash = (
        expected_hash if descriptor.get("schema_version") == 2
        and descriptor.get("kind") == "installed-client-lane"
        else descriptor.get("node_sha256")
    )
    if (executable.name != "node" or not isinstance(expected_hash, str)
            or not HEX64.fullmatch(expected_hash)
            or descriptor_hash != expected_hash
            or job["command_files"][0]["sha256"] != expected_hash
            or sha256_file(executable) != expected_hash):
        raise MatrixError("TypeScript Node launcher identity mismatch")
    try:
        result = subprocess.run(
            [str(executable), "--version"], stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
            timeout=10, check=True,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise MatrixError("TypeScript Node launcher probe failed") from error
    version = result.stdout.strip()
    if version != expected_version:
        raise MatrixError("TypeScript Node launcher version mismatch")
    local_os, local_arch = _local_host_identity()
    return {"kind": "node", "path": str(executable), "sha256": expected_hash,
            "version": version, "os": local_os, "architecture": local_arch}


def strict_json(raw):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise MatrixError("duplicate JSON member")
            value[key] = item
        return value

    def reject(_value):
        raise MatrixError("non-finite JSON number")

    def finite(value):
        parsed = float(value)
        if not math.isfinite(parsed):
            raise MatrixError("non-finite JSON number")
        return parsed

    try:
        return json.loads(
            raw,
            object_pairs_hook=unique,
            parse_constant=reject,
            parse_float=finite,
        )
    except (UnicodeError, json.JSONDecodeError) as error:
        raise MatrixError("invalid JSON") from error


def _read_regular(path, limit, private=False):
    path = Path(path)
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    try:
        fd = os.open(path, flags)
    except OSError as error:
        raise MatrixError("file cannot be safely opened") from error
    with os.fdopen(fd, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        if not stat.S_ISREG(metadata.st_mode):
            raise MatrixError("file must be regular")
        if private and (metadata.st_uid != os.getuid() or metadata.st_mode & 0o077):
            raise MatrixError("private file permissions invalid")
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise MatrixError("file exceeds byte limit")
    return raw


def read_json(path, limit=MAX_CONFIG_BYTES, private=False):
    return strict_json(_read_regular(path, limit, private=private))


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        while True:
            block = stream.read(1024 * 1024)
            if not block:
                break
            digest.update(block)
    return digest.hexdigest()


def write_private_json(path, value):
    path = Path(path)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True, ensure_ascii=False)
        stream.write("\n")


def _resolved(path):
    """Resolve existing ancestors too, so symlink aliases cannot escape checks."""
    path = Path(path)
    if not path.is_absolute():
        raise MatrixError("private path must be absolute")
    missing = []
    cursor = path
    while not cursor.exists():
        missing.append(cursor.name)
        parent = cursor.parent
        if parent == cursor:
            raise MatrixError("private path parent is missing")
        cursor = parent
    resolved = cursor.resolve(strict=True)
    for member in reversed(missing):
        resolved /= member
    return resolved


def _is_within(path, root):
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _validate_private_output(path, *, must_exist=False):
    path = Path(path)
    resolved = _resolved(path)
    if _is_within(resolved, WORKSPACE_ROOT.resolve()):
        raise MatrixError("private output must be outside workspace sources")
    parent = resolved.parent
    if not parent.is_dir():
        raise MatrixError("private output parent is missing")
    metadata = parent.stat()
    if metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
        raise MatrixError("private output parent permissions invalid")
    if must_exist:
        _read_regular(resolved, MAX_CONFIG_BYTES, private=True)
    return resolved


def _safe_command(adapter):
    return [adapter, "fixed-reviewed-entrypoint"]


def _validate_config_schema(config):
    try:
        from jsonschema import Draft202012Validator
    except ImportError as error:
        raise MatrixError("jsonschema dependency is unavailable") from error
    schema = read_json(Path(__file__).with_name("matrix_runner") / "config.schema.json")
    if next(Draft202012Validator(schema).iter_errors(config), None) is not None:
        raise MatrixError("config violates schema")


def _git(repo, *args, binary=False):
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        timeout=30,
    )
    if result.returncode:
        raise MatrixError("source identity observation failed")
    return result.stdout if binary else result.stdout.decode("utf-8").strip()


def observe_source_identity(repo, role=None):
    """Return a content-bound identity without mutating the checkout."""
    repo = Path(repo)
    if not repo.is_absolute() or not repo.is_dir():
        raise MatrixError("source repo must be an absolute directory")
    head = _git(repo, "rev-parse", "HEAD")
    patch = _git(repo, "diff", "--binary", "HEAD", "--", binary=True)
    untracked = _git(
        repo, "ls-files", "--others", "--exclude-standard", "-z", binary=True
    ).split(b"\0")
    manifest = hashlib.sha256()
    for encoded in sorted(item for item in untracked if item):
        try:
            relative = encoded.decode("utf-8")
        except UnicodeError as error:
            raise MatrixError("untracked source path is not UTF-8") from error
        path = repo / relative
        if path.is_symlink():
            kind = b"symlink"
            content = os.readlink(path).encode("utf-8")
        elif path.is_file():
            kind = b"file"
            content = path.read_bytes()
        else:
            raise MatrixError("untracked source member is unsupported")
        manifest.update(encoded + b"\0" + kind + b"\0")
        manifest.update(hashlib.sha256(content).hexdigest().encode("ascii") + b"\0")
    value = {
        "repo": str(repo),
        "head": head,
        "dirty_patch_sha256": hashlib.sha256(patch).hexdigest(),
        "untracked_source_manifest_sha256": manifest.hexdigest(),
    }
    if role is not None:
        value["role"] = role
    return value


def _require_exact_fields(value, fields, label):
    if not isinstance(value, dict):
        raise MatrixError(label + " must be an object")
    missing = fields - set(value)
    unknown = set(value) - fields
    if missing:
        raise MatrixError("missing " + label + " fields")
    if unknown:
        raise MatrixError("unknown " + label + " fields")


def _require_absolute(value, label, directory=False):
    if not isinstance(value, str) or not Path(value).is_absolute():
        raise MatrixError(label + " must be absolute")
    if directory and not Path(value).is_dir():
        raise MatrixError(label + " directory is missing")


def _validate_digest(value, label):
    if not isinstance(value, str) or not HEX64.fullmatch(value):
        raise MatrixError(label + " digest is invalid")


def _validate_source(value):
    _require_exact_fields(value, SOURCE_FIELDS, "source identity")
    _require_absolute(value["repo"], "source repo", directory=True)
    for name in ("dirty_patch_sha256", "untracked_source_manifest_sha256"):
        _validate_digest(value[name], name)
    if not isinstance(value["head"], str) or not re.fullmatch(r"[0-9a-f]{40,64}", value["head"]):
        raise MatrixError("source head is invalid")
    if not isinstance(value["role"], str) or not value["role"]:
        raise MatrixError("source role is invalid")


def _validate_artifact(value):
    _require_exact_fields(value, ARTIFACT_FIELDS, "artifact identity")
    _require_absolute(value["path"], "artifact path")
    if not Path(value["path"]).is_file():
        raise MatrixError("artifact is missing")
    if not all(isinstance(value[key], str) and value[key] for key in ("role", "name", "embedded_version")):
        raise MatrixError("artifact name or version is invalid")
    _validate_digest(value["sha256"], "artifact")


def _verify_artifact_version(artifact, product_version, actual, *, probe_executable=True):
    role = artifact["role"]
    path = Path(artifact["path"])
    if not actual:
        if role != "deterministic_fixture":
            raise MatrixError("deterministic artifact role is invalid")
        return
    executable_labels = {
        "hub_executable": "teslatlas-hub",
        "edge_executable": "teslatlas-edge",
    }
    if role in executable_labels:
        if not probe_executable:
            if artifact["embedded_version"] != product_version:
                raise MatrixError("executable artifact embedded version mismatch")
            return
        try:
            observed = subprocess.run(
                [str(path), "--version"], stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                text=True, timeout=10, check=True,
            ).stdout.strip()
        except (OSError, subprocess.SubprocessError) as error:
            raise MatrixError("Hub artifact version probe failed") from error
        if observed != executable_labels[role] + " " + product_version or artifact["embedded_version"] != product_version:
            raise MatrixError("executable artifact embedded version mismatch")
    elif role == "typescript_sdk_tarball":
        try:
            with tarfile.open(path, "r:gz") as archive:
                member = archive.getmember("package/package.json")
                stream = archive.extractfile(member)
                if stream is None or member.size > MAX_CONFIG_BYTES:
                    raise MatrixError("SDK artifact manifest is invalid")
                package = strict_json(stream.read(MAX_CONFIG_BYTES + 1))
        except (OSError, tarfile.TarError, KeyError) as error:
            raise MatrixError("SDK artifact manifest is invalid") from error
        if (not isinstance(package, dict) or package.get("name") != "@teslatlas/sdk"
                or package.get("version") != product_version
                or artifact["embedded_version"] != product_version):
            raise MatrixError("package artifact embedded version mismatch")
    elif role == "home_assistant_integration_archive":
        try:
            with tarfile.open(path, "r:gz") as archive:
                names = {
                    "custom_components/teslatlas_hub/manifest.json",
                }
                member = next(item for item in archive.getmembers() if item.name in names)
                if member.size > MAX_CONFIG_BYTES:
                    raise MatrixError("Home Assistant artifact manifest is invalid")
                stream = archive.extractfile(member)
                manifest = strict_json(stream.read(MAX_CONFIG_BYTES + 1)) if stream else None
        except (OSError, tarfile.TarError, StopIteration) as error:
            raise MatrixError("Home Assistant artifact manifest is invalid") from error
        if (not isinstance(manifest, dict) or manifest.get("domain") != "teslatlas_hub"
                or manifest.get("version") != product_version or artifact["embedded_version"] != product_version):
            raise MatrixError("Home Assistant artifact embedded version mismatch")
    elif role == "protocol_fixture_seed":
        if artifact["name"] != "interop_fixture" or artifact["embedded_version"] != "tooling":
            raise MatrixError("protocol fixture tooling identity mismatch")
    elif role == "swift_sdk_product":
        # A Swift matrix row is only meaningful when the selected product is
        # an immutable source/product archive.  Do not infer its version from
        # the filename or accept a loose build directory: the fixed Swift
        # launcher later binds its installed member inventory to this archive.
        required = {
            "Package.swift", "VERSION", "tools/matrix-contract.json",
            "tools/matrix_contract.py", "tools/matrix_live.py", "tools/matrix_wire.py",
        }
        try:
            with tarfile.open(path, "r:gz") as archive:
                members = archive.getmembers()
                files = {}
                for member in members:
                    if member.isdir():
                        continue
                    if not member.isfile() or member.name.startswith("/") or ".." in Path(member.name).parts:
                        raise MatrixError("Swift product archive member is invalid")
                    prefix = "teslatlas-sdk-swift/"
                    if not member.name.startswith(prefix) or member.name == prefix:
                        raise MatrixError("Swift product archive root is invalid")
                    relative = member.name[len(prefix):]
                    if relative in files or member.size > MAX_CONFIG_BYTES:
                        raise MatrixError("Swift product archive member inventory is invalid")
                    stream = archive.extractfile(member)
                    if stream is None:
                        raise MatrixError("Swift product archive member is unavailable")
                    files[relative] = stream.read(MAX_CONFIG_BYTES + 1)
                    if len(files[relative]) > MAX_CONFIG_BYTES:
                        raise MatrixError("Swift product archive member exceeds bound")
        except (OSError, tarfile.TarError) as error:
            raise MatrixError("Swift product archive is invalid") from error
        if (not required.issubset(files)
                or b'name: "teslatlas-sdk-swift"' not in files["Package.swift"]):
            raise MatrixError("Swift product archive is incomplete")
        try:
            version = files["VERSION"].decode("ascii", "strict").strip()
        except UnicodeDecodeError as error:
            raise MatrixError("Swift product version is invalid") from error
        if version != product_version or artifact["embedded_version"] != product_version:
            raise MatrixError("Swift product embedded version mismatch")
    else:
        raise MatrixError("unsupported actual artifact role")


def _validate_runtime_host(value):
    _require_exact_fields(value, RUNTIME_HOST_FIELDS, "runtime host identity")
    for key in ("os", "architecture", "native_or_emulated", "service_mode"):
        if not isinstance(value[key], str) or not value[key]:
            raise MatrixError("runtime identity value is invalid")
    if value["native_or_emulated"] not in ("native", "emulated"):
        raise MatrixError("runtime native-or-emulated value is invalid")
    if not isinstance(value["tool_versions"], dict) or not value["tool_versions"]:
        raise MatrixError("runtime tool versions are missing")
    if not all(isinstance(k, str) and isinstance(v, str) and v for k, v in value["tool_versions"].items()):
        raise MatrixError("runtime tool version is invalid")


def _validate_runtime(value):
    _require_exact_fields(value, RUNTIME_FIELDS, "runtime identity")
    _validate_runtime_host(value["hub"])
    _validate_runtime_host(value["client"])
    for key in ("browser_engines", "client_transports"):
        if not isinstance(value[key], list) or not all(isinstance(item, str) and item for item in value[key]):
            raise MatrixError("runtime list is invalid")


def _validate_profile(profile, matrix, execution_kind):
    _require_exact_fields(profile, PROFILE_FIELDS, "profile")
    _require_absolute(profile["path"], "profile path", directory=True)
    if profile["id"] != matrix["profile"]["id"] or profile["revision"] != matrix["profile"]["revision"]:
        raise MatrixError("profile identity mismatch")
    _validate_digest(profile["sha256"], "profile")
    if execution_kind in ("actual_hub_acceptance", "interim_actual_protocol_smoke") and profile["sha256"] != matrix["profile"]["manifest_sha256"]:
        raise MatrixError("profile digest does not match matrix")
    manifest = Path(profile["path"]) / "SHA256SUMS"
    if sha256_file(manifest) != profile["sha256"]:
        raise MatrixError("profile digest mismatch")
    listed = set()
    for line in manifest.read_text(encoding="ascii").splitlines():
        try:
            digest, name = line.split("  ", 1)
        except ValueError as error:
            raise MatrixError("profile manifest is malformed") from error
        member = Path(name)
        if not HEX64.fullmatch(digest) or member.is_absolute() or ".." in member.parts or name in listed:
            raise MatrixError("profile manifest is malformed")
        listed.add(name)
        if sha256_file(Path(profile["path"]) / member) != digest:
            raise MatrixError("profile member digest mismatch")
    profile_json = read_json(Path(profile["path"]) / "profile.json", private=False)
    if profile_json.get("profile_id") != f"{profile['id']}@{profile['revision']}":
        raise MatrixError("profile document identity mismatch")


def load_matrix():
    path = Path(__file__).resolve().parents[2] / "docs/compatibility/matrix.json"
    matrix = read_json(path, private=False)
    required = {"schema_version", "matrix_id", "product_version", "profile", "case_contract", "clients", "hub_targets", "cells", "cohort_requirements"}
    _require_exact_fields(matrix, required, "matrix")
    if matrix["schema_version"] != 1 or matrix["product_version"] != "2026.36.2":
        raise MatrixError("unsupported matrix identity")
    if not isinstance(matrix["cells"], list) or len(matrix["cells"]) != 18:
        raise MatrixError("matrix must contain exactly 18 cells")
    ids = [cell.get("id") for cell in matrix["cells"] if isinstance(cell, dict)]
    if len(ids) != 18 or len(set(ids)) != 18:
        raise MatrixError("matrix cell identity is invalid")
    for cell in matrix["cells"]:
        if set(cell) != {"id", "client_id", "adapter", "hub_target", "required"}:
            raise MatrixError("matrix cell shape is invalid")
        if cell["required"] is not True or cell["client_id"] not in matrix["clients"] or cell["hub_target"] not in matrix["hub_targets"]:
            raise MatrixError("matrix cell binding is invalid")
        client = matrix["clients"][cell["client_id"]]
        if cell["adapter"] != client["adapter"] or not client.get("required_cases"):
            raise MatrixError("matrix client binding is invalid")
        if len(client["required_cases"]) != len(set(client["required_cases"])):
            raise MatrixError("matrix client cases are duplicated")
    return matrix, path


def _bound_private_json(binding, label):
    if not isinstance(binding, dict) or set(binding) != {"path", "sha256"}:
        raise MatrixError(label + " binding is invalid")
    _require_absolute(binding["path"], label)
    _validate_digest(binding["sha256"], label)
    path = _validate_private_output(binding["path"], must_exist=True)
    raw = _read_regular(path, MAX_CONFIG_BYTES, private=True)
    if hashlib.sha256(raw).hexdigest() != binding["sha256"]:
        raise MatrixError(label + " digest mismatch")
    value = strict_json(raw)
    if not isinstance(value, dict):
        raise MatrixError(label + " must contain an object")
    return value


def _validate_installed_job(job, cell, matrix):
    installed = job.get("installed_session")
    if not isinstance(installed, dict) or set(installed) != {"config", "registration_inventory"}:
        raise MatrixError("installed session binding is invalid")
    session_config = _bound_private_json(installed["config"], "installed session config")
    inventory = _bound_private_json(installed["registration_inventory"], "registration inventory")
    try:
        registered = _read_registered_config(session_config, inventory)
    except Exception as error:
        raise MatrixError("installed registration is invalid") from error
    target = matrix["hub_targets"][cell["hub_target"]]
    expected = registered.config["expected"]
    if (
        registered.config["cell_id"] != cell["id"]
        or registered.config["adapter_id"] != cell["adapter"]
        or registered.config["client_id"] != cell["client_id"]
        or expected["os"] != target["os"]
        or expected["architecture"] != target["architecture"]
        or expected["service_mode"] != target["required_service_mode"]
        or expected["product_version"] != matrix["product_version"]
    ):
        raise MatrixError("installed registration does not match matrix cell")
    hub = job["runtime"]["hub"]
    if (
        hub["os"], hub["architecture"], hub["native_or_emulated"], hub["service_mode"]
    ) != (
        expected["os"], expected["architecture"], expected["native_or_emulated"], expected["service_mode"]
    ):
        raise MatrixError("installed registration does not match expected runtime")
    hub_artifact = next((item for item in job["artifacts"] if item["role"] == "hub_executable"), None)
    if hub_artifact is None or hub_artifact["sha256"] != expected["hub_executable_sha256"]:
        raise MatrixError("installed registration does not match Hub artifact")
    execution = job.get("client_execution")
    if not isinstance(execution, dict) or set(execution) != {"kind", "registration", "relay"}:
        raise MatrixError("client execution binding is invalid")
    kind = execution["kind"]
    if kind not in {"local", "ssh_linux", "docker_exec_pipe"}:
        raise MatrixError("client execution kind is invalid")
    if kind == "local":
        if execution["registration"] is not None or execution["relay"] is not None:
            raise MatrixError("local client execution has unused bindings")
    else:
        _bound_private_json(execution["registration"], "client runtime registration")
        _bound_private_json(execution["relay"], "client relay registration")
    return registered


def _validate_session_input_binding(binding, job):
    value = _bound_private_json(binding, "adapter session input")
    try:
        from jsonschema import Draft202012Validator
    except ImportError as error:
        raise MatrixError("jsonschema dependency is unavailable") from error
    schema = read_json(Path(__file__).with_name("matrix_runner") / "adapter_wire.schema.json")
    if next(Draft202012Validator(schema).iter_errors(value), None) is not None:
        raise MatrixError("adapter session input violates schema")
    if (
        value["cell_id"] != job["cell_id"] or value["adapter_id"] != job["adapter"]
        or value["client_id"] != job["adapter"]
    ):
        raise MatrixError("adapter session input identity mismatch")
    return value


def _validate_cohort_binding(binding, product_version, jobs):
    if not isinstance(binding, dict) or set(binding) != {"path", "sha256"}:
        raise MatrixError("cohort input binding is invalid")
    _require_absolute(binding["path"], "cohort input manifest")
    _validate_digest(binding["sha256"], "cohort input manifest")
    path = _validate_private_output(binding["path"], must_exist=True)
    raw = _read_regular(path, MAX_CONFIG_BYTES, private=True)
    if hashlib.sha256(raw).hexdigest() != binding["sha256"]:
        raise MatrixError("cohort input manifest digest mismatch")
    try:
        cohort = _source_evidence.validate_cohort_inputs(str(path), validation_mode="live")
    except Exception as error:
        raise MatrixError("cohort input validation failed") from error
    if cohort.get("schema_version") != 2 or cohort.get("product_version") != product_version:
        raise MatrixError("cohort input identity mismatch")
    sources = {
        item["source_identity"]["role"]: item["source_identity"]
        for item in cohort.get("repository_observations", [])
        if isinstance(item, dict) and isinstance(item.get("source_identity"), dict)
    }
    outputs = [
        output for build in cohort.get("builds", []) if isinstance(build, dict)
        for output in build.get("outputs", []) if isinstance(output, dict)
    ]
    for job in jobs:
        for expected in job["source_identities"]:
            if sources.get(expected["role"]) != expected:
                raise MatrixError("job source identity is outside admitted cohort")
        for artifact in job["artifacts"]:
            if not any(
                output.get("role") == artifact["role"]
                and output.get("path") == artifact["path"]
                and output.get("sha256") == artifact["sha256"]
                and output.get("embedded_version") == artifact["embedded_version"]
                for output in outputs
            ):
                raise MatrixError("job artifact is outside admitted cohort")
    if hashlib.sha256(_read_regular(path, MAX_CONFIG_BYTES, private=True)).hexdigest() != binding["sha256"]:
        raise MatrixError("cohort input manifest changed during validation")
    return cohort


def _validate_job(job, matrix, execution_kind, config_version=1):
    _require_exact_fields(job, INSTALLED_JOB_FIELDS if config_version == 2 else JOB_FIELDS, "job")
    if not isinstance(job["argv"], list) or not job["argv"] or not all(isinstance(item, str) and item for item in job["argv"]):
        raise MatrixError("job argv is invalid")
    if any("\n" in item or "\r" in item for item in job["argv"]):
        raise MatrixError("job argv contains invalid text")
    lowered = " ".join(job["argv"]).lower()
    if any(marker in lowered for marker in ("bearer ", "--password", "--secret", "--token")):
        raise MatrixError("secret-bearing argv is forbidden")
    _require_absolute(job["argv"][0], "job executable")
    _require_absolute(job["cwd"], "job cwd", directory=True)
    _require_absolute(job["environment_file"], "environment file")
    _require_absolute(job["evidence_path"], "evidence path")
    environment_path = _validate_private_output(job["environment_file"], must_exist=True)
    evidence_path = _validate_private_output(job["evidence_path"])
    if environment_path == evidence_path:
        raise MatrixError("private input and output paths alias")
    if job["evidence_mode"] not in ("stdout_json", "file_json"):
        raise MatrixError("evidence mode is invalid")
    if type(job["timeout_seconds"]) is not int or not 1 <= job["timeout_seconds"] <= 3600:
        raise MatrixError("timeout is invalid")
    if type(job["max_output_bytes"]) is not int or not 1024 <= job["max_output_bytes"] <= MAX_EVIDENCE_BYTES:
        raise MatrixError("output bound is invalid")
    if not isinstance(job["command_files"], list) or not job["command_files"]:
        raise MatrixError("command file identities are missing")
    for index, item in enumerate(job["command_files"]):
        if set(item) != {"path", "sha256"}:
            raise MatrixError("command file identity is invalid")
        _require_absolute(item["path"], "command file")
        _validate_digest(item["sha256"], "command file")
        if index == 0 and item["path"] != job["argv"][0]:
            raise MatrixError("first command file must be the executable")
    if not isinstance(job["source_identities"], list) or not job["source_identities"]:
        raise MatrixError("source identities are missing")
    for source in job["source_identities"]:
        _validate_source(source)
    if not isinstance(job["artifacts"], list) or not job["artifacts"]:
        raise MatrixError("artifact identities are missing")
    for artifact in job["artifacts"]:
        _validate_artifact(artifact)
    _validate_runtime(job["runtime"])
    if execution_kind in ACTUAL_KINDS:
        launcher = Path(job["argv"][0]).name
        if launcher in FORBIDDEN_LAUNCHERS:
            raise MatrixError("shell launcher is forbidden")
        cell = next((item for item in matrix["cells"] if item["id"] == job["cell_id"]), None)
        if cell is None:
            raise MatrixError("unknown configured cell")
        client = matrix["clients"][cell["client_id"]]
        _require_cell_adapter(job, cell)
        registered = _validate_installed_job(job, cell, matrix) if config_version == 2 else None
        if execution_kind == "actual_hub_acceptance":
            target = matrix["hub_targets"][cell["hub_target"]]
            hub_runtime = job["runtime"]["hub"]
            if ((hub_runtime["os"], hub_runtime["architecture"]) != (target["os"], target["architecture"])
                    or hub_runtime["service_mode"] not in {target["required_service_mode"], "owned-user-process"}):
                raise MatrixError("Hub runtime does not match matrix target")
        elif cell["client_id"] != "protocol_actual_hub":
            raise MatrixError("interim smoke permits only the protocol adapter")
        if config_version == 1:
            _require_local_hub_runtime(job["runtime"]["hub"])
        roles = {source["role"] for source in job["source_identities"]}
        if roles != SOURCE_ROLES[job["adapter"]] or len(roles) != len(job["source_identities"]):
            raise MatrixError("actual source role binding mismatch")
        for source in job["source_identities"]:
            expected_root = ACTUAL_SOURCE_ROOTS.get(source["role"])
            if expected_root is None or Path(source["repo"]).resolve() != expected_root.resolve():
                raise MatrixError("actual source root binding mismatch")
        artifact_roles = {item["role"] for item in job["artifacts"]}
        if artifact_roles != ARTIFACT_ROLES[job["adapter"]] or len(artifact_roles) != len(job["artifacts"]):
            raise MatrixError("actual artifact role binding mismatch")
        files = [Path(item["path"]).resolve() for item in job["command_files"]]
        if job["adapter"] == "protocol_actual_hub":
            entry = (WORKSPACE_ROOT / "teslatlas-protocol/conformance/run").resolve()
            adapter = (WORKSPACE_ROOT / "teslatlas-protocol/conformance/adapters/actual-hub").resolve()
            if (len(job["argv"]) != 8 or Path(job["argv"][0]).resolve() != entry or files[:2] != [entry, adapter]
                    or job["argv"][1:] != ["--profile", "hub-http-v1@1.0.0", "--adapter", str(adapter), "--config", job["argv"][6], "--json"]
                    or not Path(job["argv"][6]).is_absolute()):
                raise MatrixError("protocol actual adapter invocation is not fixed")
            _validate_private_output(job["argv"][6], must_exist=True)
        elif job["adapter"] in {"typescript_node", "typescript_browser"}:
            entry = (WORKSPACE_ROOT / "hub/tools/interop/client_lanes/run.mjs").resolve()
            if len(job["argv"]) != 3 or Path(job["argv"][1]).resolve() != entry or len(files) < 2 or files[1] != entry:
                raise MatrixError("TypeScript actual adapter invocation is not fixed")
            mode = "node" if job["adapter"] == "typescript_node" else "browser"
            if config_version == 2:
                # Installed jobs use a runner-created descriptor, so the
                # descriptor cannot carry the launcher identity yet.  Bind
                # the executable to the matrix-pinned Node binary before the
                # descriptor is reserved; otherwise a job could self-bind an
                # arbitrary `node` file through command_files.
                launcher_identity = _node_launcher_identity(
                    job, client,
                    {"schema_version": 2, "kind": "installed-client-lane"},
                )
                # The fixed registry creates the controller-bound SessionInput
                # only after the installed session's initial verify. Reserve a
                # fresh private descriptor path here; launch_adapter writes its
                # exact binding before the child is started.
                descriptor_path = _validate_private_output(job["argv"][2], must_exist=False)
                if os.path.lexists(descriptor_path):
                    raise MatrixError("TypeScript installed lane descriptor path is not fresh")
            else:
                descriptor_path = _validate_private_output(job["argv"][2], must_exist=True)
                descriptor = read_json(descriptor_path, private=True)
                if not isinstance(descriptor, dict) or descriptor.get("mode") != mode or descriptor.get("evidence_path") != job["evidence_path"]:
                    raise MatrixError("TypeScript lane descriptor binding mismatch")
                launcher_identity = _node_launcher_identity(job, client, descriptor)
            if mode == "node" and job["runtime"]["client"]["tool_versions"].get("node") != launcher_identity["version"]:
                raise MatrixError("Node launcher and client runtime version differ")
        elif config_version == 1:
            raise PendingCapability("pending: actual adapter invocation contract is unavailable")
        for artifact in job["artifacts"]:
            _verify_artifact_version(
                artifact, matrix["product_version"], True,
                probe_executable=config_version == 1,
            )
    else:
        for artifact in job["artifacts"]:
            _verify_artifact_version(artifact, matrix["product_version"], False)


def load_config(path, matrix, matrix_path):
    config = read_json(path, private=True)
    if not isinstance(config, dict):
        raise MatrixError("config must be an object")
    _validate_config_schema(config)
    version = config.get("schema_version")
    fields = CONFIG_V2_FIELDS if version == 2 else CONFIG_FIELDS
    if set(config) - fields:
        raise MatrixError("unknown config fields")
    if fields - set(config):
        raise MatrixError("missing config fields")
    if version not in {1, 2}:
        raise MatrixError("unsupported config schema")
    if config["execution_kind"] not in ("actual_hub_acceptance", "interim_actual_protocol_smoke", "deterministic_runner_test"):
        raise MatrixError("execution kind is invalid")
    if version == 2 and config["execution_kind"] != "actual_hub_acceptance":
        raise MatrixError("config schema v2 is reserved for installed acceptance")
    if config["matrix_sha256"] != sha256_file(matrix_path):
        raise MatrixError("matrix digest mismatch")
    if config["product_version"] != matrix["product_version"]:
        raise MatrixError("product version mismatch")
    if version == 2:
        binding = config["cohort_inputs"]
        if not isinstance(binding, dict) or set(binding) != {"path", "sha256"}:
            raise MatrixError("cohort input binding is invalid")
        _require_absolute(binding["path"], "cohort input manifest")
        _validate_digest(binding["sha256"], "cohort input manifest")
    _validate_profile(config["profile"], matrix, config["execution_kind"])
    if not isinstance(config["jobs"], list):
        raise MatrixError("jobs must be an array")
    return config


def _observe_job_identities(job, product_version, actual, *, installed=False):
    sources = []
    for expected in job["source_identities"]:
        observed = observe_source_identity(expected["repo"], expected["role"])
        if observed != expected:
            raise MatrixError("source identity mismatch")
        sources.append(observed)
    artifacts = []
    for expected in job["artifacts"]:
        _verify_artifact_version(
            expected, product_version, actual, probe_executable=not installed
        )
        observed = dict(expected, sha256=sha256_file(expected["path"]))
        if observed != expected:
            raise MatrixError("artifact identity mismatch")
        artifacts.append(observed)
    for expected in job["command_files"]:
        if not Path(expected["path"]).is_file() or sha256_file(expected["path"]) != expected["sha256"]:
            raise MatrixError("command file identity mismatch")
    return sources, artifacts


def _read_environment(path):
    value = read_json(path, private=True)
    if not isinstance(value, dict) or len(value) > 128:
        raise MatrixError("environment file is invalid")
    if not all(isinstance(key, str) and key and isinstance(item, str) for key, item in value.items()):
        raise MatrixError("environment file is invalid")
    if any("\0" in key or "=" in key or "\0" in item for key, item in value.items()):
        raise MatrixError("environment file is invalid")
    return value


def _group_pids(pgid):
    try:
        result = subprocess.run(
            ["ps", "-axo", "pid=,pgid=,uid=,stat="], stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=2,
        )
    except (OSError, subprocess.SubprocessError):
        return []
    members = []
    for line in result.stdout.splitlines():
        parts = line.split()
        if len(parts) >= 4 and parts[1] == str(pgid) and parts[2] == str(os.getuid()) and "Z" not in parts[3]:
            members.append(int(parts[0]))
    return members


def _signal_group(pgid, sig):
    try:
        os.killpg(pgid, sig)
    except (ProcessLookupError, PermissionError):
        for pid in _group_pids(pgid):
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass


def _stop_owned_group(process):
    """Terminate the whole owned session even after its direct leader exits."""
    pgid = process.pid
    if _group_pids(pgid):
        _signal_group(pgid, signal.SIGTERM)
        deadline = time.monotonic() + 0.75
        while _group_pids(pgid) and time.monotonic() < deadline:
            time.sleep(0.02)
        if _group_pids(pgid):
            _signal_group(pgid, signal.SIGKILL)
            deadline = time.monotonic() + 1.0
            while _group_pids(pgid) and time.monotonic() < deadline:
                time.sleep(0.02)
    if process.poll() is None:
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=1)


def _bounded_log_metadata(path, maximum):
    path = Path(path)
    size = path.stat().st_size
    exceeded = size > maximum
    if exceeded:
        with open(path, "r+b") as stream:
            stream.truncate(maximum)
        size = maximum
    return {
        "path": str(path), "sha256": sha256_file(path), "bytes": size,
        "truncated": exceeded,
    }


def _execute(job):
    evidence_path = Path(job["evidence_path"])
    if os.path.lexists(evidence_path):
        raise MatrixError("evidence path already exists")
    stdout_path = Path(str(evidence_path) + ".stdout.log")
    stderr_path = Path(str(evidence_path) + ".stderr.log")
    command_path = Path(str(evidence_path) + ".command.json")
    for output in (stdout_path, stderr_path, command_path):
        if os.path.lexists(output):
            raise MatrixError("private execution log already exists")
    environment = os.environ.copy()
    environment.update(_read_environment(job["environment_file"]))
    started_ns = time.time_ns()
    started_at = utc_now()
    process = None
    error = None
    try:
        stdout_fd = os.open(stdout_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        stderr_fd = os.open(stderr_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(stdout_fd, "wb") as stdout, os.fdopen(stderr_fd, "wb") as stderr:
            process = subprocess.Popen(
                job["argv"],
                cwd=job["cwd"],
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
                start_new_session=True,
            )
            deadline = time.monotonic() + job["timeout_seconds"]
            while process.poll() is None:
                if time.monotonic() >= deadline:
                    error = "child timed out"
                    break
                evidence_too_large = (
                    job["evidence_mode"] == "file_json" and evidence_path.exists()
                    and evidence_path.stat().st_size > job["max_output_bytes"]
                )
                if (stdout_path.stat().st_size > job["max_output_bytes"]
                        or stderr_path.stat().st_size > job["max_output_bytes"]
                        or evidence_too_large):
                    error = "child output exceeded bound"
                    break
                time.sleep(0.02)
    except OSError:
        error = "child could not start"
    finally:
        if process is not None:
            _stop_owned_group(process)
    exit_code = process.returncode if process is not None else None
    stdout_log = _bounded_log_metadata(stdout_path, job["max_output_bytes"])
    stderr_log = _bounded_log_metadata(stderr_path, job["max_output_bytes"])
    if job["evidence_mode"] == "stdout_json" and not evidence_path.exists() and not stdout_log["truncated"]:
        stdout_bytes = _read_regular(stdout_path, job["max_output_bytes"], private=True)
        fd = os.open(evidence_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(stdout_bytes)
    if evidence_path.exists() and evidence_path.stat().st_size > job["max_output_bytes"]:
        with open(evidence_path, "r+b") as stream:
            stream.truncate(job["max_output_bytes"])
        error = error or "child evidence exceeded bound"
    if error is None and exit_code != 0:
        error = "child exited nonzero"
    if not evidence_path.is_file():
        error = error or "child did not produce evidence"
    elif evidence_path.stat().st_mtime_ns < started_ns:
        error = error or "child evidence is stale"
    ended_at = utc_now()
    write_private_json(command_path, {
        "argv": job["argv"], "cwd": job["cwd"], "started_at": started_at,
        "ended_at": ended_at, "exit_code": exit_code, "outcome": error or "exited_zero",
    })
    return {
        "started_at": started_at, "ended_at": ended_at,
        "exit_code": exit_code, "error": error,
        "logs": {
            "stdout": stdout_log, "stderr": stderr_log,
            "command_record": {"path": str(command_path), "sha256": sha256_file(command_path)},
            "duration_ms": max(0, (time.time_ns() - started_ns) // 1_000_000),
        },
    }


def _case_result(case_id, status, expected, actual, evidence_kind, transcript, job, evidence_hash, process_evidence=None, reason=None):
    value = {
        "id": case_id,
        "status": status,
        "command": _safe_command(job["adapter"]),
        "exit_code": 0,
        "expected": expected,
        "actual": actual,
        "evidence_kind": evidence_kind,
        "evidence_path": job["evidence_path"],
        "evidence_sha256": evidence_hash,
        "request_transcript": transcript,
    }
    if reason is not None:
        value["not_applicable_reason"] = reason
    if process_evidence is not None:
        value["process_evidence"] = process_evidence
    return value


def _installed_case_result(case_id, job, evidence_hash, *, status="pending", reason=None):
    """Create a redacted v2 row until an admitted adapter supplies case facts.

    Installed adapters do not use the v1 aggregate evidence document.  Their
    private supplement is retained separately and is validated by
    ``receipt_validation``; a case is promoted only by a reviewed adapter
    predicate.  Keeping this constructor separate prevents a syntactically
    valid supplement from silently entering the legacy case normalizer.
    """
    value = _case_result(
        case_id, status, {"required": True}, {"evidence": "installed supplement"},
        "identity", [], job, evidence_hash, reason=reason,
    )
    value["evidence_path"] = None
    return value


def _normalize_installed_evidence(installed_dispatch, job, config, required_cases):
    """Normalize the runner-owned v2 completion without reading v1 evidence.

    The shared runner owns lifecycle and supplement binding.  A reviewed fixed
    registry entry may additionally return its closed semantic admission
    result; no child-supplied case map can promote an installed row by itself.
    Entries without that capability remain pending.
    """
    binding = installed_dispatch.supplement
    if not isinstance(binding, dict) or set(binding) != {"path", "sha256"}:
        raise MatrixError("installed supplement binding is invalid")
    _require_absolute(binding["path"], "installed supplement")
    _validate_digest(binding["sha256"], "installed supplement")
    evidence_hash = binding["sha256"]
    admission = getattr(installed_dispatch, "admission", None)
    if admission is None:
        reason = "pending: reviewed installed case predicates are unavailable"
        return [
            _installed_case_result(case_id, job, evidence_hash, reason=reason)
            for case_id in required_cases
        ], [reason], evidence_hash
    if (not isinstance(admission, dict)
            or set(admission) != {"schema_version", "adapter_id", "cases"}
            or admission.get("schema_version") != 1
            or admission.get("adapter_id") != job["adapter"]
            or not isinstance(admission.get("cases"), list)):
        raise MatrixError("reviewed installed admission result is invalid")
    rows = admission["cases"]
    by_id = {row.get("id"): row for row in rows if isinstance(row, dict)}
    if len(by_id) != len(rows) or set(by_id) != set(required_cases):
        raise MatrixError("reviewed installed admission case coverage is incomplete")
    results = []
    closed = getattr(getattr(installed_dispatch, "result", None), "session_evidence", None)
    runner_closed = (
        getattr(closed, "state", None) == "closed"
        and not getattr(closed, "cleanup_errors", ("missing",))
        and isinstance(getattr(closed, "final_stopped", None), dict)
        and closed.final_stopped.get("status") == "stopped"
        and isinstance(closed.final_stopped.get("service"), dict)
        and closed.final_stopped["service"].get("state") == "stopped"
    )
    for case_id in required_cases:
        row = by_id[case_id]
        if set(row) != {"id", "status", "expected", "actual", "evidence_kind", "request_transcript"}:
            raise MatrixError("reviewed installed admission case shape is invalid")
        if case_id == "installed_service_runtime":
            if row["status"] != "pending" or not runner_closed:
                raise MatrixError("runner-owned service-runtime proof is incomplete")
            status = "passed"
        else:
            if row["status"] != "passed":
                raise MatrixError("reviewed installed admission case is not passed")
            status = "passed"
        _validate_public_json(row["expected"])
        _validate_public_json(row["actual"])
        if not isinstance(row["expected"], dict) or not row["expected"] or not _typed_equal(row["expected"], row["actual"]):
            raise MatrixError("reviewed installed admission case facts are invalid")
        if row["evidence_kind"] not in {"http", "zero_request", "identity"}:
            raise MatrixError("reviewed installed admission case kind is invalid")
        transcript = row["request_transcript"]
        if not isinstance(transcript, list):
            raise MatrixError("reviewed installed admission transcript is invalid")
        # The private TypeScript contract retains scope for route binding. The
        # public matrix schema intentionally exposes only method/path/status/
        # request-id, so remove that private field at this boundary.
        public_transcript = []
        for request in transcript:
            if not isinstance(request, dict) or set(request) != {"method", "route", "status", "request_id", "scope"}:
                raise MatrixError("reviewed installed admission transcript is invalid")
            public_transcript.append({key: request[key] for key in ("method", "route", "status", "request_id")})
        _validate_transcript(case_id, public_transcript, config["execution_kind"] in ACTUAL_KINDS)
        value = _case_result(
            case_id, status, row["expected"], row["actual"], row["evidence_kind"],
            public_transcript, job, evidence_hash,
        )
        value["evidence_path"] = None
        results.append(value)
    return results, [], evidence_hash


def _validate_legacy_evidence(raw, job, config):
    """Recognize current focused receipts without promoting them to matrix cases."""
    role = job["adapter"]
    malformed = "malformed " + role.replace("_", " ") + " adapter evidence"
    if not isinstance(raw, dict) or not raw:
        raise MatrixError(malformed)
    if role == "protocol_actual_hub":
        cases = raw.get("cases")
        ids = [item.get("case_id") for item in cases] if isinstance(cases, list) else []
        artifact_hashes = {item["sha256"] for item in job["artifacts"]}
        valid = (
            raw.get("status") == "passed"
            and raw.get("profile_id") == "hub-http-v1@1.0.0"
            and raw.get("profile_sha256") == config["profile"]["sha256"]
            and raw.get("hub_product_version") == config["product_version"]
            and type(raw.get("runs")) is int and raw["runs"] > 0
            and raw.get("passed") == raw["runs"] and raw.get("failed") == 0
            and len(ids) == raw["runs"] and len(set(ids)) == len(ids)
            and raw.get("binary_sha256") in artifact_hashes
            and raw.get("seed_binary_sha256") in artifact_hashes
            and isinstance(raw.get("native_evidence"), dict)
            and raw["native_evidence"].get("status") == "verified"
        )
    elif role == "typescript_node":
        valid = (
            isinstance(raw.get("runtime"), str) and raw.get("defaultFetch") is True
            and raw.get("invitationClaim") == "passed"
            and raw.get("credentialRotation") == "passed"
            and type(raw.get("vehicleCount")) is int and raw["vehicleCount"] > 0
            and type(raw.get("driveCount")) is int and raw["driveCount"] > 0
            and isinstance(raw.get("hubId"), str) and bool(raw["hubId"])
        )
    elif role == "typescript_browser":
        valid = (
            raw.get("ok") is True and raw.get("defaultFetch") is True
            and raw.get("normalCertificateValidation") is True
            and type(raw.get("corsPreflightCount")) is int and raw["corsPreflightCount"] > 0
            and type(raw.get("vehicleCount")) is int and raw["vehicleCount"] > 0
            and type(raw.get("driveCount")) is int and raw["driveCount"] > 0
        )
    elif role == "swift":
        valid = (
            raw.get("status") == "passed" and raw.get("profile_id") == "hub-http-v1@1.0.0"
            and raw.get("profile_sha256") == config["profile"]["sha256"]
            and raw.get("product_version") == config["product_version"]
            and type(raw.get("vehicle_count")) is int and raw["vehicle_count"] > 0
            and raw.get("page_count") == 3 and isinstance(raw.get("checks"), list)
            and bool(raw["checks"])
        )
    elif role == "home_assistant":
        valid = (
            raw.get("schema_version") == 1 and raw.get("profile_id") == "hub-http-v1@1.0.0"
            and raw.get("profile_sha256") == config["profile"]["sha256"]
            and raw.get("config_entry_loaded") is True
            and type(raw.get("vehicles")) is int and raw["vehicles"] > 0
            and type(raw.get("http_requests_observed")) is int and raw["http_requests_observed"] > 0
            and raw.get("reauth_started_after_401") is True
            and raw.get("reauth_completed_with_fresh_invitation") is True
        )
    elif role == "edge_v2":
        valid = (
            raw.get("secret_material_in_receipt") is False
            and isinstance(raw.get("edge_after_ack"), dict)
            and isinstance(raw.get("fault_witnesses"), dict)
            and isinstance(raw.get("sqlite"), dict)
            and isinstance(raw.get("public_current"), dict)
        )
    else:
        valid = False
    if not valid:
        raise MatrixError(malformed)


def _validate_public_json(value, depth=0):
    if depth > 8:
        raise MatrixError("case assertion nesting exceeds bound")
    if value is None or isinstance(value, (bool, int, float)):
        return
    if isinstance(value, str):
        if len(value) > 4096 or any(marker in value.lower() for marker in ("bearer ", "password=", "secret=")):
            raise MatrixError("case assertion contains private or unbounded text")
        return
    if isinstance(value, list):
        if len(value) > 256:
            raise MatrixError("case assertion list exceeds bound")
        for item in value:
            _validate_public_json(item, depth + 1)
        return
    if isinstance(value, dict):
        if len(value) > 128:
            raise MatrixError("case assertion object exceeds bound")
        for key, item in value.items():
            if not isinstance(key, str) or len(key) > 128 or any(
                marker in key.lower() for marker in ("token", "secret", "password", "authorization", "credential_value")
            ):
                raise MatrixError("case assertion member is private or invalid")
            _validate_public_json(item, depth + 1)
        return
    raise MatrixError("case assertion value is invalid")


def _has_meaningful_fact(value):
    if isinstance(value, bool):
        return True
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return math.isfinite(value)
    if isinstance(value, str):
        return bool(value)
    if isinstance(value, list):
        return bool(value) and any(_has_meaningful_fact(item) for item in value)
    if isinstance(value, dict):
        return bool(value) and any(_has_meaningful_fact(item) for item in value.values())
    return False


def _validate_transcript(case_id, transcript, actual_mode):
    if not isinstance(transcript, list) or len(transcript) > 256:
        raise MatrixError("request transcript is invalid")
    for request in transcript:
        if not isinstance(request, dict) or set(request) != REQUEST_FIELDS:
            raise MatrixError("request transcript entry is invalid")
        method, route, status_code, request_id = (
            request["method"], request["route"], request["status"], request["request_id"]
        )
        if (not isinstance(method, str) or method not in {"GET", "POST", "PUT", "DELETE", "OPTIONS"}
                or not isinstance(route, str) or not route.startswith("/") or "?" in route
                or len(route) > 512 or type(status_code) is not int or not 100 <= status_code <= 599
                or not isinstance(request_id, str) or not re.fullmatch(r"[A-Za-z0-9._:-]{1,128}", request_id)):
            raise MatrixError("request transcript entry is invalid")
    if actual_mode and transcript and case_id in HTTP_RULES:
        required_method, route_pattern = HTTP_RULES[case_id]
        if not any((required_method is None or item["method"] == required_method)
                   and route_pattern.search(item["route"]) for item in transcript):
            raise MatrixError("case transcript does not prove its operation")
    elif actual_mode and transcript and not any(
        item["route"] not in {"/healthz", "/readyz"} and not item["route"].startswith("/.well-known/")
        for item in transcript
    ):
        raise MatrixError("case transcript is unrelated health traffic")
    if actual_mode and transcript:
        for method, pattern, statuses in HTTP_STATUS_RULES.get(case_id, []):
            if not any(item["method"] == method and pattern.search(item["route"])
                       and item["status"] in statuses for item in transcript):
                raise MatrixError("case transcript has wrong HTTP outcome")


def _validate_identity_proof(case_id, proof, job):
    if not isinstance(proof, dict) or not proof:
        raise MatrixError("identity case lacks bound process evidence")
    artifact_hashes = {item["role"]: item["sha256"] for item in job["artifacts"]}
    if case_id == "candidate_artifact_identity":
        if set(proof) != {"hub", "packed", "client"}:
            raise MatrixError("candidate process evidence fields are invalid")
        hub, packed, client = proof["hub"], proof["packed"], proof["client"]
        hub_fields = {"status", "hub_pid", "launcher_pid", "hub_started_at", "binary_sha256", "seed_binary_sha256", "cleanup_sha256"}
        if (not isinstance(hub, dict) or set(hub) != hub_fields or hub.get("status") != "verified"
                or type(hub.get("hub_pid")) is not int or hub["hub_pid"] <= 0
                or type(hub.get("launcher_pid")) is not int or hub["launcher_pid"] <= 0
                or not isinstance(hub.get("hub_started_at"), str) or not hub["hub_started_at"]
                or hub.get("binary_sha256") != artifact_hashes.get("hub_executable")):
            raise MatrixError("candidate process evidence is not artifact-bound")
        for name in ("binary_sha256", "seed_binary_sha256", "cleanup_sha256"):
            _validate_digest(hub[name], "candidate process " + name)
        if job["adapter"] in {"typescript_node", "typescript_browser"}:
            packed_fields = {"packageName", "packageVersion", "entry", "entrySha256", "tarballSha256", "installedContentManifestSha256", "installedMemberCount"}
            if (not isinstance(packed, dict) or set(packed) != packed_fields
                    or packed.get("packageName") != "@teslatlas/sdk"
                    or packed.get("packageVersion") != "2026.36.2"
                    or packed.get("tarballSha256") != artifact_hashes.get("typescript_sdk_tarball")
                    or packed.get("entry") != ("dist/node.js" if job["adapter"] == "typescript_node" else "dist/browser.js")
                    or type(packed.get("installedMemberCount")) is not int or packed["installedMemberCount"] != 81
                    or not isinstance(client, dict) or type(client.get("pid")) is not int or client["pid"] <= 0):
                raise MatrixError("packed client process evidence is invalid")
            for name in ("entrySha256", "tarballSha256", "installedContentManifestSha256"):
                _validate_digest(packed[name], "packed SDK " + name)
            runtime_client = job["runtime"]["client"]
            if job["adapter"] == "typescript_node":
                expected_fields = {"os", "architecture", "node", "pid"}
                if client.get("os") == "linux":
                    expected_fields.add("distribution")
                if set(client) != expected_fields:
                    raise MatrixError("Node process evidence fields are invalid")
                observed_os = "macOS" if client.get("os") == "darwin" else client.get("distribution") if client.get("os") == "linux" else None
                observed_arch = _normalize_arch(client.get("architecture"))
                if (client.get("node") != runtime_client["tool_versions"].get("node")
                        or observed_os != runtime_client["os"] or observed_arch != runtime_client["architecture"]):
                    raise MatrixError("Node runtime identity is not process-bound")
            else:
                expected_fields = {"os", "architecture", "distribution", "kernel", "uid", "pid", "browser"}
                if (set(client) != expected_fields or client.get("os") != "linux"
                        or not isinstance(client.get("distribution"), str) or not client["distribution"]
                        or not isinstance(client.get("kernel"), str) or not client["kernel"]
                        or not isinstance(client.get("browser"), str) or not client["browser"]
                        or type(client.get("uid")) is not int or client["uid"] < 0):
                    raise MatrixError("browser process evidence fields are invalid")
                observed_arch = _normalize_arch(client.get("architecture"))
                if (client.get("browser") != runtime_client["tool_versions"].get("chromium")
                        or client.get("distribution") != runtime_client["os"]
                        or observed_arch != runtime_client["architecture"]):
                    raise MatrixError("browser runtime identity is not process-bound")
    else:
        raise PendingCapability("pending: identity verifier contract is unavailable")


def _validate_actual_assertion(case_id, expected, actual, job):
    required = ASSERTION_KEYS.get(case_id)
    if case_id == "bad_invitation" and isinstance(expected, dict) and expected.get("outgoing_requests") == 0:
        required = {"typed_error", "outgoing_requests"}
    if required is None or not isinstance(expected, dict) or set(expected) != required:
        raise MatrixError("case assertion fields do not match fixed contract")
    if case_id == "candidate_artifact_identity":
        hashes = {item["role"]: item["sha256"] for item in job["artifacts"]}
        fixed = {"hub_sha256": hashes.get("hub_executable"),
                 "tarball_sha256": hashes.get("typescript_sdk_tarball"),
                 "package_version": "2026.36.2", "installed_members": 81}
        if not _typed_equal(expected, fixed):
            raise MatrixError("candidate assertion is not bound to exact artifacts")
        return
    if case_id == "discovery_identity_profile":
        try:
            import uuid
            uuid.UUID(expected["hub_id"])
        except (ValueError, TypeError, AttributeError) as error:
            raise MatrixError("discovery Hub identity is invalid") from error
        fixed = dict(expected, hub_id=expected["hub_id"])
        fixed.update(api_versions=["1.0"], protocol="teslatlas-sync", protocol_major=1,
                     pack_format="sqlite-zstd", version="2026.36.2")
        if not _typed_equal(expected, fixed):
            raise MatrixError("discovery assertion differs from fixed profile")
        return
    fixed = EXACT_CASE_EXPECTED.get(case_id)
    if case_id == "bad_invitation" and expected.get("outgoing_requests") == 0:
        fixed = {"outgoing_requests": 0, "typed_error": "protocol_validation"}
    if fixed is None or not _typed_equal(expected, fixed):
        raise MatrixError("case assertion differs from independent scenario")


def _admit_case(case, job, actual_mode):
    case_id = case["id"]
    expected, actual = case["expected"], case["actual"]
    _validate_public_json(expected)
    _validate_public_json(actual)
    if not isinstance(expected, dict) or not expected or not _has_meaningful_fact(expected):
        raise MatrixError("case assertion is empty or meaningless")
    if not _typed_equal(expected, actual):
        return False, "expected and actual case values disagree"
    kind = case["evidence_kind"]
    transcript = case["request_transcript"]
    _validate_transcript(case_id, transcript, actual_mode)
    if actual_mode:
        required_kind = "zero_request" if case_id in ZERO_REQUEST_CASES else "identity" if case_id in IDENTITY_CASES else "http"
        if case_id in OPTIONAL_PREFLIGHT_CASES:
            required_kind = kind if kind in {"http", "zero_request"} else required_kind
        if kind != required_kind:
            raise MatrixError("case evidence kind does not match fixed contract")
        _validate_actual_assertion(case_id, expected, actual, job)
    if kind == "zero_request":
        if case_id not in ZERO_REQUEST_CASES | OPTIONAL_PREFLIGHT_CASES or transcript or expected.get("outgoing_requests") != 0:
            raise MatrixError("zero-request case evidence is invalid")
        if case_id not in {"unsupported_operation_zero_requests", "polling_transport_zero_sse"} and not isinstance(expected.get("typed_error"), str):
            raise MatrixError("client preflight typed error is missing")
    elif kind == "identity":
        if transcript:
            raise MatrixError("identity case contains transport evidence")
        if actual_mode:
            _validate_identity_proof(case_id, case.get("process_evidence"), job)
        elif not isinstance(case.get("process_evidence"), dict) or not case["process_evidence"]:
            raise MatrixError("identity case lacks bound process evidence")
    elif kind != "http" or not transcript:
        raise MatrixError("passed transport case lacks request evidence")
    return True, None


def _normalize_evidence(raw, job, config, required_cases, evidence_hash):
    if not isinstance(raw, dict):
        raise MatrixError("adapter evidence must be an object")
    if "execution_kind" not in raw:
        # Existing focused adapters predate the matrix evidence contract. Their
        # successful process exit is retained, but no matrix case is promoted
        # without the required identity and request/assertion fields.
        _validate_legacy_evidence(raw, job, config)
        return [
            _case_result(
                case_id, "pending", {"required": True}, {"evidence": "missing"},
                "http", [], job, evidence_hash,
            )
            for case_id in required_cases
        ], ["pending: adapter evidence lacks canonical matrix bindings"]
    _require_exact_fields(raw, NORMALIZED_EVIDENCE_FIELDS, "adapter evidence")
    expected_header = {
        "schema_version": 1,
        "execution_kind": config["execution_kind"],
        "adapter": job["adapter"],
        "cell_id": job["cell_id"],
        "product_version": config["product_version"],
        "profile_id": config["profile"]["id"],
        "profile_revision": config["profile"]["revision"],
        "profile_sha256": config["profile"]["sha256"],
    }
    for key, expected in expected_header.items():
        if raw[key] != expected:
            raise MatrixError("adapter evidence identity mismatch")
    if raw["source_identities"] != job["source_identities"]:
        raise MatrixError("source evidence identity mismatch")
    if raw["artifacts"] != job["artifacts"]:
        raise MatrixError("artifact evidence identity mismatch")
    if raw["runtime"] != job["runtime"]:
        raise MatrixError("runtime identity mismatch")
    if not isinstance(raw["cases"], list):
        raise MatrixError("adapter cases must be an array")
    ids = [item.get("id") for item in raw["cases"] if isinstance(item, dict)]
    if (len(ids) != len(raw["cases"]) or not all(isinstance(item, str) for item in ids)
            or len(ids) != len(set(ids))):
        raise MatrixError("duplicate or malformed adapter case")
    if set(ids) - set(required_cases):
        raise MatrixError("undeclared adapter case")
    results = []
    errors = []
    for case in raw["cases"]:
        unknown = set(case) - CASE_FIELDS
        needed = {"id", "status", "expected", "actual", "evidence_kind", "request_transcript"}
        if unknown or needed - set(case):
            raise MatrixError("adapter case shape is invalid")
        status = case["status"]
        if status not in ("passed", "failed", "pending", "not_applicable"):
            raise MatrixError("adapter case status is invalid")
        transcript = case["request_transcript"]
        evidence_kind = case["evidence_kind"]
        if evidence_kind not in ("http", "zero_request", "identity"):
            raise MatrixError("case evidence kind is invalid")
        if not isinstance(transcript, list) or len(transcript) > 256:
            raise MatrixError("request transcript is invalid")
        for request in transcript:
            if not isinstance(request, dict) or set(request) != REQUEST_FIELDS:
                raise MatrixError("request transcript entry is invalid")
            if not isinstance(request["method"], str) or not isinstance(request["route"], str) or type(request["status"]) is not int or not isinstance(request["request_id"], str) or not request["request_id"]:
                raise MatrixError("request transcript entry is invalid")
        _validate_public_json(case["expected"])
        _validate_public_json(case["actual"])
        if "process_evidence" in case:
            _validate_public_json(case["process_evidence"])
        reason = case.get("not_applicable_reason")
        if status == "not_applicable":
            # Every case listed for these 18 rows is required. N/A is legal
            # only for an undeclared operation outside this matrix, so it can
            # never discharge one of these required case IDs.
            status = "failed"
            errors.append("illegal not-applicable required case")
        actual_mode = config["execution_kind"] in ACTUAL_KINDS
        if status == "passed" and actual_mode and job["adapter"] not in SUPPORTED_ACTUAL_CASE_ADAPTERS:
            status = "pending"
            errors.append("pending: adapter case contract is unavailable")
        elif status == "passed" and actual_mode and case["id"] == "installed_service_runtime":
            status = "pending"
            errors.append("pending: installed service verifier is unavailable")
        elif status == "passed":
            admitted, problem = _admit_case(case, job, config["execution_kind"] in ACTUAL_KINDS)
            if not admitted:
                status = "failed"
                errors.append(problem)
        results.append(
            _case_result(
                case["id"], status, case["expected"], case["actual"], evidence_kind,
                transcript, job, evidence_hash,
                process_evidence=case.get("process_evidence"), reason=reason,
            )
        )
    present = {result["id"] for result in results}
    for case_id in required_cases:
        if case_id not in present:
            results.append(
                _case_result(
                    case_id,
                    "pending",
                    {"required": True},
                    {"evidence": "missing"},
                    "http",
                    [],
                    job,
                    evidence_hash,
                )
            )
    order = {case_id: index for index, case_id in enumerate(required_cases)}
    results.sort(key=lambda item: order[item["id"]])
    if any(item["status"] == "passed" for item in results) and not any(
        item["request_transcript"] for item in results
    ):
        errors.append("row lacks real transport transcript")
    return results, errors


def _empty_cell(cell, matrix, error):
    required_cases = matrix["clients"][cell["client_id"]]["required_cases"]
    return {
        "cell_id": cell["id"],
        "client_id": cell["client_id"],
        "hub_target": cell["hub_target"],
        "required": True,
        "status": "pending",
        "adapter": cell["adapter"],
        "command": [],
        "exit_code": None,
        "started_at": None,
        "ended_at": None,
        "evidence_path": None,
        "evidence_sha256": None,
        "identity_before": None,
        "identity_after": None,
        "runtime_expected": None,
        "runtime_actual": None,
        "launcher_identity": None,
        "execution_log": None,
        "case_results": [
            {
                "id": case_id,
                "status": "pending",
                "command": [],
                "exit_code": None,
                "expected": {"required": True},
                "actual": {"evidence": "missing"},
                "evidence_kind": "http",
                "evidence_path": None,
                "evidence_sha256": None,
                "request_transcript": [],
            }
            for case_id in required_cases
        ],
        "errors": [error],
    }


def _run_job(job, cell, config, matrix):
    required_cases = matrix["clients"][cell["client_id"]]["required_cases"]
    result = _empty_cell(cell, matrix, "job not executed")
    result.update(
        command=_safe_command(cell["adapter"]),
        errors=[],
    )
    try:
        _validate_job(job, matrix, config["execution_kind"], config["schema_version"])
        installed_dispatch = None
        if (config["schema_version"] == 1 and config["execution_kind"] in ACTUAL_KINDS
                and job["adapter"] in {"typescript_node", "typescript_browser"}):
            descriptor = read_json(job["argv"][2], private=True)
            result["launcher_identity"] = _node_launcher_identity(
                job, matrix["clients"][cell["client_id"]], descriptor
            )
        # v2 installed adapters publish a private supplement from the
        # runner-owned close path; they do not produce the v1 evidence file.
        # Keep the legacy path for schema-v1 jobs only.
        if config["schema_version"] == 1:
            result["evidence_path"] = job["evidence_path"]
        result["runtime_expected"] = job["runtime"]
        before_sources, before_artifacts = _observe_job_identities(
            job, config["product_version"], config["execution_kind"] in ACTUAL_KINDS,
            installed=config["schema_version"] == 2,
        )
        result["identity_before"] = {"sources": before_sources, "artifacts": before_artifacts}
        if config["schema_version"] == 2:
            installed_registry = importlib.import_module(f"{_MATRIX_RUNNER_PACKAGE}.installed_registry")
            try:
                installed_dispatch = installed_registry.dispatch(job, cell, config, matrix)
            except installed_registry.InstalledRegistryPending as error:
                raise PendingCapability(str(error)) from error
            except installed_registry.InstalledRegistryError as error:
                raise MatrixError(str(error)) from error
            execution = {
                "started_at": None, "ended_at": None,
                "exit_code": installed_dispatch.result.exit_code,
                "error": None, "logs": installed_dispatch.logs,
            }
        else:
            execution = _execute(job)
        result.update(
            started_at=execution["started_at"],
            ended_at=execution["ended_at"],
            exit_code=execution["exit_code"],
            execution_log=execution["logs"],
        )
        if execution["error"]:
            raise MatrixError(execution["error"])
        if installed_dispatch is not None:
            # The supplement is the v2 completion binding.  Do not read or
            # normalize the legacy evidence_path, which may not exist for a
            # reviewed installed entry.
            case_results, case_errors, evidence_hash = _normalize_installed_evidence(
                installed_dispatch, job, config, required_cases
            )
            result["case_results"] = case_results
            result["evidence_path"] = installed_dispatch.supplement["path"]
            result["evidence_sha256"] = evidence_hash
            result["runtime_actual"] = getattr(installed_dispatch, "runtime_actual", None)
            result["errors"].extend(case_errors)
        else:
            evidence_hash = sha256_file(job["evidence_path"])
            result["evidence_sha256"] = evidence_hash
            evidence = read_json(job["evidence_path"], job["max_output_bytes"], private=True)
            case_results, case_errors = _normalize_evidence(
                evidence, job, config, required_cases, evidence_hash
            )
            result["case_results"] = case_results
            result["runtime_actual"] = evidence.get("runtime") if isinstance(evidence, dict) else None
            result["errors"].extend(case_errors)
        after_sources, after_artifacts = _observe_job_identities(
            job, config["product_version"], config["execution_kind"] in ACTUAL_KINDS,
            installed=config["schema_version"] == 2,
        )
        result["identity_after"] = {"sources": after_sources, "artifacts": after_artifacts}
        if result["identity_after"] != result["identity_before"]:
            raise MatrixError("identity changed during adapter execution")
        statuses = {item["status"] for item in result["case_results"]}
        hard_errors = [error for error in result["errors"] if not error.startswith("pending:")]
        if hard_errors or "failed" in statuses:
            result["status"] = "failed"
        elif "pending" in statuses or "not_applicable" in statuses:
            result["status"] = "pending"
        else:
            result["status"] = "passed"
        if installed_dispatch is not None:
            receipt_validation = importlib.import_module(f"{_MATRIX_RUNNER_PACKAGE}.receipt_validation")
            result["supplement"] = dict(installed_dispatch.supplement)
            try:
                receipt_validation._supplement(installed_dispatch.supplement, result)
            except receipt_validation.ReceiptValidationError:
                # Supplement bytes are private and may contain credentials or
                # host topology.  Preserve the completed row and its binding,
                # but turn a semantic rejection into a permanent redacted row
                # failure instead of allowing it to escape the aggregate.
                result["errors"] = [
                    error for error in result["errors"]
                    if not error.startswith("pending: reviewed installed case predicates")
                ]
                result["errors"].append("installed supplement rejected")
                result["status"] = "failed"
    except PendingCapability as error:
        result["errors"].append(str(error))
        result["status"] = "pending"
    except (MatrixError, OSError, subprocess.SubprocessError) as error:
        result["errors"].append(str(error) if isinstance(error, MatrixError) else "job execution failed")
        if any(label in result["errors"][-1] for label in ("missing", "lacks canonical")):
            result["status"] = "pending"
        else:
            result["status"] = "failed"
    return result


def _base_receipt(matrix, matrix_path, execution_kind="invalid", cohort_inputs=None):
    receipt = {
        "schema_version": 2 if execution_kind == "actual_hub_acceptance" else 1,
        "matrix_id": matrix.get("matrix_id", "invalid"),
        "matrix_sha256": sha256_file(matrix_path) if Path(matrix_path).is_file() else "0" * 64,
        "execution_kind": execution_kind,
        "acceptance_scope": (
            "schema-and-admission-only" if execution_kind == "deterministic_runner_test"
            else "interim-protocol-smoke" if execution_kind == "interim_actual_protocol_smoke"
            else "actual-hub-acceptance"
        ),
        "product_version": matrix.get("product_version", "unknown"),
        "profile_id": matrix.get("profile", {}).get("id", "unknown"),
        "profile_revision": matrix.get("profile", {}).get("revision", "unknown"),
        "profile_sha256": matrix.get("profile", {}).get("manifest_sha256", "0" * 64),
        "started_at": utc_now(),
        "ended_at": utc_now(),
        "status": "failed",
        "complete": False,
        "summary": {"required_cells": 18, "passed": 0, "failed": 0, "pending": 18},
        "cells": [],
        "cohort_requirements": matrix.get("cohort_requirements", []),
        "errors": [],
    }
    if execution_kind == "actual_hub_acceptance":
        receipt["cohort_inputs"] = cohort_inputs
    return receipt


def _validate_receipt_schema(receipt):
    schema_path = Path(__file__).resolve().parents[2] / "docs/compatibility/receipt.schema.json"
    try:
        from jsonschema import Draft202012Validator
    except ImportError as error:
        raise MatrixError("jsonschema dependency is unavailable") from error
    schema = read_json(schema_path, private=False)
    errors = sorted(Draft202012Validator(schema).iter_errors(receipt), key=lambda item: list(item.path))
    if errors:
        raise MatrixError("generated receipt violates schema")


def _preflight_private_boundaries(config, config_path, receipt_path):
    paths = {_resolved(config_path), _resolved(receipt_path)}
    for job in config["jobs"]:
        if config.get("schema_version") == 2:
            installed = job.get("installed_session", {})
            execution = job.get("client_execution", {})
            for binding in (
                installed.get("config"), installed.get("registration_inventory"),
                execution.get("registration"), execution.get("relay"),
            ):
                if binding is None:
                    continue
                resolved = _validate_private_output(binding["path"], must_exist=True)
                if resolved in paths:
                    raise MatrixError("private matrix paths alias")
                paths.add(resolved)
        for key in ("environment_file", "evidence_path"):
            resolved = _validate_private_output(job[key], must_exist=key == "environment_file")
            if resolved in paths:
                raise MatrixError("private matrix paths alias")
            paths.add(resolved)
        evidence = _resolved(job["evidence_path"])
        for suffix in (".stdout.log", ".stderr.log", ".command.json"):
            derived = _resolved(str(evidence) + suffix)
            if derived in paths or _is_within(derived, WORKSPACE_ROOT.resolve()):
                raise MatrixError("private matrix paths alias")
            paths.add(derived)
        descriptor = None
        if job["adapter"] == "protocol_actual_hub" and len(job["argv"]) > 6:
            descriptor = job["argv"][6]
        elif job["adapter"] in {"typescript_node", "typescript_browser"} and len(job["argv"]) > 2:
            descriptor = job["argv"][2]
        if descriptor is not None:
            dynamic_installed_descriptor = (
                config.get("schema_version") == 2
                and job.get("adapter") in {"typescript_node", "typescript_browser"}
            )
            resolved = _validate_private_output(descriptor, must_exist=not dynamic_installed_descriptor)
            if resolved in paths:
                raise MatrixError("private matrix paths alias")
            paths.add(resolved)
            if (not dynamic_installed_descriptor and config.get("schema_version") == 2
                    and job["adapter"] in {"typescript_node", "typescript_browser"}):
                value = read_json(resolved, private=True)
                binding = value.get("session_input") if isinstance(value, dict) else None
                if isinstance(binding, dict) and set(binding) == {"path", "sha256"}:
                    session = _validate_private_output(binding["path"], must_exist=True)
                    if session in paths:
                        raise MatrixError("private matrix paths alias")
                    paths.add(session)


def run_matrix(config_path, receipt_path, require_complete=False):
    receipt_path = Path(receipt_path)
    _validate_private_output(receipt_path)
    if os.path.lexists(receipt_path):
        raise MatrixError("receipt path already exists")
    matrix, matrix_path = load_matrix()
    receipt = _base_receipt(matrix, matrix_path)
    try:
        config_path = _validate_private_output(config_path, must_exist=True)
        if _resolved(config_path) == _resolved(receipt_path):
            raise MatrixError("config and receipt paths alias")
        config = load_config(config_path, matrix, matrix_path)
        _preflight_private_boundaries(config, config_path, receipt_path)
        cohort_before = None
        if config["schema_version"] == 2:
            cohort_before = _validate_cohort_binding(
                config["cohort_inputs"], config["product_version"], config["jobs"]
            )
        for job in config["jobs"]:
            try:
                _validate_job(job, matrix, config["execution_kind"], config["schema_version"])
            except PendingCapability:
                pass
        receipt.update(
            schema_version=config["schema_version"],
            execution_kind=config["execution_kind"],
            acceptance_scope=(
                "schema-and-admission-only" if config["execution_kind"] == "deterministic_runner_test"
                else "interim-protocol-smoke" if config["execution_kind"] == "interim_actual_protocol_smoke"
                else "actual-hub-acceptance"
            ),
            profile_sha256=config["profile"]["sha256"],
        )
        if config["schema_version"] == 2:
            receipt["cohort_inputs"] = config["cohort_inputs"]
        jobs_by_cell = {}
        duplicates = set()
        for job in config["jobs"]:
            if not isinstance(job, dict) or not isinstance(job.get("cell_id"), str):
                receipt["errors"].append("malformed configured job")
                continue
            if job["cell_id"] in jobs_by_cell:
                duplicates.add(job["cell_id"])
            else:
                jobs_by_cell[job["cell_id"]] = job
        known = {cell["id"] for cell in matrix["cells"]}
        if duplicates:
            receipt["errors"].append("duplicate configured cell")
        if set(jobs_by_cell) - known:
            receipt["errors"].append("unknown configured cell")
        missing = known - set(jobs_by_cell)
        if missing:
            receipt["errors"].append("missing required cell")
        cells = []
        for cell in matrix["cells"]:
            if cell["id"] in duplicates:
                cells.append(_empty_cell(cell, matrix, "duplicate configured cell"))
            elif cell["id"] not in jobs_by_cell:
                cells.append(_empty_cell(cell, matrix, "missing required cell"))
            else:
                job = jobs_by_cell[cell["id"]]
                row_before = None
                row_error = None
                if config["schema_version"] == 2:
                    try:
                        # Revalidate the exact admitted source/export/output
                        # boundary immediately before this row.  Passing the
                        # single job keeps the check attributable to the row
                        # while the helper still audits the complete cohort.
                        row_before = _validate_cohort_binding(
                            config["cohort_inputs"], config["product_version"], [job]
                        )
                    except (MatrixError, OSError) as error:
                        row_error = "row cohort preflight failed"
                        receipt["errors"].append(row_error)
                if row_error is not None:
                    failed = _empty_cell(cell, matrix, row_error)
                    failed["status"] = "failed"
                    cells.append(failed)
                    continue
                row = _run_job(job, cell, config, matrix)
                if config["schema_version"] == 2:
                    try:
                        row_after = _validate_cohort_binding(
                            config["cohort_inputs"], config["product_version"], [job]
                        )
                        if row_after != row_before:
                            raise MatrixError("row cohort identity changed during adapter execution")
                    except (MatrixError, OSError) as error:
                        message = (
                            "row cohort identity changed during adapter execution"
                            if isinstance(error, MatrixError)
                            and str(error) == "row cohort identity changed during adapter execution"
                            else "row cohort postflight failed"
                        )
                        row["errors"].append(message)
                        row["status"] = "failed"
                        receipt["errors"].append(message)
                cells.append(row)
        if config["schema_version"] == 2:
            for item in cells:
                item.setdefault("supplement", None)
            cohort_after = _validate_cohort_binding(
                config["cohort_inputs"], config["product_version"], config["jobs"]
            )
            if cohort_after != cohort_before:
                receipt["errors"].append("cohort inputs changed during installed matrix")
        receipt["cells"] = cells
        passed = sum(item["status"] == "passed" for item in cells)
        failed = sum(item["status"] == "failed" for item in cells)
        pending = len(cells) - passed - failed
        receipt["summary"] = {"required_cells": 18, "passed": passed, "failed": failed, "pending": pending}
        receipt["complete"] = (
            passed == 18
            and not receipt["errors"]
            and config["execution_kind"] != "interim_actual_protocol_smoke"
            and (
                config["schema_version"] == 1
                or all(isinstance(item.get("supplement"), dict) for item in cells)
            )
        )
        hard_aggregate_errors = [
            error for error in receipt["errors"] if error != "missing required cell"
        ]
        receipt["status"] = (
            "passed" if receipt["complete"]
            else "failed" if failed or hard_aggregate_errors
            else "pending"
        )
    except (MatrixError, OSError, subprocess.SubprocessError) as error:
        receipt["errors"].append(str(error) if isinstance(error, MatrixError) else "matrix execution failed")
    receipt["ended_at"] = utc_now()
    try:
        _validate_receipt_schema(receipt)
    except MatrixError as error:
        receipt["errors"].append(str(error))
        receipt["status"] = "failed"
        receipt["complete"] = False
    write_private_json(receipt_path, receipt)
    if require_complete and not receipt["complete"]:
        return 1
    return 0 if receipt["status"] == "passed" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--require-complete", action="store_true")
    parser.add_argument("--receipt", required=True, type=Path)
    args = parser.parse_args()
    try:
        code = run_matrix(args.config, args.receipt, args.require_complete)
    except (MatrixError, OSError):
        print("matrix runner failed; inspect the redacted receipt", file=sys.stderr)
        return 1
    if code:
        print("matrix incomplete; inspect the redacted receipt", file=sys.stderr)
    else:
        print(json.dumps({"status": "passed", "receipt": str(args.receipt)}))
    return code


if __name__ == "__main__":
    raise SystemExit(main())
