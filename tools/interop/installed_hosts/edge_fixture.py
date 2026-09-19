# SPDX-License-Identifier: AGPL-3.0-only
"""Pure producer for the private root-owned Edge installed fixture input."""
import hashlib
import json
import os
import re
import stat
from collections.abc import Mapping
from pathlib import Path

from .contract import MAX_JSON_BYTES, ContractError, validate_file_binding


LANES = (
    ("primary", "installed", 18480, 18500, 18510),
    ("reference", "normal_aux", 18490, 18501, 18511),
    ("fault", "fault_aux", 18491, 18502, 18512),
    ("negative", "normal_aux", 18492, 18503, 18513),
)
EDGE_CELLS = frozenset((
    "edge_v2__macos_arm64", "edge_v2__debian13_amd64",
    "edge_v2__debian13_arm64",
))
MAX_PUBLIC_PROFILE_MEMBER_BYTES = 1_048_576
_DIGEST = re.compile(r"^[0-9a-f]{64}$")
_OWNER_FIELDS = frozenset((
    "schema_version", "kind", "run_id", "cell_id", "session_id",
    "instance_nonce", "host_registration", "recipe", "vectors",
    "edge_profile", "producer_registration", "launch_inventory",
    "credentials", "private_root", "primary_hub_root",
))
_RESERVATION_FIELDS = frozenset((
    "schema_version", "kind", "run_id", "cell_id", "session_id",
    "instance_nonce", "lease", "lanes",
))
_LANE_FIELDS = frozenset((
    "id", "hub_role", "hub_root", "producer_root", "hub_port",
    "receiver_port", "delivery_port",
))
_CREDENTIAL_FIELDS = frozenset((
    "id", "role", "root", "local", "uid", "mode", "bytes", "der_sha256",
))
_CREDENTIAL_ROLES = (
    "edge_server_ca", "edge_server_leaf", "edge_server_key",
    "edge_client_ca", "edge_client_leaf", "edge_client_key",
    "edge_delivery_bearer", "edge_receiver_bearer", "edge_spool_key",
    "edge_credential_store", "negative_server_ca", "negative_server_leaf",
    "negative_server_key", "untrusted_client_leaf", "untrusted_client_key",
    "bad_delivery_bearer",
)
_CERTIFICATE_ROLES = frozenset((
    "edge_server_ca", "edge_server_leaf", "edge_client_ca",
    "edge_client_leaf", "negative_server_ca", "negative_server_leaf",
    "untrusted_client_leaf",
))


class EdgeFixtureError(ContractError):
    """The private Edge fixture boundary is incomplete or foreign."""


def _fail(message):
    raise EdgeFixtureError(message)


def _exact(value, fields, label):
    if not isinstance(value, Mapping):
        _fail(label + " must be an object")
    missing = sorted(fields - set(value))
    extra = sorted(set(value) - fields)
    if missing:
        _fail(label + " missing fields: " + ", ".join(missing))
    if extra:
        _fail(label + " unexpected fields: " + ", ".join(extra))
    return value


def _canonical(value, label):
    if not isinstance(value, str) or not value.startswith("/"):
        _fail(label + " path is invalid")
    path = Path(value)
    if str(path.resolve(strict=False)) != value:
        _fail(label + " path is not canonical")
    return path


def _private_binding(value, label, *, private=True, maximum=MAX_JSON_BYTES):
    try:
        checked = validate_file_binding(value, label)
    except ContractError as error:
        raise EdgeFixtureError(str(error)) from error
    path = _canonical(checked["path"], label)
    try:
        info = path.lstat()
    except OSError as error:
        raise EdgeFixtureError(label + " is unavailable") from error
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        _fail(label + " must be a single-link regular file")
    if private and stat.S_IMODE(info.st_mode) & 0o077:
        _fail(label + " must be owner-only")
    if info.st_size > maximum:
        _fail(label + " exceeds the private binding limit")
    raw = path.read_bytes()
    if len(raw) != info.st_size or hashlib.sha256(raw).hexdigest() != checked["sha256"]:
        _fail(label + " digest changed")
    return dict(checked)


def _credential_custody(binding, *, cell_id, session_id, instance_nonce):
    checked = _private_binding(binding, "Edge credentials")
    try:
        value = json.loads(Path(checked["path"]).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EdgeFixtureError("Edge credential custody is not strict JSON") from error
    fields = frozenset(("schema_version", "session_id", "cell_id", "instance_nonce", "files"))
    _exact(value, fields, "Edge credential custody")
    if (
        value["schema_version"] != 1 or value["session_id"] != session_id
        or value["cell_id"] != cell_id or value["instance_nonce"] != instance_nonce
        or not isinstance(value["files"], list) or len(value["files"]) != len(_CREDENTIAL_ROLES)
    ):
        _fail("Edge credential custody identity or inventory is invalid")
    expected_ids = []
    for role in _CREDENTIAL_ROLES:
        lane = "negative" if role.startswith(("negative_", "untrusted_", "bad_")) else "primary"
        expected_ids.append(lane + "." + role)
    seen_paths = set()
    for expected_role, expected_id, row in zip(_CREDENTIAL_ROLES, expected_ids, value["files"], strict=True):
        _exact(row, _CREDENTIAL_FIELDS, "Edge credential")
        if (
            row["id"] != expected_id or row["role"] != expected_role
            or isinstance(row["uid"], bool) or not isinstance(row["uid"], int) or row["uid"] < 0
            or row["mode"] != 384 or isinstance(row["bytes"], bool)
            or not isinstance(row["bytes"], int) or row["bytes"] < 1
        ):
            _fail("Edge credential custody row is invalid")
        if expected_role in _CERTIFICATE_ROLES:
            if not isinstance(row["der_sha256"], str) or _DIGEST.fullmatch(row["der_sha256"]) is None:
                _fail("Edge certificate custody lacks a DER identity")
        elif row["der_sha256"] is not None:
            _fail("Edge non-certificate custody has a DER identity")
        root = _private_binding(row["root"], "Edge credential root copy")
        local = _private_binding(row["local"], "Edge credential local copy")
        if root["sha256"] != local["sha256"]:
            _fail("Edge credential copies differ")
        for item in (root, local):
            if item["path"] in seen_paths or Path(item["path"]).stat().st_size != row["bytes"]:
                _fail("Edge credential custody path or byte count is invalid")
            seen_paths.add(item["path"])
    return checked


def _identity(owner, reservation):
    identity = tuple(owner.get(key) for key in ("run_id", "cell_id", "session_id", "instance_nonce"))
    if identity != tuple(reservation.get(key) for key in ("run_id", "cell_id", "session_id", "instance_nonce")):
        _fail("Edge fixture identity differs from its reservation")
    run_id, cell_id, session_id, nonce = identity
    if (
        not isinstance(run_id, str) or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", run_id) is None
        or cell_id not in EDGE_CELLS
        or not isinstance(session_id, str) or re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}", session_id) is None
        or not isinstance(nonce, str) or _DIGEST.fullmatch(nonce) is None
    ):
        _fail("Edge fixture identity is invalid")
    return identity


def _lanes(owner, reservation):
    lanes = reservation["lanes"]
    if not isinstance(lanes, list) or len(lanes) != 4:
        _fail("Edge lane topology is incomplete")
    primary = _canonical(owner["primary_hub_root"], "primary Hub root")
    expected_roots = (
        primary,
        primary.parent / (owner["cell_id"] + "-edge-reference"),
        primary.parent / (owner["cell_id"] + "-edge-fault"),
        primary.parent / (owner["cell_id"] + "-edge-negative"),
    )
    roots = set()
    checked = []
    for expected, expected_hub_root, lane in zip(LANES, expected_roots, lanes, strict=True):
        _exact(lane, _LANE_FIELDS, "Edge lane")
        observed = tuple(lane[key] for key in ("id", "hub_role", "hub_port", "receiver_port", "delivery_port"))
        if observed != expected or _canonical(lane["hub_root"], "Edge lane Hub root") != expected_hub_root:
            _fail("Edge lane topology is invalid")
        for field in ("hub_root", "producer_root"):
            root = _canonical(lane[field], "Edge lane " + field)
            if root in roots:
                _fail("Edge lane roots alias")
            if root.exists():
                _fail("Edge lane root is not fresh")
            roots.add(root)
        checked.append(dict(lane))
    return checked


def build_edge_fixture_input(owner_config, reservation):
    """Build the exact EdgeFixtureInput from owner config and reservations.

    This function performs no preparation or launch. It binds only already
    created private inputs and runner-reserved roots and ports.
    """
    owner = _exact(owner_config, _OWNER_FIELDS, "Edge owner config")
    reserved = _exact(reservation, _RESERVATION_FIELDS, "Edge reservation")
    if owner["schema_version"] != 1 or owner["kind"] != "edge-fixture-owner-config":
        _fail("Edge owner config identity is invalid")
    if reserved["schema_version"] != 1 or reserved["kind"] != "edge-fixture-reservation":
        _fail("Edge reservation identity is invalid")
    run_id, cell_id, session_id, nonce = _identity(owner, reserved)
    bindings = {}
    for key in (
        "host_registration", "recipe", "producer_registration",
        "launch_inventory",
    ):
        bindings[key] = _private_binding(owner[key], "Edge " + key)
    bindings["vectors"] = _private_binding(
        owner["vectors"], "Edge vectors", private=False,
    )
    bindings["credentials"] = _credential_custody(
        owner["credentials"], cell_id=cell_id, session_id=session_id,
        instance_nonce=nonce,
    )
    lease = _private_binding(reserved["lease"], "Edge lease")
    profile = _exact(owner["edge_profile"], frozenset(("manifest", "members")), "Edge profile")
    if not isinstance(profile["members"], list) or len(profile["members"]) != 19:
        _fail("Edge profile must bind exactly 19 content members")
    edge_profile = {
        "manifest": _private_binding(
            profile["manifest"], "Edge profile manifest", private=False,
        ),
        "members": [
            _private_binding(
                item,
                "Edge profile member",
                private=False,
                maximum=MAX_PUBLIC_PROFILE_MEMBER_BYTES,
            )
            for item in profile["members"]
        ],
    }
    private_root = _canonical(owner["private_root"], "Edge private root")
    if private_root.exists():
        _fail("Edge private root is not fresh")
    lanes = _lanes(owner, reserved)
    return {
        "schema_version": 1, "kind": "edge-installed-fixture",
        "run_id": run_id, "cell_id": cell_id, "session_id": session_id,
        "instance_nonce": nonce, "host_registration": bindings["host_registration"],
        "recipe": bindings["recipe"], "vectors": bindings["vectors"],
        "edge_profile": edge_profile,
        "producer_registration": bindings["producer_registration"],
        "launch_inventory": bindings["launch_inventory"], "lease": lease,
        "private_root": str(private_root), "lanes": lanes,
        "credentials": bindings["credentials"],
    }


def write_edge_fixture_input(path, fixture):
    """Exclusively persist canonical fixture bytes and return their binding."""
    target = _canonical(str(Path(path)), "Edge fixture output")
    raw = (json.dumps(fixture, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False) + "\n").encode("utf-8")
    if len(raw) > MAX_JSON_BYTES:
        _fail("Edge fixture output exceeds the private binding limit")
    target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    target.parent.chmod(0o700)
    try:
        descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    except OSError as error:
        raise EdgeFixtureError("Edge fixture output is not fresh") from error
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        target.chmod(0o600)
    except BaseException:
        target.unlink(missing_ok=True)
        raise
    return {"path": str(target), "sha256": hashlib.sha256(raw).hexdigest()}
