# SPDX-License-Identifier: AGPL-3.0-only
"""Source-fixed construction and launch seam for installed Viewer sessions.

The reviewed Viewer coordinator and the two admitted package archives are
fixed in Hub source.  Matrix job JSON supplies evidence inputs only; it cannot
select code, validators, commands, brokers, or output paths.  The current
Viewer coordinator still emits semantic placeholders, so this module keeps
admission fail-closed until real per-case actor evidence exists.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import ssl
import stat
import subprocess
import tarfile
from types import MappingProxyType
from typing import Any, Mapping

from .adapter_wire import (
    LoadedContract,
    ReviewedContract,
    WireError,
    read_bound_file,
    strict_json,
    validate_viewer_session_contract,
)


WORKSPACE = Path(__file__).resolve().parents[4]
VIEWER_ROOT = WORKSPACE / "teslatlas-viewer"
SDK_ROOT = WORKSPACE / "teslatlas-sdk-typescript"
PROTOCOL_ROOT = WORKSPACE / "teslatlas-protocol"

CONTRACT = ReviewedContract(
    str(VIEWER_ROOT / "tools/matrix-contract.json"),
    "ee235f62f2aa8300ee88d2632c16eb63f2eaeb837c2bf46e8f4211b305fd425c",
    str(VIEWER_ROOT / "tools/matrix_contract.py"),
    "4ef035ef2ee376dca3d9292df5db6ff1cb4b85f935cb85bc94e6056229b852c1",
)

COORDINATOR = VIEWER_ROOT / "tools/matrix-live.mjs"
NODE_EXECUTABLE = Path(
    "/Users/bolyki/.codex/artifacts/teslatlas-interop/2026-09-05-execution/"
    "tooling/node-v26.7.0-darwin-arm64/bin/node"
)
NODE_EXECUTABLE_SHA256 = "a9bd0630891c2dcdee70de88270fee2cc0c4a9e76495039dd3b4f91c5e6b71df"

# These are the source files directly selected by the reviewed coordinator.
# node_modules remains a runtime inventory gap and is never inferred from the
# package lock or a successful Playwright exit.
REVIEWED_VIEWER_SOURCES = MappingProxyType({
    "tools/matrix-live.mjs":
        "f6f09e6d9bca4dcb03c407085213275fec6b51dc986ddc7b9ea3aca491cd65b0",
    "bin/teslatlas-viewer.mjs":
        "57cf13e85d4fa8966aa36153265cb8e4329e9670f0ce0a880e5acfc81fab9895",
    "playwright.hub.config.ts":
        "5f79e34a972a1a6dbcc138a37dbb637bd6ef3d9c50216b5576206a656a1a7bc0",
    "e2e/installed-hub.spec.ts":
        "19bd789cad8a6c8eb42db68d28a454c4cb0ea707b24eac61d2a3795184350f44",
    "package.json":
        "7f1f2759e70a237bc09b106d4d2fe13b62d9f93b8685e2a7e0d6cecabb1f6c15",
    "package-lock.json":
        "fcd510bdf27796c807e8c019ae0a5b5e0eef8f1751539ae9524281cfc40eb72e",
})

SDK_ARCHIVE = Path(
    "/Users/bolyki/.codex/artifacts/teslatlas-interop/2026-09-08-working-product/"
    "sdk-m1-node-26.7.0-pack/teslatlas-sdk-2026.36.2.tgz"
)
SDK_ARCHIVE_SHA256 = "03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c"
SDK_MEMBER_COUNT = 81
VIEWER_ARCHIVE = Path(
    "/Users/bolyki/.codex/artifacts/teslatlas-interop/"
    "2026-09-05-task10a-viewer-sdk-reconcile/pack/teslatlas-viewer-2026.36.2.tgz"
)
VIEWER_ARCHIVE_SHA256 = "6f70b4b7a41e69cc6867b205f85d416fa08c70bcb803f5f5a498275f7a8d8538"
VIEWER_MEMBERS = (
    "LICENSE", "README.md", "bin/teslatlas-viewer.mjs",
    "bin/verify-source-install.mjs", "dist/assets/index-Bu38ncPX.js",
    "dist/assets/index-DKdVZc9w.css", "dist/index.html",
    "dist/version.json", "package.json",
)
PROFILE_MEMBERS = (
    "SHA256SUMS", "auth.schema.json", "cases.json", "discovery.schema.json",
    "errors.schema.json", "examples/claim.json", "examples/current.json",
    "examples/discovery.json", "examples/drives.json", "examples/health.json",
    "examples/invitation.json", "examples/ready.json", "examples/vehicles.json",
    "field-semantics.json", "openapi.json", "profile.json",
    "resources.schema.json", "sync-regression.json",
)

ENVIRONMENT_KEYS = frozenset({
    "TESLATLAS_VIEWER_PACKAGE_ROOT", "TESLATLAS_VIEWER_PACKAGE_MANIFEST",
    "TESLATLAS_VIEWER_SDK_ROOT", "TESLATLAS_VIEWER_SDK_MANIFEST",
    "TESLATLAS_VIEWER_PAGE_ORIGIN", "TESLATLAS_VIEWER_BROWSER_EXECUTABLE",
    "TESLATLAS_VIEWER_BROWSER_SHA256", "TESLATLAS_VIEWER_BROWSER_ENGINE",
    "TESLATLAS_VIEWER_BROWSER_VERSION",
})
FORBIDDEN_JOB_AUTHORITY = frozenset({
    "executable", "validator", "validator_path", "output", "output_path",
    "broker", "broker_socket",
})


class ViewerInstalledPending(RuntimeError):
    """The fixed Viewer seam cannot admit the supplied source or evidence."""


def execution_by_target():
    """The reviewed Viewer coordinator runs beside the Hub matrix runner."""
    return (
        ("macos_arm64", "local"),
        ("debian13_amd64", "local"),
        ("debian13_arm64", "local"),
    )


def broker_kind_by_target():
    """Every Viewer target uses the runner-owned Unix controller broker."""
    return (
        ("macos_arm64", "unix"),
        ("debian13_amd64", "unix"),
        ("debian13_arm64", "unix"),
    )


def _digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _canonical(path: Path, label: str) -> Path:
    if not path.is_absolute() or path.resolve(strict=False) != path:
        raise ViewerInstalledPending(label + " path is not canonical")
    return path


def _read_regular(
    path: Path, label: str, maximum: int = 8_388_608, *, private: bool = True,
) -> bytes:
    path = _canonical(path, label)
    try:
        before = path.lstat()
    except OSError as error:
        raise ViewerInstalledPending(label + " is unavailable") from error
    if (
        not stat.S_ISREG(before.st_mode) or before.st_nlink != 1
        or before.st_uid != os.getuid() or before.st_size > maximum
        or (private and stat.S_IMODE(before.st_mode) & 0o077)
    ):
        raise ViewerInstalledPending(label + " is not an admissible regular file")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(path, flags)
        try:
            opened = os.fstat(descriptor)
            if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                raise ViewerInstalledPending(label + " changed before reading")
            chunks = bytearray()
            while len(chunks) <= maximum:
                part = os.read(descriptor, min(65_536, maximum + 1 - len(chunks)))
                if not part:
                    break
                chunks.extend(part)
            after = os.fstat(descriptor)
        finally:
            os.close(descriptor)
        final = path.lstat()
    except ViewerInstalledPending:
        raise
    except OSError as error:
        raise ViewerInstalledPending(label + " cannot be read") from error
    identity = (before.st_dev, before.st_ino, before.st_mtime_ns, before.st_size)
    if (
        len(chunks) > maximum or len(chunks) != before.st_size
        or identity != (after.st_dev, after.st_ino, after.st_mtime_ns, after.st_size)
        or identity != (final.st_dev, final.st_ino, final.st_mtime_ns, final.st_size)
    ):
        raise ViewerInstalledPending(label + " changed while reading")
    return bytes(chunks)


def _json_bytes(value: Mapping[str, Any]) -> bytes:
    try:
        text = json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
            allow_nan=False,
        )
    except (TypeError, ValueError) as error:
        raise ViewerInstalledPending("staged JSON is invalid") from error
    return (text + "\n").encode("utf-8")


def _write_bytes(path: Path, raw: bytes, *, mode: int = 0o600) -> Mapping[str, str]:
    path = _canonical(path, "staged file")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    try:
        descriptor = os.open(
            path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), mode,
        )
    except OSError as error:
        raise ViewerInstalledPending("staged file path is not fresh") from error
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        try:
            path.unlink()
        except OSError:
            pass
        raise
    return {"path": str(path), "sha256": _digest(raw)}


def _stage(root: Path, identifier: str, raw: bytes, suffix: str = ".bin") -> Mapping[str, Any]:
    leaf = identifier.replace("/", "_") + suffix
    return {
        "id": identifier,
        "root": _write_bytes(root / "root" / leaf, raw),
        "local": _write_bytes(root / "local" / leaf, raw),
    }


def _bound_json(binding: Mapping[str, Any], label: str) -> Mapping[str, Any]:
    try:
        value = strict_json(read_bound_file(binding, label=label, maximum=1_048_576))
    except Exception as error:
        raise ViewerInstalledPending(label + " is invalid") from error
    if not isinstance(value, Mapping):
        raise ViewerInstalledPending(label + " is not an object")
    return value


def _path_json(path: Path, label: str) -> Mapping[str, Any]:
    try:
        value = strict_json(_read_regular(path, label, 1_048_576))
    except Exception as error:
        raise ViewerInstalledPending(label + " is invalid") from error
    if not isinstance(value, Mapping):
        raise ViewerInstalledPending(label + " is not an object")
    return value


def _archive_members(archive: Path, expected_digest: str, expected_count: int) -> tuple[tuple[str, int], ...]:
    raw = _read_regular(archive, "product archive", 1_073_741_824, private=False)
    if _digest(raw) != expected_digest:
        raise ViewerInstalledPending("product archive digest changed")
    try:
        with tarfile.open(archive, "r:gz") as package:
            members = []
            for item in package.getmembers():
                if item.isfile():
                    if not item.name.startswith("package/"):
                        raise ViewerInstalledPending("product archive member path is invalid")
                    relative = item.name.removeprefix("package/")
                    if not relative or Path(relative).is_absolute() or ".." in Path(relative).parts:
                        raise ViewerInstalledPending("product archive member path is invalid")
                    members.append((relative, item.mode & 0o777))
                elif not item.isdir():
                    raise ViewerInstalledPending("product archive contains a non-regular member")
    except (OSError, tarfile.TarError) as error:
        raise ViewerInstalledPending("product archive inventory is invalid") from error
    members.sort()
    if len(members) != expected_count or len({name for name, _mode in members}) != len(members):
        raise ViewerInstalledPending("product archive member inventory differs")
    return tuple(members)


def installed_product_inventory(
    root: Path, archive: Path, artifact_sha256: str, product: str,
) -> Mapping[str, Any]:
    """Rehash a complete installed package tree against its pinned archive."""
    if product == "viewer":
        expected_digest, count = VIEWER_ARCHIVE_SHA256, len(VIEWER_MEMBERS)
    elif product == "sdk":
        expected_digest, count = SDK_ARCHIVE_SHA256, SDK_MEMBER_COUNT
    else:
        raise ViewerInstalledPending("installed product identity is invalid")
    if artifact_sha256 != expected_digest:
        raise ViewerInstalledPending("installed product artifact is foreign")
    archive_members = _archive_members(archive, expected_digest, count)
    names = tuple(name for name, _mode in archive_members)
    if product == "viewer" and names != VIEWER_MEMBERS:
        raise ViewerInstalledPending("Viewer archive inventory differs from review")
    root = _canonical(root, "installed product root")
    try:
        actual_names = tuple(sorted(
            str(path.relative_to(root)) for path in root.rglob("*")
            if path.is_file() and not path.is_symlink()
        ))
    except OSError as error:
        raise ViewerInstalledPending("installed product cannot be enumerated") from error
    if actual_names != names:
        raise ViewerInstalledPending("installed product member inventory differs")
    files = []
    mode_by_name = dict(archive_members)
    for name in names:
        path = root / name
        raw = _read_regular(path, "installed product member", 16_777_216, private=False)
        metadata = path.lstat()
        if stat.S_IMODE(metadata.st_mode) != mode_by_name[name]:
            raise ViewerInstalledPending("installed product member mode differs")
        files.append({
            "path": name, "bytes": len(raw), "mode": stat.S_IMODE(metadata.st_mode),
            "sha256": _digest(raw),
        })
    return {"schema_version": 1, "artifact_sha256": artifact_sha256, "files": files}


def validate_installed_product(
    root: Path, artifact: Mapping[str, Any], inventory: Mapping[str, Any],
) -> Mapping[str, Any]:
    role = artifact.get("role")
    if role == "viewer_package_tarball":
        archive, product = VIEWER_ARCHIVE, "viewer"
    elif role == "typescript_sdk_tarball":
        archive, product = SDK_ARCHIVE, "sdk"
    else:
        raise ViewerInstalledPending("installed product role is foreign")
    observed = installed_product_inventory(root, archive, artifact.get("sha256"), product)
    if inventory != observed:
        raise ViewerInstalledPending("installed product manifest differs")
    return observed


def _reviewed_sources() -> None:
    for relative, expected in REVIEWED_VIEWER_SOURCES.items():
        raw = _read_regular(VIEWER_ROOT / relative, "reviewed Viewer source", 2_097_152, private=False)
        if _digest(raw) != expected:
            raise ViewerInstalledPending("pending: reviewed Viewer source changed")
    node = _read_regular(NODE_EXECUTABLE, "pinned Node executable", 200_000_000, private=False)
    if _digest(node) != NODE_EXECUTABLE_SHA256:
        raise ViewerInstalledPending("pending: pinned Node executable changed")


def validate_job_inventory(job: Mapping[str, Any]) -> Mapping[str, Mapping[str, Any]]:
    if FORBIDDEN_JOB_AUTHORITY.intersection(job):
        raise ViewerInstalledPending("job attempts to select Viewer adapter authority")
    sources = job.get("source_identities")
    if not isinstance(sources, list) or len(sources) != 4:
        raise ViewerInstalledPending("Viewer source inventory is incomplete")
    by_source = {item.get("role"): item for item in sources if isinstance(item, Mapping)}
    expected_sources = {
        "hub_source": WORKSPACE / "hub", "protocol_source": PROTOCOL_ROOT,
        "typescript_sdk_source": SDK_ROOT, "viewer_source": VIEWER_ROOT,
    }
    if set(by_source) != set(expected_sources):
        raise ViewerInstalledPending("Viewer source inventory is foreign")
    for role, expected in expected_sources.items():
        if Path(str(by_source[role].get("repo", ""))).resolve() != expected:
            raise ViewerInstalledPending("Viewer source checkout is foreign")

    artifacts = job.get("artifacts")
    if not isinstance(artifacts, list) or len(artifacts) != 3:
        raise ViewerInstalledPending("Viewer reviewed product inventory is incomplete")
    by_role = {item.get("role"): item for item in artifacts if isinstance(item, Mapping)}
    if set(by_role) != {"hub_executable", "typescript_sdk_tarball", "viewer_package_tarball"}:
        raise ViewerInstalledPending("Viewer reviewed product inventory is foreign")
    expected = {
        "typescript_sdk_tarball": (SDK_ARCHIVE, SDK_ARCHIVE_SHA256),
        "viewer_package_tarball": (VIEWER_ARCHIVE, VIEWER_ARCHIVE_SHA256),
    }
    for role, (path, digest) in expected.items():
        artifact = by_role[role]
        if (
            Path(str(artifact.get("path", ""))).resolve() != path
            or artifact.get("sha256") != digest
            or artifact.get("embedded_version") != "2026.36.2"
        ):
            raise ViewerInstalledPending("Viewer reviewed product inventory is foreign")
        raw = _read_regular(path, "Viewer product archive", 1_073_741_824, private=False)
        if _digest(raw) != digest:
            raise ViewerInstalledPending("Viewer reviewed product inventory digest changed")
    if by_role["hub_executable"].get("embedded_version") != "2026.36.2":
        raise ViewerInstalledPending("Hub artifact version is foreign")
    _reviewed_sources()
    return MappingProxyType(by_role)


def _profile_inputs(profile: Mapping[str, Any], stage: Path):
    root = _canonical(Path(str(profile.get("path", ""))), "profile root")
    manifest_raw = _read_regular(root / "SHA256SUMS", "profile manifest", 1_048_576, private=False)
    if _digest(manifest_raw) != profile.get("sha256"):
        raise ViewerInstalledPending("profile manifest digest changed")
    try:
        checksums = dict(
            line.split("  ", 1) for line in manifest_raw.decode("ascii").splitlines()
        )
    except (UnicodeError, ValueError) as error:
        raise ViewerInstalledPending("profile manifest syntax is invalid") from error
    # SHA256SUMS uses digest then member name; reverse the temporary mapping.
    checksums = {name: digest_value for digest_value, name in checksums.items()}
    if set(checksums) != set(PROFILE_MEMBERS) - {"SHA256SUMS"}:
        raise ViewerInstalledPending("profile member set is incomplete")
    members = []
    for name in PROFILE_MEMBERS:
        raw = manifest_raw if name == "SHA256SUMS" else _read_regular(
            root / name, "profile member", 1_048_576, private=False,
        )
        if name != "SHA256SUMS" and _digest(raw) != checksums[name]:
            raise ViewerInstalledPending("profile member digest differs from manifest")
        members.append(_stage(stage, "profile_" + name.replace("/", "_"), raw, ".json"))
    return members[0], members


def _private_root(descriptor: Mapping[str, Any], cell_id: str) -> Path:
    broker = _canonical(Path(str(descriptor.get("broker_socket", ""))), "broker socket")
    session_id = descriptor.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        raise ViewerInstalledPending("installed session identity is unavailable")
    root = broker.parent / ("viewer-" + cell_id + "-" + session_id)
    root.mkdir(mode=0o700, exist_ok=False)
    return root


def build_session_input(
    job, cell, config, matrix, contract: LoadedContract, descriptor, running,
    deadline=None,
):
    """Stage a closed Viewer SessionInput from reviewed source and products."""
    del matrix
    if deadline is not None:
        deadline.remaining()
    if (
        job.get("adapter") != "viewer" or cell.get("client_id") != "viewer"
        or cell.get("id") != job.get("cell_id")
        or contract.manifest.get("adapter_id") != "viewer"
        or config.get("product_version") != "2026.36.2"
        or not isinstance(running, Mapping)
        or not isinstance(running.get("descriptor"), Mapping)
    ):
        raise ViewerInstalledPending("installed Viewer descriptor is unavailable")
    artifacts = validate_job_inventory(job)
    session_config = _bound_json(job.get("installed_session", {}).get("config"), "installed session config")
    run_id = session_config.get("run_id")
    if not isinstance(run_id, str) or not run_id:
        raise ViewerInstalledPending("installed run identity is unavailable")
    try:
        scenario_raw = read_bound_file(session_config.get("scenario"), label="Viewer scenario", maximum=1_048_576)
    except Exception as error:
        raise ViewerInstalledPending("Viewer scenario is unavailable") from error
    environment = _path_json(Path(str(job.get("environment_file", ""))), "Viewer environment")
    if set(environment) != ENVIRONMENT_KEYS or any(not isinstance(value, str) for value in environment.values()):
        raise ViewerInstalledPending("Viewer environment is not closed")

    stage = _private_root(descriptor, cell["id"])
    profile_manifest, profile_members = _profile_inputs(config["profile"], stage / "profile")
    scenario = _stage(stage / "inputs", "scenario", scenario_raw, ".json")
    certificate_path = _canonical(
        Path(str(running["descriptor"].get("certificate_path", ""))), "certificate",
    )
    certificate_raw = _read_regular(certificate_path, "certificate", 65_536)
    certificate = _stage(stage / "inputs", "viewer_trusted_ca", certificate_raw, ".pem")
    try:
        certificate_der_sha256 = _digest(
            ssl.PEM_cert_to_DER_cert(certificate_raw.decode("ascii")),
        )
    except (UnicodeError, ValueError, ssl.SSLError) as error:
        raise ViewerInstalledPending("certificate is not PEM") from error

    product_inputs = []
    actor_manifests = {}
    product_specs = (
        ("viewer_package_tarball", "TESLATLAS_VIEWER_PACKAGE_ROOT",
         "TESLATLAS_VIEWER_PACKAGE_MANIFEST", VIEWER_ARCHIVE, "viewer"),
        ("typescript_sdk_tarball", "TESLATLAS_VIEWER_SDK_ROOT",
         "TESLATLAS_VIEWER_SDK_MANIFEST", SDK_ARCHIVE, "sdk"),
    )
    for role, root_key, manifest_key, archive, product_name in product_specs:
        artifact = artifacts[role]
        root = _canonical(Path(environment[root_key]), "installed product root")
        manifest_path = _canonical(Path(environment[manifest_key]), "installed product manifest")
        manifest = _path_json(manifest_path, "installed product manifest")
        validate_installed_product(root, artifact, manifest)
        archive_raw = _read_regular(archive, "product archive", 1_073_741_824, private=False)
        staged_archive = _stage(stage / "products", role, archive_raw, ".tgz")
        staged_manifest = _stage(
            stage / "products", product_name + "_installed_manifest",
            _json_bytes(manifest), ".json",
        )
        actor_manifests[role] = staged_manifest
        product_inputs.append({
            "artifact_role": role, "staged": staged_archive,
            "installed_manifest": staged_manifest, "local_root": str(root),
        })

    actors = [
        {
            "id": "viewer_ui", "kind": "built_viewer_ui",
            "execution": "browser_worker", "runtime_ref": "viewer_browser",
            "artifact_roles": ["viewer_package_tarball"],
            "source_roles": ["viewer_source"],
            "entrypoint_ref": "viewer_installed_playwright",
            "input_manifest": actor_manifests["viewer_package_tarball"],
            "phase_contract": None,
        },
        {
            "id": "viewer_sdk_contract", "kind": "packed_sdk_dependency",
            "execution": "local_worker", "runtime_ref": "viewer_browser",
            "artifact_roles": ["typescript_sdk_tarball"],
            "source_roles": ["typescript_sdk_source"],
            "entrypoint_ref": "viewer_installed_playwright_sdk_dependency",
            "input_manifest": actor_manifests["typescript_sdk_tarball"],
            "phase_contract": None,
        },
    ]
    header = {
        "schema_version": 1, "execution_kind": "actual_hub_acceptance",
        "adapter": "viewer", "cell_id": job["cell_id"],
        "product_version": config["product_version"],
        "profile_id": config["profile"]["id"],
        "profile_revision": config["profile"]["revision"],
        "profile_sha256": config["profile"]["sha256"],
        "source_identities": job["source_identities"],
        "artifacts": job["artifacts"], "runtime": job["runtime"],
    }
    header_stage = _stage(stage / "inputs", "header", _json_bytes(header), ".json")
    contract_raw = _read_regular(
        Path(CONTRACT.manifest_path), "reviewed Viewer contract", 1_048_576,
        private=False,
    )
    if _digest(contract_raw) != CONTRACT.manifest_sha256:
        raise ViewerInstalledPending("reviewed Viewer contract changed")
    contract_stage = _stage(stage / "inputs", "case-contract", contract_raw, ".json")
    output_root = stage / "outputs"
    output_root.mkdir(mode=0o700)
    outputs = {
        "normalized": str(output_root / "normalized.json"),
        "actor_evidence": str(output_root / "actor-evidence.json"),
        "coordination_dir": str(output_root / "coordination"),
        "framework_log": str(output_root / "framework.json"),
    }
    browser_path = _canonical(
        Path(environment["TESLATLAS_VIEWER_BROWSER_EXECUTABLE"]),
        "Viewer browser executable",
    )
    browser_raw = _read_regular(
        browser_path, "Viewer browser executable", 1_073_741_824, private=False,
    )
    browser_sha256 = environment["TESLATLAS_VIEWER_BROWSER_SHA256"]
    try:
        browser_mode = browser_path.lstat().st_mode
    except OSError as error:
        raise ViewerInstalledPending("Viewer browser executable is unavailable") from error
    if (
        _digest(browser_raw) != browser_sha256
        or stat.S_IMODE(browser_mode) & stat.S_IXUSR == 0
    ):
        raise ViewerInstalledPending("Viewer browser executable binding is invalid")
    viewer = {
        "page": {
            "origin": environment["TESLATLAS_VIEWER_PAGE_ORIGIN"],
            "server_authority": "runner-owned-installed-viewer",
            "artifact_role": "viewer_package_tarball",
        },
        "hub": {
            "public_origin": running["descriptor"].get("endpoint"),
            "cors_allowed_origin": environment["TESLATLAS_VIEWER_PAGE_ORIGIN"],
            "cross_origin": True,
        },
        "trusted_ca": {
            "certificate": certificate,
            "certificate_der_sha256": certificate_der_sha256,
        },
        "browser": {
            "authority": "runner-bound-executable",
            "engine": environment["TESLATLAS_VIEWER_BROWSER_ENGINE"],
            "version": environment["TESLATLAS_VIEWER_BROWSER_VERSION"],
            "executable": {"path": str(browser_path), "sha256": browser_sha256},
        },
        "reservations": {
            "raw_evidence_dir": str(output_root / "raw-evidence"),
            "browser_log": str(output_root / "browser.json"),
            "close_record": str(output_root / "close.json"),
            "supplement": str(output_root / "supplement.json"),
        },
    }
    try:
        validate_viewer_session_contract(viewer)
    except WireError as error:
        raise ViewerInstalledPending("Viewer SessionInput wire is invalid") from error
    session = {
        "schema_version": 1, "kind": "matrix-adapter-session",
        "run_id": run_id, "cell_id": cell["id"], "adapter_id": "viewer",
        "client_id": "viewer", "session_id": descriptor["session_id"],
        "instance_nonce": os.urandom(32).hex(), "header": header_stage,
        "case_contract": contract_stage, "host_session": descriptor,
        "broker": {"kind": "unix", "socket_path": descriptor["broker_socket"]},
        "inputs": {
            "profile_manifest": profile_manifest, "profile_members": profile_members,
            "scenario": scenario, "certificate": certificate,
            "certificate_der_sha256": certificate_der_sha256,
            "product_inputs": product_inputs,
        },
        "actors": actors, "outputs": outputs, "viewer": viewer,
        "bounds": {
            "cell_timeout_ms": job["timeout_seconds"] * 1000,
            "cleanup_timeout_ms": 45_000, "frame_bytes": 1_048_576,
            "evidence_bytes": 8_388_608, "framework_log_bytes": 8_388_608,
        },
    }
    session_path = stage / "session-input.json"
    _write_bytes(session_path, _json_bytes(session))
    return session_path, session


def launch_adapter(job, cell, config, contract, session_input_path, session_input):
    """Remain closed until the reviewed coordinator consumes the Viewer wire."""
    del job, cell, config, contract
    del session_input_path, session_input
    raise ViewerInstalledPending(
        "reviewed Viewer coordinator must consume the staged Viewer wire before launch"
    )


def admit(
    job, cell, config, matrix, contract, normalized, actors, *,
    admission_views=None, runtime_context=None, deadline=None,
):
    """Reject placeholders; only a complete reviewed per-case record can proceed."""
    del job, config, matrix, admission_views, runtime_context
    if deadline is not None:
        deadline.remaining()
    required = tuple(contract.manifest.get("required_cases", ()))
    cases = normalized.get("cases") if isinstance(normalized, Mapping) else None
    if not isinstance(cases, list) or tuple(item.get("id") for item in cases if isinstance(item, Mapping)) != required:
        raise ViewerInstalledPending("Viewer semantic evidence is pending")
    if any(
        not isinstance(item, Mapping) or item.get("status") != "passed"
        or item.get("expected") == {"evidence": "not-run"}
        or item.get("actual") == {"evidence": "not-run"}
        for item in cases
    ):
        raise ViewerInstalledPending("Viewer semantic evidence is pending")
    claims = actors.get("actors") if isinstance(actors, Mapping) else None
    invocations = actors.get("invocations") if isinstance(actors, Mapping) else None
    if (
        not isinstance(claims, list) or [item.get("id") for item in claims] != ["viewer_ui", "viewer_sdk_contract"]
        or not isinstance(invocations, list)
        or sorted(item.get("case_id") for item in invocations if isinstance(item, Mapping)) != sorted(required)
    ):
        raise ViewerInstalledPending("Viewer semantic evidence is pending")
    # The pure validator needs runner-created immutable actor, invocation, raw,
    # runtime, and controller views.  The current coordinator does not emit
    # them, so reaching this boundary remains pending rather than fabricating a
    # context from normalized expected/actual values.
    raise ViewerInstalledPending("Viewer runner-owned admission context is pending")


def runtime_inventory(job, cell, config, contract, session_input, deadline=None):
    del job, cell, config, contract, session_input
    if deadline is not None:
        deadline.remaining()
    raise ViewerInstalledPending("Viewer Playwright and browser runtime inventory is pending")


def build_supplement(job, cell, config, matrix, contract, result, *, runtime_context=None):
    del job, cell, config, matrix, contract, result, runtime_context
    raise ViewerInstalledPending("Viewer semantic supplement is pending")


def execution_logs(job, cell, config, contract, result):
    del job, cell, config, contract
    framework = Path(result.session_input["outputs"]["framework_log"])
    output = {}
    for name, path in (
        ("stdout", framework.parent / "launcher.stdout.log"),
        ("stderr", framework.parent / "launcher.stderr.log"),
    ):
        raw = _read_regular(path, "Viewer launcher " + name, 8_388_608)
        output[name] = {"sha256": _digest(raw), "bytes": len(raw), "truncated": False}
    output["duration_ms"] = 0
    return output


def source_entry(adapter_id: str):
    if adapter_id != "viewer":
        raise ViewerInstalledPending("Viewer adapter identity is foreign")
    return CONTRACT
