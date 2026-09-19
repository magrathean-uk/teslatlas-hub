# SPDX-License-Identifier: AGPL-3.0-only
"""Closed evidence interface for the root-owned Edge installed runtime.

No function in this module launches a process, selects an executable, or
operates a host. Runtime implementations must produce these independently
bound records before the Edge adapter can admit an installed result.
"""
import hashlib
import re
import stat
from collections.abc import Mapping
from pathlib import Path

from .contract import ContractError, validate_file_binding


_DIGEST = re.compile(r"^[0-9a-f]{64}$")
_FIELDS = frozenset((
    "schema_version", "kind", "run_id", "cell_id", "session_id",
    "instance_nonce", "fixture_sha256", "lanes",
))
_LANE_FIELDS = frozenset((
    "id", "hub_generation", "producer_generation", "namespace", "config",
    "store", "spool", "transport", "witnesses", "cleanup",
))
_LANES = ("primary", "reference", "fault", "negative")


class EdgeRuntimeError(ContractError):
    """Root runtime evidence is missing, mutable, or foreign."""


def _fail(message):
    raise EdgeRuntimeError(message)


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


def _bound(value, label):
    try:
        binding = validate_file_binding(value, label)
    except ContractError as error:
        raise EdgeRuntimeError(str(error)) from error
    path = Path(binding["path"])
    try:
        info = path.lstat()
    except OSError as error:
        raise EdgeRuntimeError(label + " is unavailable") from error
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        _fail(label + " must be a single-link regular file")
    if stat.S_IMODE(info.st_mode) & 0o077:
        _fail(label + " must be owner-only")
    raw = path.read_bytes()
    if len(raw) != info.st_size or hashlib.sha256(raw).hexdigest() != binding["sha256"]:
        _fail(label + " digest changed")
    return dict(binding)


def validate_edge_runtime_evidence(
    value, *, run_id, cell_id, session_id, instance_nonce, fixture_sha256,
):
    """Validate all four runtime lanes without interpreting worker claims."""
    evidence = _exact(value, _FIELDS, "Edge runtime evidence")
    if evidence["schema_version"] != 1 or evidence["kind"] != "edge-installed-runtime-evidence":
        _fail("Edge runtime evidence identity is invalid")
    expected = (run_id, cell_id, session_id, instance_nonce, fixture_sha256)
    observed = tuple(evidence[key] for key in (
        "run_id", "cell_id", "session_id", "instance_nonce", "fixture_sha256",
    ))
    if observed != expected or any(not isinstance(item, str) for item in observed) or _DIGEST.fullmatch(instance_nonce) is None or _DIGEST.fullmatch(fixture_sha256) is None:
        _fail("Edge runtime evidence identity changed")
    lanes = evidence["lanes"]
    if not isinstance(lanes, list) or len(lanes) != 4:
        _fail("Edge runtime lane inventory is incomplete")
    checked_lanes = []
    paths = set()
    for expected_lane, lane in zip(_LANES, lanes, strict=True):
        _exact(lane, _LANE_FIELDS, "Edge runtime lane")
        if lane["id"] != expected_lane:
            _fail("Edge runtime lane order is invalid")
        negative = expected_lane == "negative"
        if lane["producer_generation"] is None:
            _fail("Edge producer generation obligation is invalid")
        if (lane["spool"] is None) != negative:
            _fail("Edge negative lane cannot claim a production spool")
        checked = {"id": expected_lane}
        for key in ("hub_generation", "namespace", "config", "store", "transport", "cleanup"):
            checked[key] = _bound(lane[key], "Edge {} {}".format(expected_lane, key))
        for key in ("producer_generation", "spool"):
            checked[key] = None if lane[key] is None else _bound(lane[key], "Edge {} {}".format(expected_lane, key))
        witnesses = lane["witnesses"]
        if not isinstance(witnesses, list) or not witnesses or len(witnesses) > 32:
            _fail("Edge runtime witnesses are incomplete")
        checked["witnesses"] = [_bound(item, "Edge {} witness".format(expected_lane)) for item in witnesses]
        for binding in [item for key, item in checked.items() if key != "id" and item is not None]:
            for item in binding if isinstance(binding, list) else (binding,):
                if item["path"] in paths:
                    _fail("Edge runtime evidence bindings alias")
                paths.add(item["path"])
        checked_lanes.append(checked)
    return {**dict(evidence), "lanes": checked_lanes}
