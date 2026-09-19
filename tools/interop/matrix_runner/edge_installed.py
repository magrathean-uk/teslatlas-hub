# SPDX-License-Identifier: AGPL-3.0-only
"""Source-fixed Edge installed-adapter boundary.

This module binds the independently reviewed Edge coordinator and its contract
package.  The root installed-host fixture remains the sole authority for
credentials, stores, processes, namespaces, forwards and fault recipes.  Job
JSON can identify evidence inputs, but cannot select code or runtime authority.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import ssl
import stat
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path
from types import MappingProxyType
from typing import Any

from .adapter_wire import (
    LoadedContract,
    ReviewedContract,
    read_bound_file,
    read_safe_file,
    strict_json,
)


WORKSPACE = Path(__file__).resolve().parents[4]
EDGE_ROOT = WORKSPACE / "teslatlas-edge"
PROTOCOL_ROOT = WORKSPACE / "teslatlas-protocol"
CONTRACT_ROOT = EDGE_ROOT / "tools" / "interop" / "client_lanes" / "edge_contract"
COORDINATOR_PATH = EDGE_ROOT / "tools" / "interop" / "client_lanes" / "edge.py"
VECTORS_PATH = CONTRACT_ROOT / "edge-installed-vectors.json"
EDGE_PROFILE_ROOT = PROTOCOL_ROOT / "profiles" / "edge-delivery-v2" / "2.0.0"

PRODUCT_VERSION = "2026.36.2"
EDGE_PROFILE_SHA256 = "e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224"

CONTRACT = ReviewedContract(
    str(CONTRACT_ROOT / "matrix-contract.json"),
    "328c62d724c96011ba06c4d4d1fae6b362dae4e0e740323bf17e8ad3d8cf8915",
    str(CONTRACT_ROOT / "matrix_contract.py"),
    "bf1219b1766e3f2af0f90d45903e961117bcce3275fc2e0424dd6e146851605f",
)

RAW_SCHEMAS = MappingProxyType(
    {
        "edge_fixture_v1": (
            CONTRACT_ROOT / "edge_fixture_v1.schema.json",
            "5f65a6a91e8a580ffe81bb5ee922ef39505ebaa19c232a80603efe822c9c9db6",
        ),
        "edge_checkpoint_v1": (
            CONTRACT_ROOT / "edge_checkpoint_v1.schema.json",
            "859d2a6f90a8c610c6810608e9bf63cf1e0f0670e6e50262c88f7190a31d2f41",
        ),
        "edge_http_v1": (
            CONTRACT_ROOT / "edge_http_v1.schema.json",
            "bb4e3c87df84053fe7a298ca5fc42c99424533b5da83d89372a50dcd75d2be0a",
        ),
        "edge_fault_v1": (
            CONTRACT_ROOT / "edge_fault_v1.schema.json",
            "9eba37bec670157bd9641be14ee8cea446162c4c822ed503f95cf8b2909e4152",
        ),
        "edge_snapshot_v1": (
            CONTRACT_ROOT / "edge_snapshot_v1.schema.json",
            "6279358ff76721ca0b97bba86bfc5573bf83e4dd88779b5c94d375d0ebf7b19e",
        ),
        "edge_cleanup_v1": (
            CONTRACT_ROOT / "edge_cleanup_v1.schema.json",
            "3d6d1fcece6772bb1125c5a79027caab7b2a12726063a291b231317b10ce1ce8",
        ),
    }
)

REVIEWED_SOURCES = MappingProxyType(
    {
        "tools/interop/client_lanes/edge.py":
            "926a0b876a9d06a28474ef4d43ec04d3f7414f0d16c9e25b0e66ff8a86cc19be",
        "tools/interop/client_lanes/edge_contract/matrix-contract.json":
            CONTRACT.manifest_sha256,
        "tools/interop/client_lanes/edge_contract/matrix_contract.py":
            CONTRACT.validator_sha256,
        "tools/interop/client_lanes/edge_contract/phases.json":
            "15000c3bfdc02578426c8484f2cc96f2152f9a89d66cb70a1862ef675a98dbf5",
        "tools/interop/client_lanes/edge_contract/edge-installed-vectors.json":
            "d40a853cd02111ff200083be81588526dc7d2cf28c3daa168d2ea7dd9dda6eb1",
        **{
            "tools/interop/client_lanes/edge_contract/" + path.name: digest
            for path, digest in RAW_SCHEMAS.values()
        },
    }
)

REVIEW_HANDOFFS = MappingProxyType({
    ".superpowers/sdd/2026-09-08-working-product-plan/edge-arm64-coordinator-handoff-2026-09-08.json":
        "ac1215808a8e50393f6c1ee88a76454b06ff5bc36103726541d475a9f35fa996",
    ".superpowers/sdd/2026-09-08-working-product-plan/edge-arm64-coordinator-review-2026-09-08.md":
        "4bfc5274fa8fa7d653dd6d96d416d5cd527943389286cfb10389aca3b1976c8e",
})

ACTOR_IDS = (
    "installed_controller",
    "edge_normal",
    "edge_producer",
    "edge_reference",
    "edge_fault",
    "edge_transport",
)

PHASES = (
    ("edge_normal", "initial", 1, "edge-initial"),
    ("edge_producer", "primary_1", 2, "edge-primary_1"),
    ("edge_reference", "reference_1", 3, "edge-reference_1"),
    ("edge_producer", "primary_2", 4, "edge-primary_2"),
    ("edge_reference", "reference_2", 5, "edge-reference_2"),
    ("edge_producer", "primary_3", 6, "edge-primary_3"),
    ("edge_reference", "reference_3", 7, "edge-reference_3"),
    ("edge_transport", "negative_auth", 8, "edge-negative_auth"),
    ("edge_transport", "negative_v1_404", 9, "edge-negative_v1_404"),
    ("edge_transport", "negative_v1_body", 10, "edge-negative_v1_body"),
    ("edge_transport", "negative_redirect", 11, "edge-negative_redirect"),
    ("edge_transport", "negative_conflict", 12, "edge-negative_conflict"),
    ("edge_fault", "fault_raw_insert", 13, "edge-fault_raw_insert"),
    ("edge_fault", "fault_lifecycle_write", 14, "edge-fault_lifecycle_write"),
    ("edge_fault", "fault_receipt_insert", 15, "edge-fault_receipt_insert"),
    ("edge_fault", "fault_frontier_update", 16, "edge-fault_frontier_update"),
    ("edge_fault", "fault_commit", 17, "edge-fault_commit"),
    ("edge_fault", "fault_before_commit", 18, "edge-fault_before_commit"),
    ("edge_fault", "fault_after_commit", 19, "edge-fault_after_commit"),
    ("edge_fault", "fault_lost_ack", 20, "edge-fault_lost_ack"),
    ("edge_fault", "fault_offline_recovery", 21, "edge-fault_offline_recovery"),
    ("edge_producer", "primary_6", 22, "edge-primary_6"),
    ("edge_reference", "reference_6", 23, "edge-reference_6"),
    ("edge_producer", "primary_8", 24, "edge-primary_8"),
    ("edge_reference", "reference_8", 25, "edge-reference_8"),
    ("edge_producer", "gap_prepare", 26, "edge-gap_prepare"),
    ("edge_normal", "gap_reject", 27, "edge-gap_reject"),
    ("edge_producer", "gap_consume", 28, "edge-gap_consume"),
    ("edge_normal", "resume_stop", 29, "edge-resume_stop"),
    ("edge_normal", "resume_start", 30, "edge-resume_start"),
    ("edge_normal", "actors_closed", 31, "edge-actors_closed"),
)

LANES = (
    ("primary", "installed", 18480, 18500, 18510),
    ("reference", "normal_aux", 18490, 18501, 18511),
    ("fault", "fault_aux", 18491, 18502, 18512),
    ("negative", "normal_aux", 18492, 18503, 18513),
)

EDGE_PROFILE_MEMBERS = (
    "consumer-disposition.schema.json", "delivery.schema.json",
    "examples/ack-prefix.json", "examples/ack-result.json",
    "examples/alert-envelope.json", "examples/batch.json",
    "examples/error-envelope.json", "examples/gap-disposition.json",
    "examples/non-projection-disposition.json", "examples/projected-envelope.json",
    "negative/duplicate-key-ack.json", "negative/non-contiguous-ack.json",
    "negative/oversized-ack.json", "negative/oversized-items-batch.json",
    "negative/stale-ack.json", "openapi.json", "profile.json",
    "receiver-envelope.schema.json", "vectors.json",
)

_FIXTURE_FIELDS = frozenset({
    "schema_version", "kind", "run_id", "cell_id", "session_id",
    "instance_nonce", "host_registration", "recipe", "vectors",
    "edge_profile", "producer_registration", "launch_inventory", "lease",
    "private_root", "lanes", "credentials",
})
_LANE_FIELDS = frozenset({
    "id", "hub_role", "hub_root", "producer_root", "hub_port",
    "receiver_port", "delivery_port",
})
_FORBIDDEN_JOB_AUTHORITY = frozenset({
    "executable", "validator_path", "output_path", "broker_socket",
    "fault_point", "fault_environment", "worker_argv",
})
_SHA256 = re.compile(r"^[0-9a-f]{64}$")


class EdgeInstalledPending(RuntimeError):
    """The reviewed Edge seam cannot admit the supplied root inputs."""


def execution_by_target():
    """The Edge coordinator runs with the root runner for every target."""
    return (
        ("macos_arm64", "local"),
        ("debian13_amd64", "local"),
        ("debian13_arm64", "local"),
    )


def broker_kind_by_target():
    """The reviewed coordinator owns one fixed Unix broker attachment."""
    return tuple((target, "unix") for target, _kind in execution_by_target())


def _canonical(value: Any, label: str) -> Path:
    if not isinstance(value, str) or not value.startswith("/"):
        raise EdgeInstalledPending(label + " path is invalid")
    path = Path(value)
    if str(path.resolve(strict=False)) != value:
        raise EdgeInstalledPending(label + " path is not canonical")
    return path


def _binding(value: Any, label: str, *, private: bool = True) -> bytes:
    if not isinstance(value, Mapping) or set(value) != {"path", "sha256"}:
        raise EdgeInstalledPending(label + " binding shape is invalid")
    try:
        return read_bound_file(value, label=label, maximum=8_388_608, private=private)
    except Exception as error:
        raise EdgeInstalledPending(label + " binding is invalid") from error


def _validate_reviewed_sources() -> None:
    for relative, expected in {**REVIEWED_SOURCES, **REVIEW_HANDOFFS}.items():
        path = EDGE_ROOT / relative
        try:
            observed = hashlib.sha256(path.read_bytes()).hexdigest()
        except OSError as error:
            raise EdgeInstalledPending("reviewed Edge source is unavailable") from error
        if observed != expected:
            raise EdgeInstalledPending("reviewed Edge source changed: " + relative)


def validate_job_inventory(job: Mapping[str, Any], product_version: str) -> None:
    if _FORBIDDEN_JOB_AUTHORITY.intersection(job):
        raise EdgeInstalledPending("job attempts to select Edge adapter authority")
    if job.get("adapter") != "edge_v2" or job.get("cell_id") not in {
        "edge_v2__macos_arm64", "edge_v2__debian13_amd64",
        "edge_v2__debian13_arm64",
    }:
        raise EdgeInstalledPending("Edge job identity is invalid")
    sources = job.get("source_identities")
    if not isinstance(sources, list) or len(sources) != 3:
        raise EdgeInstalledPending("Edge source inventory is incomplete")
    by_role = {item.get("role"): item for item in sources if isinstance(item, Mapping)}
    expected = {
        "hub_source": WORKSPACE / "hub",
        "protocol_source": PROTOCOL_ROOT,
        "edge_source": EDGE_ROOT,
    }
    if set(by_role) != set(expected) or any(
        Path(str(by_role[role].get("repo", ""))).resolve() != root
        for role, root in expected.items()
    ):
        raise EdgeInstalledPending("Edge source inventory is foreign")
    source_fields = {
        "role", "repo", "head", "dirty_patch_sha256",
        "untracked_source_manifest_sha256",
    }
    for source in sources:
        if (
            not isinstance(source, Mapping)
            or set(source) != source_fields
            or not isinstance(source["head"], str)
            or re.fullmatch(r"[0-9a-f]{40,64}", source["head"]) is None
            or any(
                not isinstance(source[field], str)
                or _SHA256.fullmatch(source[field]) is None
                for field in (
                    "dirty_patch_sha256", "untracked_source_manifest_sha256"
                )
            )
        ):
            raise EdgeInstalledPending("Edge source identity is invalid")
    artifacts = job.get("artifacts")
    if not isinstance(artifacts, list) or len(artifacts) != 2:
        raise EdgeInstalledPending("Edge product inventory is incomplete")
    by_role = {item.get("role"): item for item in artifacts if isinstance(item, Mapping)}
    if set(by_role) != {"hub_executable", "edge_executable"}:
        raise EdgeInstalledPending("Edge product inventory is foreign")
    artifact_fields = {"role", "name", "path", "embedded_version", "sha256"}
    for artifact in artifacts:
        if (
            not isinstance(artifact, Mapping)
            or set(artifact) != artifact_fields
            or not isinstance(artifact["name"], str)
            or not artifact["name"]
            or not isinstance(artifact["sha256"], str)
            or _SHA256.fullmatch(artifact["sha256"]) is None
        ):
            raise EdgeInstalledPending("Edge product identity is invalid")
        _canonical(artifact["path"], "Edge product")
    if product_version != PRODUCT_VERSION or any(
        item.get("embedded_version") != product_version for item in by_role.values()
    ):
        raise EdgeInstalledPending("Edge product version is foreign")


def _validate_edge_profile(profile: Any) -> None:
    if not isinstance(profile, Mapping) or set(profile) != {"manifest", "members"}:
        raise EdgeInstalledPending("Edge profile shape is invalid")
    manifest = profile["manifest"]
    if not isinstance(manifest, Mapping) or manifest != {
        "path": str(EDGE_PROFILE_ROOT / "SHA256SUMS"),
        "sha256": EDGE_PROFILE_SHA256,
    }:
        raise EdgeInstalledPending("Edge profile manifest is foreign")
    manifest_raw = _binding(manifest, "Edge profile manifest", private=False)
    declarations = {}
    try:
        for line in manifest_raw.decode("ascii").splitlines():
            digest, name = line.split("  ", 1)
            if not _SHA256.fullmatch(digest) or name in declarations:
                raise ValueError
            declarations[name] = digest
    except (UnicodeError, ValueError) as error:
        raise EdgeInstalledPending("Edge profile manifest syntax is invalid") from error
    if tuple(declarations) != EDGE_PROFILE_MEMBERS:
        raise EdgeInstalledPending("Edge profile member registry is incomplete")
    members = profile["members"]
    if not isinstance(members, list) or len(members) != len(EDGE_PROFILE_MEMBERS):
        raise EdgeInstalledPending("Edge profile member inventory is incomplete")
    for relative, binding in zip(EDGE_PROFILE_MEMBERS, members, strict=True):
        if not isinstance(binding, Mapping) or binding != {
            "path": str(EDGE_PROFILE_ROOT / relative),
            "sha256": declarations[relative],
        }:
            raise EdgeInstalledPending("Edge profile member binding is foreign")
        _binding(binding, "Edge profile member", private=False)


def validate_fixture_input(
    fixture: Mapping[str, Any], *, cell_id: str, session_id: str
) -> None:
    """Validate the exact reviewed root-owned EdgeFixtureInput boundary."""
    if not isinstance(fixture, Mapping) or set(fixture) != _FIXTURE_FIELDS:
        raise EdgeInstalledPending("Edge fixture shape is invalid")
    if (
        fixture["schema_version"] != 1
        or fixture["kind"] != "edge-installed-fixture"
        or fixture["cell_id"] != cell_id
        or fixture["session_id"] != session_id
        or not isinstance(fixture["run_id"], str)
        or not fixture["run_id"]
        or not isinstance(fixture["instance_nonce"], str)
        or not _SHA256.fullmatch(fixture["instance_nonce"])
    ):
        raise EdgeInstalledPending("Edge fixture identity is invalid")
    for field in (
        "host_registration", "recipe", "producer_registration",
        "launch_inventory", "lease", "credentials",
    ):
        _binding(fixture[field], "Edge fixture " + field)
    vectors = fixture["vectors"]
    if vectors != {
        "path": str(VECTORS_PATH),
        "sha256": REVIEWED_SOURCES[
            "tools/interop/client_lanes/edge_contract/edge-installed-vectors.json"
        ],
    }:
        raise EdgeInstalledPending("Edge fixture vectors are foreign")
    _binding(vectors, "Edge fixture vectors", private=False)
    _validate_edge_profile(fixture["edge_profile"])
    _canonical(fixture["private_root"], "Edge fixture private root")
    lanes = fixture["lanes"]
    if not isinstance(lanes, list) or len(lanes) != len(LANES):
        raise EdgeInstalledPending("Edge lane inventory is incomplete")
    roots = set()
    primary_hub_root = None
    for expected, lane in zip(LANES, lanes, strict=True):
        if not isinstance(lane, Mapping) or set(lane) != _LANE_FIELDS:
            raise EdgeInstalledPending("Edge lane shape is invalid")
        observed = tuple(lane[key] for key in (
            "id", "hub_role", "hub_port", "receiver_port", "delivery_port"
        ))
        if observed != expected:
            raise EdgeInstalledPending("Edge lane topology is invalid")
        for key in ("hub_root", "producer_root"):
            root = _canonical(lane[key], "Edge lane " + key)
            if root in roots:
                raise EdgeInstalledPending("Edge lane roots alias")
            roots.add(root)
            if lane["id"] == "primary" and key == "hub_root":
                primary_hub_root = root
    assert primary_hub_root is not None
    if primary_hub_root.name != cell_id:
        raise EdgeInstalledPending("Edge primary lane root is foreign")
    expected_hub_roots = (
        primary_hub_root,
        primary_hub_root.parent / (cell_id + "-edge-reference"),
        primary_hub_root.parent / (cell_id + "-edge-fault"),
        primary_hub_root.parent / (cell_id + "-edge-negative"),
    )
    if tuple(Path(lane["hub_root"]) for lane in lanes) != expected_hub_roots:
        raise EdgeInstalledPending("Edge auxiliary lane roots are foreign")
    credentials = strict_json(_binding(fixture["credentials"], "Edge credentials"))
    if (
        not isinstance(credentials, Mapping)
        or set(credentials) != {
            "schema_version", "session_id", "cell_id", "instance_nonce", "files"
        }
        or credentials["schema_version"] != 1
        or credentials["session_id"] != session_id
        or credentials["cell_id"] != cell_id
        or credentials["instance_nonce"] != fixture["instance_nonce"]
        or not isinstance(credentials["files"], list)
    ):
        raise EdgeInstalledPending("Edge credential inventory is invalid")


def source_entry(adapter_id: str):
    if adapter_id != "edge_v2":
        raise EdgeInstalledPending("Edge adapter identity is foreign")
    _validate_reviewed_sources()
    return CONTRACT


def _json_bytes(value: Mapping[str, Any]) -> bytes:
    return (
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
            allow_nan=False,
        ) + "\n"
    ).encode("utf-8")


def _write_bytes(path: Path, raw: bytes, *, mode: int = 0o600) -> dict[str, str]:
    path = _canonical(str(path), "staged file")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    try:
        descriptor = os.open(
            path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
            mode,
        )
    except OSError as error:
        raise EdgeInstalledPending("staged file path is not fresh") from error
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        path.chmod(mode)
    except BaseException:
        path.unlink(missing_ok=True)
        raise
    return {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}


def _stage(
    root: Path, identifier: str, raw: bytes, *, relative: Path | None = None,
    mode: int = 0o600,
) -> dict[str, Any]:
    leaf = relative if relative is not None else Path(identifier.replace("/", "_") + ".bin")
    return {
        "id": identifier,
        "root": _write_bytes(root / "root" / leaf, raw, mode=mode),
        "local": _write_bytes(root / "local" / leaf, raw, mode=mode),
    }


def _bound_json(value: Any, label: str) -> Mapping[str, Any]:
    parsed = strict_json(_binding(value, label))
    if not isinstance(parsed, Mapping):
        raise EdgeInstalledPending(label + " is not an object")
    return parsed


def _private_stage_root(descriptor: Mapping[str, Any], cell_id: str) -> Path:
    broker = _canonical(str(descriptor.get("broker_socket", "")), "broker socket")
    session_id = descriptor.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        raise EdgeInstalledPending("installed session identity is unavailable")
    root = broker.parent / ("edge-" + cell_id + "-" + session_id)
    try:
        root.mkdir(mode=0o700)
    except OSError as error:
        raise EdgeInstalledPending("Edge staging root is not fresh") from error
    return root


def _http_profile_inputs(profile: Mapping[str, Any], stage: Path):
    root = _canonical(str(profile.get("path", "")), "HTTP profile root")
    manifest_path = root / "SHA256SUMS"
    manifest_raw = read_safe_file(
        manifest_path, label="HTTP profile manifest", maximum=1_048_576,
        private=False,
    )
    if hashlib.sha256(manifest_raw).hexdigest() != profile.get("sha256"):
        raise EdgeInstalledPending("HTTP profile manifest digest changed")
    try:
        declarations = [line.split("  ", 1) for line in manifest_raw.decode("ascii").splitlines()]
    except (UnicodeError, ValueError) as error:
        raise EdgeInstalledPending("HTTP profile manifest syntax is invalid") from error
    if len(declarations) != 17 or len({name for _digest, name in declarations}) != 17:
        raise EdgeInstalledPending("HTTP profile member inventory is incomplete")
    manifest = _stage(
        stage, "profile_SHA256SUMS", manifest_raw,
        relative=Path("hub-http-v1") / "1.0.0" / "SHA256SUMS",
    )
    members = [manifest]
    for expected, relative in declarations:
        if not _SHA256.fullmatch(expected) or relative.startswith("/") or ".." in Path(relative).parts:
            raise EdgeInstalledPending("HTTP profile manifest member is invalid")
        raw = read_safe_file(
            root / relative, label="HTTP profile member", maximum=1_048_576,
            private=False,
        )
        if hashlib.sha256(raw).hexdigest() != expected:
            raise EdgeInstalledPending("HTTP profile member digest changed")
        members.append(_stage(
            stage, "profile_" + relative.replace("/", "_").replace(".", "_"), raw,
            relative=Path("hub-http-v1") / "1.0.0" / relative,
        ))
    return manifest, members


def _artifact(job: Mapping[str, Any], role: str) -> tuple[Mapping[str, Any], bytes]:
    selected = [item for item in job.get("artifacts", []) if item.get("role") == role]
    if len(selected) != 1:
        raise EdgeInstalledPending("Edge artifact inventory is incomplete")
    item = selected[0]
    path = _canonical(str(item.get("path", "")), role)
    raw = read_safe_file(path, label=role, maximum=512 * 1024 * 1024, private=False)
    if hashlib.sha256(raw).hexdigest() != item.get("sha256"):
        raise EdgeInstalledPending(role + " digest changed")
    return item, raw


def _manifest(
    stage: Path, identifier: str, *, artifact: Mapping[str, Any] | None = None,
    raw: bytes | None = None, build_record: Mapping[str, Any] | None = None,
    files: list[Mapping[str, Any]] | None = None,
) -> dict[str, Any]:
    if artifact is not None:
        assert raw is not None
        value = {
            "schema_version": 1,
            "artifact_sha256": artifact["sha256"],
            "files": [{
                "path": artifact["name"], "bytes": len(raw), "mode": 0o500,
                "sha256": artifact["sha256"],
            }],
        }
    else:
        assert build_record is not None and files
        value = {"schema_version": 1, "build_record": dict(build_record), "files": files}
    return _stage(stage, identifier, _json_bytes(value), relative=Path(identifier + ".json"))


def _stage_reviewed_sources(stage: Path):
    records = []
    staged_root = stage / "source"
    for relative, expected in REVIEWED_SOURCES.items():
        raw = read_safe_file(
            EDGE_ROOT / relative, label="reviewed Edge source", maximum=8_388_608,
            private=False,
        )
        if hashlib.sha256(raw).hexdigest() != expected:
            raise EdgeInstalledPending("reviewed Edge source changed: " + relative)
        target = staged_root / relative
        _write_bytes(target, raw, mode=0o400)
        records.append({
            "path": relative, "bytes": len(raw), "mode": 0o400, "sha256": expected,
        })
    return staged_root, sorted(records, key=lambda item: item["path"])


def _stage_edge_resources(
    source_root: Path, fixture: Mapping[str, Any], records: list[Mapping[str, Any]]
) -> list[Mapping[str, Any]]:
    resources = [
        ("edge_profile/SHA256SUMS", fixture["edge_profile"]["manifest"]),
        *[
            ("edge_profile/" + relative, binding)
            for relative, binding in zip(
                EDGE_PROFILE_MEMBERS, fixture["edge_profile"]["members"], strict=True
            )
        ],
        ("edge_contract/edge-installed-vectors.json", fixture["vectors"]),
    ]
    result = list(records)
    for relative, binding in resources:
        raw = _binding(binding, "Edge actor resource", private=False)
        _write_bytes(source_root / relative, raw, mode=0o400)
        result.append({
            "path": relative, "bytes": len(raw), "mode": 0o400,
            "sha256": hashlib.sha256(raw).hexdigest(),
        })
    return sorted(result, key=lambda item: item["path"])


def build_session_input(
    job, cell, config, matrix, contract: LoadedContract, descriptor, running,
    *, deadline=None,
):
    """Stage the common SessionInput from root-owned Edge fixture evidence."""
    del matrix
    if deadline is not None:
        deadline.remaining()
    _validate_reviewed_sources()
    validate_job_inventory(job, config.get("product_version"))
    if (
        cell.get("id") != job.get("cell_id")
        or cell.get("client_id") != "edge_v2"
        or contract.manifest.get("adapter_id") != "edge_v2"
        or not isinstance(running, Mapping)
        or not isinstance(running.get("descriptor"), Mapping)
    ):
        raise EdgeInstalledPending("Edge installed runner identity is unavailable")
    session_config = _bound_json(
        job.get("installed_session", {}).get("config"), "installed session config"
    )
    if (
        session_config.get("schema_version") != 2
        or set(session_config) != {"schema_version", "run_id", "edge_fixture", "scenario"}
    ):
        raise EdgeInstalledPending("root-owned Edge session v2 is unavailable")
    fixture = _bound_json(session_config["edge_fixture"], "Edge fixture input")
    validate_fixture_input(
        fixture, cell_id=cell["id"], session_id=descriptor.get("session_id")
    )
    if fixture["run_id"] != session_config["run_id"]:
        raise EdgeInstalledPending("Edge fixture run identity changed")
    stage = _private_stage_root(descriptor, cell["id"])
    source_root, source_records = _stage_reviewed_sources(stage)
    source_records = _stage_edge_resources(source_root, fixture, source_records)
    profile_manifest, profile_members = _http_profile_inputs(
        config["profile"], stage / "inputs"
    )
    scenario_raw = _binding(session_config["scenario"], "Edge scenario")
    scenario = _stage(stage / "inputs", "scenario", scenario_raw, relative=Path("scenario.json"))
    certificate_path = _canonical(
        str(running["descriptor"].get("certificate_path", "")), "Hub certificate"
    )
    certificate_raw = read_safe_file(
        certificate_path, label="Hub certificate", maximum=65_536, private=True
    )
    certificate = _stage(
        stage / "inputs", "certificate", certificate_raw,
        relative=Path("certificate.pem"),
    )
    try:
        der = ssl.PEM_cert_to_DER_cert(certificate_raw.decode("ascii"))
        certificate_der_sha256 = hashlib.sha256(
            bytes.fromhex(der) if isinstance(der, str) else der
        ).hexdigest()
    except (UnicodeError, ValueError, ssl.SSLError) as error:
        raise EdgeInstalledPending("Hub certificate is not PEM") from error

    edge_artifact, edge_raw = _artifact(job, "edge_executable")
    hub_artifact, hub_raw = _artifact(job, "hub_executable")
    edge_product = _stage(
        stage / "products", "edge_executable", edge_raw,
        relative=Path("edge") / edge_artifact["name"], mode=0o500,
    )
    edge_manifest = _manifest(
        stage / "actors", "edge_executable_manifest",
        artifact=edge_artifact, raw=edge_raw,
    )
    hub_manifest = _manifest(
        stage / "actors", "hub_executable_manifest",
        artifact=hub_artifact, raw=hub_raw,
    )
    source_manifest = _manifest(
        stage / "actors", "edge_source_manifest",
        build_record=fixture["launch_inventory"], files=source_records,
    )
    phase_raw = read_safe_file(
        CONTRACT_ROOT / "phases.json", label="reviewed Edge phases",
        maximum=1_048_576, private=False,
    )
    phase_contract = _stage(
        stage / "inputs", "edge_phase_contract", phase_raw,
        relative=Path("phases.json"),
    )
    manifest_by_actor = {
        "installed_controller": hub_manifest,
        "edge_normal": source_manifest,
        "edge_producer": edge_manifest,
        "edge_reference": hub_manifest,
        "edge_fault": source_manifest,
        "edge_transport": source_manifest,
    }
    actor_contract = {
        "installed_controller": ("installed_controller", "coordinator", "root_controller", ["hub_executable"], ["hub_source", "protocol_source"], "installed_controller"),
        "edge_normal": ("edge_normal", "coordinator", "root_python", [], ["hub_source", "protocol_source"], "edge_matrix"),
        "edge_producer": ("edge_normal", "docker_worker", "edge_linux", ["edge_executable"], ["edge_source", "protocol_source"], "edge_producer"),
        "edge_reference": ("edge_normal", "local_worker", "hub_target_aux", ["hub_executable"], ["hub_source", "protocol_source"], "edge_reference"),
        "edge_fault": ("edge_fault", "local_worker", "hub_target_aux", [], ["hub_source", "protocol_source"], "edge_fault"),
        "edge_transport": ("edge_fault", "docker_worker", "edge_linux", [], ["hub_source", "protocol_source"], "edge_negative_transport"),
    }
    actors = []
    for actor_id in ACTOR_IDS:
        kind, execution, runtime_ref, artifacts, sources, entrypoint = actor_contract[actor_id]
        actors.append({
            "id": actor_id, "kind": kind, "execution": execution,
            "runtime_ref": runtime_ref, "artifact_roles": artifacts,
            "source_roles": sources, "entrypoint_ref": entrypoint,
            "input_manifest": manifest_by_actor[actor_id],
            "phase_contract": None if actor_id == "installed_controller" else phase_contract,
        })
    header = {
        "schema_version": 1, "execution_kind": "actual_hub_acceptance",
        "adapter": "edge_v2", "cell_id": cell["id"],
        "product_version": config["product_version"],
        "profile_id": config["profile"]["id"],
        "profile_revision": config["profile"]["revision"],
        "profile_sha256": config["profile"]["sha256"],
        "source_identities": job["source_identities"],
        "artifacts": job["artifacts"], "runtime": job["runtime"],
    }
    header_stage = _stage(
        stage / "inputs", "header", _json_bytes(header), relative=Path("header.json")
    )
    contract_raw = read_safe_file(
        CONTRACT_ROOT / "matrix-contract.json", label="reviewed Edge contract",
        maximum=1_048_576, private=False,
    )
    contract_stage = _stage(
        stage / "inputs", "case_contract", contract_raw,
        relative=Path("matrix-contract.json"),
    )
    output_root = stage / "outputs"
    output_root.mkdir(mode=0o700)
    outputs = {
        "normalized": str(output_root / "normalized.json"),
        "actor_evidence": str(output_root / "actor-evidence.json"),
        "coordination_dir": str(output_root / "coordination"),
        "framework_log": str(output_root / "framework.log"),
    }
    session = {
        "schema_version": 1, "kind": "matrix-adapter-session",
        "run_id": session_config["run_id"], "cell_id": cell["id"],
        "adapter_id": "edge_v2", "client_id": "edge_v2",
        "session_id": descriptor["session_id"],
        "instance_nonce": fixture["instance_nonce"],
        "header": header_stage, "case_contract": contract_stage,
        "host_session": descriptor,
        "broker": {"kind": "unix", "socket_path": descriptor["broker_socket"]},
        "inputs": {
            "profile_manifest": profile_manifest, "profile_members": profile_members,
            "scenario": scenario, "certificate": certificate,
            "certificate_der_sha256": certificate_der_sha256,
            "product_inputs": [{
                "artifact_role": "edge_executable", "staged": edge_product,
                "installed_manifest": edge_manifest,
                "local_root": str(stage / "products" / "local" / "edge"),
            }],
        },
        "actors": actors, "outputs": outputs,
        "bounds": {
            "cell_timeout_ms": job["timeout_seconds"] * 1000,
            "cleanup_timeout_ms": 45_000, "frame_bytes": 1_048_576,
            "evidence_bytes": 8_388_608, "framework_log_bytes": 8_388_608,
        },
    }
    path = stage / "session-input.json"
    _write_bytes(path, _json_bytes(session))
    if deadline is not None:
        deadline.remaining()
    # The staged coordinator is deliberately retained only inside the private
    # run tree. launch_adapter derives it from this SessionInput path.
    expected_source = stage / "source" / "tools" / "interop" / "client_lanes" / "edge.py"
    if source_root / "tools" / "interop" / "client_lanes" / "edge.py" != expected_source:
        raise EdgeInstalledPending("staged Edge coordinator layout changed")
    return path, session


def _validate_staged_coordinator(session_input_path: Path | str) -> Path:
    session_path = _canonical(str(Path(session_input_path)), "Edge SessionInput")
    read_safe_file(
        session_path, label="Edge SessionInput", maximum=1_048_576, private=True
    )
    source_root = session_path.parent / "source"
    for relative, expected in REVIEWED_SOURCES.items():
        path = source_root / relative
        raw = read_safe_file(
            path, label="staged Edge source", maximum=8_388_608, private=True
        )
        if (
            hashlib.sha256(raw).hexdigest() != expected
            or stat.S_IMODE(path.stat().st_mode) != 0o400
        ):
            raise EdgeInstalledPending("staged Edge source changed: " + relative)
    return source_root / "tools" / "interop" / "client_lanes" / "edge.py"


def launch_adapter(
    job, cell, config, contract, session_input_path, session_input,
    *, deadline=None, session=None, running=None,
):
    """Launch only the staged, hash-bound Edge coordinator."""
    del cell, contract, session, running
    validate_job_inventory(job, config.get("product_version"))
    if deadline is not None:
        deadline.remaining()
    coordinator = _validate_staged_coordinator(session_input_path)
    if session_input.get("adapter_id") != "edge_v2":
        raise EdgeInstalledPending("Edge SessionInput identity is foreign")
    framework = _canonical(
        str(session_input.get("outputs", {}).get("framework_log", "")),
        "Edge framework log",
    )
    stderr_path = Path(str(framework) + ".stderr.log")
    framework.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        stdout_descriptor = os.open(
            framework, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
        try:
            stderr_descriptor = os.open(
                stderr_path,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
                0o600,
            )
        except BaseException:
            os.close(stdout_descriptor)
            framework.unlink(missing_ok=True)
            raise
    except OSError as error:
        raise EdgeInstalledPending("Edge framework logs are not fresh") from error
    stdout_handle = os.fdopen(stdout_descriptor, "wb")
    stderr_handle = os.fdopen(stderr_descriptor, "wb")
    try:
        process = subprocess.Popen(
            [sys.executable, str(coordinator), str(Path(session_input_path))],
            cwd=str(coordinator.parent), stdin=subprocess.DEVNULL,
            stdout=stdout_handle, stderr=stderr_handle,
            env={
                "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                "PYTHONDONTWRITEBYTECODE": "1",
            },
            start_new_session=True, close_fds=True,
        )
    finally:
        stdout_handle.close()
        stderr_handle.close()
    if deadline is not None:
        deadline.remaining()
    return process


def _runtime_evidence_pending(*_args, **_kwargs):
    # The accepted fixture design requires independently root-observed process,
    # namespace, store, transport, witness and cleanup bindings. The current
    # installed-host implementation publishes none of those Edge-only records.
    # Returning job expectations or child success booleans here would fabricate
    # installed evidence and let auxiliary actors impersonate the package.
    raise EdgeInstalledPending(
        "root-owned Edge runtime evidence interface is unavailable"
    )


def runtime_inventory(job, cell, config, contract, session_input, deadline=None):
    """Require the pending root actor-runtime observation interface."""
    return _runtime_evidence_pending(
        job, cell, config, contract, session_input, deadline=deadline
    )


def admit(
    job, cell, config, matrix, contract, normalized, actors, *,
    admission_views=None, runtime_context=None, deadline=None,
):
    """Keep case admission closed until root actor evidence can be cross-bound."""
    return _runtime_evidence_pending(
        job, cell, config, matrix, contract, normalized, actors,
        admission_views=admission_views, runtime_context=runtime_context,
        deadline=deadline,
    )


def build_supplement(
    job, cell, config, matrix, contract, result, *, runtime_context=None,
):
    """Keep the v2 supplement closed until root Edge evidence is admitted."""
    return _runtime_evidence_pending(
        job, cell, config, matrix, contract, result,
        runtime_context=runtime_context,
    )


def execution_logs(job, cell, config, contract, result):
    """Keep execution receipts closed with the same root evidence prerequisite."""
    return _runtime_evidence_pending(job, cell, config, contract, result)
