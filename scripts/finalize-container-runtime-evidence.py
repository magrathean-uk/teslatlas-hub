#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Create a redacted, hash-bound container-runtime evidence bundle in two phases."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import io
import json
import os
import re
import shutil
import stat
import sys
import tarfile
import tempfile
import uuid
from pathlib import Path

PRE_CLEANUP_FILES = (
    "source-archive.json",
    "source-archive.tar",
    "host-source-manifest.sha256",
    "guest-source-manifest.sha256",
    "build-summary.json",
    "tooling.json",
    "image-inspect.json",
    "binary-sha256.txt",
    "source-output.txt",
    "version.txt",
    "compose-rendered.json",
    "volume-init.json",
    "volume-permissions.json",
    "runtime-security.json",
    "tls-positive.json",
    "tls-negative.json",
    "health-before.json",
    "doctor-before.json",
    "status-before.json",
    "restart.json",
    "health-after.json",
    "doctor-after.json",
    "status-after.json",
    "database-sha256.txt",
)
CLEANUP_FILE = "cleanup.json"
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_SOURCE_ARCHIVE_BYTES = 64 * 1024 * 1024
HEX40 = re.compile(r"[0-9a-f]{40}\Z")
SHA_LINE = re.compile(rb"[0-9a-f]{64}(?:\s+[^\r\n]+)?\r?\n?\Z")
MANIFEST_LINE = re.compile(rb"([0-9a-f]{64})  ([^\r\n]+)\Z")
PRIVATE_KEY_HEADER = re.compile(
    rb"-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----", re.IGNORECASE
)
BEARER_CREDENTIAL = re.compile(rb"authorization\s*[:=]\s*bearer\s+\S+", re.IGNORECASE)
TEXT_CREDENTIAL = re.compile(
    rb"\b(?:[a-z0-9_-]*(?:token|secret|password|credential)|api[_-]?key|authorization|invitation|private[_-]?key)\b\s*=\s*[^\s]+",
    re.IGNORECASE,
)
SECRET_KEY_SUFFIXES = (
    "token",
    "secret",
    "password",
    "credential",
    "authorization",
    "invitation",
    "privatekey",
    "apikey",
)
CLEANUP_FLAGS = (
    "owned_containers_removed",
    "owned_network_removed",
    "owned_volume_removed",
    "owned_images_removed",
    "one_use_tls_removed",
    "guest_runtime_root_removed",
    "host_runtime_root_removed",
    "guest_stopped",
    "listeners_absent",
    "hub_process_absent",
    "heavy_build_lock_released",
)


def fail(message: str) -> "NoReturn":
    raise ValueError(message)


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace(
        "+00:00", "Z"
    )


def absolute(path: str, label: str) -> Path:
    candidate = Path(path)
    if not candidate.is_absolute():
        fail(f"{label} must be absolute")
    return candidate


def has_secret_value(value: object) -> bool:
    if value is None or value is False or value == "":
        return False
    if isinstance(value, (list, dict)):
        return bool(value)
    return True


def structured_secret(value: object) -> bool:
    if isinstance(value, dict):
        for key, child in value.items():
            normalized_key = re.sub(r"[^a-z0-9]", "", str(key).lower())
            if normalized_key.endswith(SECRET_KEY_SUFFIXES) and has_secret_value(child):
                return True
            if structured_secret(child):
                return True
    elif isinstance(value, list):
        return any(structured_secret(child) for child in value)
    return False


def reject_secrets(data: bytes, name: str) -> None:
    if PRIVATE_KEY_HEADER.search(data) or BEARER_CREDENTIAL.search(data) or TEXT_CREDENTIAL.search(data):
        fail(f"{name} contains prohibited secret-shaped content")
    if name.endswith(".json"):
        value = parse_json(data, name)
        if structured_secret(value):
            fail(f"{name} contains a populated secret-shaped field")


def safe_file(path: Path, name: str) -> bytes:
    status = path.lstat()
    if not stat.S_ISREG(status.st_mode) or path.is_symlink():
        fail(f"{name} must be a regular non-symlink file")
    size_limit = MAX_SOURCE_ARCHIVE_BYTES if name == "source-archive.tar" else MAX_FILE_BYTES
    if status.st_size <= 0 or status.st_size > size_limit:
        fail(f"{name} has invalid size {status.st_size}")
    data = path.read_bytes()
    if name != "source-archive.tar":
        reject_secrets(data, name)
    return data


def parse_json(data: bytes, name: str) -> object:
    try:
        return json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{name} is not valid JSON: {error}")


def git_object(kind: bytes, payload: bytes) -> bytes:
    header = kind + b" " + str(len(payload)).encode() + b"\0"
    return hashlib.sha1(header + payload).digest()


def git_tree(entries: dict[str, tuple[str, bytes] | dict]) -> bytes:
    serialized = bytearray()
    ordered = sorted(
        entries.items(), key=lambda item: (item[0] + ("/" if isinstance(item[1], dict) else "")).encode()
    )
    for name, value in ordered:
        if isinstance(value, dict):
            mode = b"40000"
            identity = git_tree(value)
        else:
            mode = value[0].encode()
            identity = value[1]
        serialized.extend(mode + b" " + name.encode() + b"\0" + identity)
    return git_object(b"tree", bytes(serialized))


def inspect_source_archive(data: bytes, source_commit: str) -> tuple[str, bytes, int]:
    root: dict[str, tuple[str, bytes] | dict] = {}
    manifest: list[tuple[str, str]] = []
    seen: set[str] = set()
    try:
        archive = tarfile.open(fileobj=io.BytesIO(data), mode="r:")
    except tarfile.TarError as error:
        fail(f"source archive is not a readable tar: {error}")
    with archive:
        if archive.pax_headers.get("comment") != source_commit:
            fail("source archive PAX commit does not match the requested commit")
        for member in archive.getmembers():
            path = Path(member.name)
            if path.is_absolute() or ".." in path.parts or not path.parts or path.parts[0] != "source":
                fail("source archive has an unsafe member path or wrong prefix")
            relative_parts = path.parts[1:]
            if not relative_parts:
                if not member.isdir():
                    fail("source archive prefix entry has the wrong type")
                continue
            relative = "/".join(relative_parts)
            if relative in seen:
                fail("source archive contains duplicate member paths")
            seen.add(relative)
            if member.isdir():
                continue
            if member.isfile():
                extracted = archive.extractfile(member)
                if extracted is None:
                    fail("source archive regular member cannot be read")
                payload = extracted.read()
                mode = "100755" if member.mode & 0o111 else "100644"
            elif member.issym():
                payload = member.linkname.encode()
                mode = "120000"
            else:
                fail("source archive contains an unsupported member type")
            manifest.append((relative, hashlib.sha256(payload).hexdigest()))
            node = root
            for component in relative_parts[:-1]:
                child = node.setdefault(component, {})
                if not isinstance(child, dict):
                    fail("source archive path conflicts with a file")
                node = child
            leaf = relative_parts[-1]
            if leaf in node:
                fail("source archive path conflicts with another member")
            node[leaf] = (mode, git_object(b"blob", payload))
    manifest.sort(key=lambda value: value[0].encode())
    manifest_bytes = b"".join(
        f"{digest}  {path}\n".encode() for path, digest in manifest
    )
    return git_tree(root).hex(), manifest_bytes, len(manifest)


def validate_cohort(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        fail("build cohort identity has the wrong shape")
    expected = {
        "compose_project",
        "image_id",
        "container_names",
        "network_name",
        "volume_name",
        "guest_runtime_root",
        "host_runtime_root",
    }
    if set(value) != expected:
        fail("build cohort identity has the wrong fields")
    for field in ("compose_project", "network_name", "volume_name"):
        if not re.fullmatch(r"[a-z0-9][a-z0-9_.-]{1,127}", str(value[field])):
            fail(f"build cohort {field} is invalid")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", str(value["image_id"])):
        fail("build cohort image ID is invalid")
    containers = value["container_names"]
    if not isinstance(containers, list) or not containers or any(
        not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{1,127}", str(name)) for name in containers
    ) or len(set(containers)) != len(containers):
        fail("build cohort container names are invalid")
    for field in ("guest_runtime_root", "host_runtime_root"):
        if not Path(str(value[field])).is_absolute():
            fail(f"build cohort {field} must be absolute")
    return value


def validate_contract(files: dict[str, bytes], source_commit: str) -> dict[str, object]:
    archive = parse_json(files["source-archive.json"], "source-archive.json")
    if not isinstance(archive, dict) or archive.get("source_commit") != source_commit:
        fail("source archive is not bound to the requested commit")
    if not re.fullmatch(r"[0-9a-f]{64}", str(archive.get("archive_sha256", ""))):
        fail("source archive SHA-256 is invalid")
    if not isinstance(archive.get("archive_bytes"), int) or archive["archive_bytes"] <= 0:
        fail("source archive byte count is invalid")
    if not HEX40.fullmatch(str(archive.get("tree", ""))):
        fail("source archive tree identity is invalid")
    if not re.fullmatch(r"[0-9a-f]{64}", str(archive.get("manifest_sha256", ""))):
        fail("source archive manifest SHA-256 is invalid")
    if not isinstance(archive.get("member_count"), int) or archive["member_count"] <= 0:
        fail("source archive member count is invalid")
    if files["host-source-manifest.sha256"] != files["guest-source-manifest.sha256"]:
        fail("host and guest source manifests differ")
    archive_bytes = files["source-archive.tar"]
    if hashlib.sha256(archive_bytes).hexdigest() != archive["archive_sha256"]:
        fail("retained source archive SHA-256 does not match its record")
    if len(archive_bytes) != archive["archive_bytes"]:
        fail("retained source archive byte count does not match its record")
    derived_tree, derived_manifest, derived_count = inspect_source_archive(
        archive_bytes, source_commit
    )
    if derived_tree != archive["tree"]:
        fail("source archive tree identity does not match its contents")
    manifest_bytes = files["host-source-manifest.sha256"]
    manifest_lines = manifest_bytes.splitlines()
    if not manifest_lines:
        fail("source manifest is empty")
    if hashlib.sha256(manifest_bytes).hexdigest() != archive["manifest_sha256"]:
        fail("source manifest SHA-256 does not match the archive record")
    if len(manifest_lines) != archive["member_count"]:
        fail("source manifest member count does not match the archive record")
    if derived_count != archive["member_count"] or derived_manifest != manifest_bytes:
        fail("source manifest does not describe the retained archive contents")
    manifest_paths = []
    for line in manifest_lines:
        match = MANIFEST_LINE.fullmatch(line)
        if match is None:
            fail("source manifest has an invalid line")
        member = match.group(2).decode("utf-8")
        member_path = Path(member)
        if member_path.is_absolute() or ".." in member_path.parts or member in ("", "."):
            fail("source manifest has an unsafe member path")
        manifest_paths.append(member)
    if len(set(manifest_paths)) != len(manifest_paths):
        fail("source manifest contains duplicate member paths")
    if not SHA_LINE.fullmatch(files["binary-sha256.txt"]):
        fail("binary SHA-256 record is invalid")
    if not SHA_LINE.fullmatch(files["database-sha256.txt"]):
        fail("database SHA-256 record is invalid")
    expected_url = (
        "https://github.com/magrathean-uk/teslatlas-hub/tree/" + source_commit
    )
    if files["source-output.txt"].decode().strip() != expected_url:
        fail("runtime source output does not match the requested commit")
    build = parse_json(files["build-summary.json"], "build-summary.json")
    if not isinstance(build, dict) or build.get("source_commit") != source_commit:
        fail("build summary is not bound to the requested commit")
    cohort = validate_cohort(build.get("cohort"))
    tooling = parse_json(files["tooling.json"], "tooling.json")
    if not isinstance(tooling, dict) or tooling.get("tooling_installed_during_run") is not False:
        fail("tooling summary does not prove a no-install run")
    if tooling.get("package_manager_mutated") is not False:
        fail("tooling summary does not prove an unmodified package manager")
    image = parse_json(files["image-inspect.json"], "image-inspect.json")
    if isinstance(image, list) and len(image) == 1:
        image = image[0]
    if not isinstance(image, dict):
        fail("image inspect has the wrong shape")
    if image.get("Os") != "linux" or image.get("Architecture") != "arm64":
        fail("image inspect is not Linux ARM64")
    if image.get("Id") != cohort["image_id"]:
        fail("image inspect is not bound to the build cohort image")
    image_config = image.get("Config")
    if not isinstance(image_config, dict):
        fail("image config has the wrong shape")
    image_environment = image_config.get("Env", [])
    if not isinstance(image_environment, list):
        fail("image environment has the wrong shape")
    secret_environment = re.compile(
        r"(?i)(token|secret|password|credential|authorization|invitation|private[_-]?key)"
    )
    if any(secret_environment.search(str(value).split("=", 1)[0]) for value in image_environment):
        fail("image environment contains a secret-shaped key")
    compose = parse_json(files["compose-rendered.json"], "compose-rendered.json")
    services = compose.get("services") if isinstance(compose, dict) else None
    hub = services.get("hub") if isinstance(services, dict) else None
    depends_on = hub.get("depends_on") if isinstance(hub, dict) else None
    dependency = depends_on.get("volume-init") if isinstance(depends_on, dict) else None
    if not isinstance(dependency, dict):
        fail("rendered Compose model has no volume-init dependency")
    if dependency.get("condition") != "service_completed_successfully":
        fail("rendered Compose model does not completion-order volume-init")
    volume_init = parse_json(files["volume-init.json"], "volume-init.json")
    if not isinstance(volume_init, dict) or volume_init.get("status") != "passed":
        fail("volume initializer evidence did not pass")
    permissions = parse_json(files["volume-permissions.json"], "volume-permissions.json")
    if not isinstance(permissions, dict) or permissions.get("root") != "10001:10001:700":
        fail("volume permission evidence is invalid")
    security = parse_json(files["runtime-security.json"], "runtime-security.json")
    if not isinstance(security, dict) or not (
        security.get("uid") == 10001
        and security.get("gid") == 10001
        and security.get("read_only_root") is True
        and security.get("cap_drop") == ["ALL"]
        and security.get("no_new_privileges") is True
    ):
        fail("runtime security evidence is invalid")
    tls_positive = parse_json(files["tls-positive.json"], "tls-positive.json")
    tls_negative = parse_json(files["tls-negative.json"], "tls-negative.json")
    if not isinstance(tls_positive, dict) or tls_positive.get("passed") is not True:
        fail("positive TLS evidence did not pass")
    if not isinstance(tls_negative, dict) or tls_negative.get("wrong_name_rejected") is not True:
        fail("negative TLS evidence did not reject the wrong name")
    for name in ("health-before.json", "doctor-before.json", "status-before.json", "health-after.json", "doctor-after.json", "status-after.json"):
        value = parse_json(files[name], name)
        if not isinstance(value, dict) or value.get("status") != "ok":
            fail(f"{name} did not report status ok")
    restart = parse_json(files["restart.json"], "restart.json")
    if not isinstance(restart, dict) or not (
        restart.get("healthy_after") is True
        and restart.get("identity_continuity") is True
        and restart.get("status_continuity") is True
    ):
        fail("restart evidence is incomplete")
    return cohort


def digest_record(relative: str, data: bytes) -> dict[str, object]:
    return {
        "path": relative,
        "bytes": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }


def aggregate(records: list[dict[str, object]]) -> str:
    payload = "".join(
        f"{record['sha256']}  {record['bytes']}  {record['path']}\n"
        for record in sorted(records, key=lambda value: str(value["path"]))
    ).encode()
    return hashlib.sha256(payload).hexdigest()


def write_manifest(
    bundle: Path,
    state: str,
    source_commit: str,
    evidence_run_id: str,
    cohort: dict[str, object],
) -> None:
    records = []
    for path in sorted((bundle / "files").iterdir()):
        data = safe_file(path, path.name)
        records.append(digest_record(f"files/{path.name}", data))
    manifest = {
        "schema_version": 1,
        "state": state,
        "generated_at": utc_now(),
        "source_commit": source_commit,
        "evidence_run_id": evidence_run_id,
        "cohort": cohort,
        "files": records,
        "aggregate_manifest_sha256": aggregate(records),
        "contains_secrets": False,
    }
    temporary = bundle / ".manifest.json.tmp"
    temporary.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    os.chmod(temporary, 0o600)
    temporary.replace(bundle / "manifest.json")


def prepare(args: argparse.Namespace) -> None:
    source_commit = args.source_commit
    if not HEX40.fullmatch(source_commit):
        fail("source commit must be exactly 40 lowercase hexadecimal characters")
    source = absolute(args.input, "input")
    output = absolute(args.output, "output")
    if output.exists() or output.is_symlink():
        fail("output must be new")
    actual = {path.name for path in source.iterdir()}
    expected = set(PRE_CLEANUP_FILES)
    if actual != expected:
        fail(f"input file set mismatch: missing={sorted(expected-actual)} extra={sorted(actual-expected)}")
    files = {name: safe_file(source / name, name) for name in PRE_CLEANUP_FILES}
    cohort = validate_contract(files, source_commit)
    evidence_run_id = str(uuid.uuid4())

    output.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=f".{output.name}.tmp-", dir=output.parent))
    try:
        os.chmod(staging, 0o700)
        destination = staging / "files"
        destination.mkdir(mode=0o700)
        for name, data in files.items():
            path = destination / name
            path.write_bytes(data)
            os.chmod(path, 0o600)
        write_manifest(
            staging, "READY_FOR_CLEANUP", source_commit, evidence_run_id, cohort
        )
        staging.replace(output)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def finalize(args: argparse.Namespace) -> None:
    bundle = absolute(args.bundle, "bundle")
    cleanup_path = absolute(args.cleanup, "cleanup")
    manifest_path = bundle / "manifest.json"
    manifest = parse_json(safe_file(manifest_path, "manifest.json"), "manifest.json")
    if not isinstance(manifest, dict) or manifest.get("state") != "READY_FOR_CLEANUP":
        fail("bundle is not ready for cleanup finalization")
    source_commit = str(manifest.get("source_commit", ""))
    if not HEX40.fullmatch(source_commit):
        fail("bundle source commit is invalid")
    evidence_run_id = str(manifest.get("evidence_run_id", ""))
    try:
        if str(uuid.UUID(evidence_run_id)) != evidence_run_id:
            fail("bundle evidence run ID is invalid")
    except ValueError:
        fail("bundle evidence run ID is invalid")
    cohort = validate_cohort(manifest.get("cohort"))
    expected_paths = {f"files/{name}" for name in PRE_CLEANUP_FILES}
    records = manifest.get("files")
    if not isinstance(records, list) or {record.get("path") for record in records} != expected_paths:
        fail("pre-cleanup manifest file set is invalid")
    for record in records:
        path = bundle / str(record["path"])
        data = safe_file(path, path.name)
        if digest_record(str(record["path"]), data) != record:
            fail(f"retained file changed after prepare: {record['path']}")

    cleanup_bytes = safe_file(cleanup_path, CLEANUP_FILE)
    cleanup = parse_json(cleanup_bytes, CLEANUP_FILE)
    if not isinstance(cleanup, dict) or any(cleanup.get(flag) is not True for flag in CLEANUP_FLAGS):
        fail("cleanup receipt does not close every required resource")
    if cleanup.get("source_commit") != source_commit:
        fail("cleanup receipt source commit does not match the prepared bundle")
    if cleanup.get("evidence_run_id") != evidence_run_id:
        fail("cleanup receipt run ID does not match the prepared bundle")
    if cleanup.get("cohort") != cohort:
        fail("cleanup receipt cohort does not match the prepared bundle")
    destination = bundle / "files" / CLEANUP_FILE
    if destination.exists() or destination.is_symlink():
        fail("cleanup evidence already exists")
    destination.write_bytes(cleanup_bytes)
    os.chmod(destination, 0o600)
    try:
        write_manifest(
            bundle, "COMPLETE", source_commit, evidence_run_id, cohort
        )
    except Exception:
        destination.unlink(missing_ok=True)
        raise


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare")
    prepare_parser.add_argument("--input", required=True)
    prepare_parser.add_argument("--output", required=True)
    prepare_parser.add_argument("--source-commit", required=True)
    prepare_parser.set_defaults(handler=prepare)
    finalize_parser = commands.add_parser("finalize")
    finalize_parser.add_argument("--bundle", required=True)
    finalize_parser.add_argument("--cleanup", required=True)
    finalize_parser.set_defaults(handler=finalize)
    return root


def main() -> int:
    try:
        args = parser().parse_args()
        args.handler(args)
        return 0
    except (OSError, ValueError, AssertionError, KeyError, TypeError, AttributeError) as error:
        print(f"container-runtime-evidence: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
