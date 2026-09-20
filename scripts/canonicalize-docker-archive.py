#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Validate and deterministically repack one Docker image-save archive.

This tool deliberately does not rewrite image configuration or layer bytes.
BuildKit and SOURCE_DATE_EPOCH must already have made those content-addressed
objects reproducible. The supported hybrid OCI/Docker envelope is validated,
then reduced to the proven minimal Docker-load member set while Docker tag
records are rebound from the private input cohort to the requested output tag.
"""

from __future__ import annotations

import argparse
import ctypes
import datetime as dt
import errno
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import signal
import stat
import sys
import tarfile
from typing import Any, BinaryIO


HEX64 = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")
DIFF_ID = re.compile(r"sha256:[0-9a-f]{64}")
BLOB_PATH = re.compile(r"blobs/sha256/([0-9a-f]{64})")
EMPTY_LAYER_DIGEST = "sha256:5f70bf18a086007016e948b04aed3b82103a36bea41755b6cddfaf10ace3c6ef"
EMPTY_LAYER_BYTES = 1024
OCI_CONFIG_MEDIA_TYPE = "application/vnd.oci.image.config.v1+json"
OCI_INDEX_MEDIA_TYPE = "application/vnd.oci.image.index.v1+json"
OCI_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"
OCI_LAYER_MEDIA_TYPE = "application/vnd.oci.image.layer.v1.tar"
MAX_ARCHIVE_BYTES = 4 * 1024 * 1024 * 1024
MAX_JSON_BYTES = 8 * 1024 * 1024
MAX_MEMBERS = 4096
MAX_LAYER_MEMBERS = 100_000
SOURCE_URL = "https://github.com/magrathean-uk/teslatlas-hub"


class ArchiveError(ValueError):
    pass


def fail(message: str) -> None:
    raise ArchiveError(message)


def json_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            fail(f"JSON contains duplicate key: {key}")
        result[key] = value
    return result


def parse_json(data: bytes, label: str) -> Any:
    if len(data) > MAX_JSON_BYTES:
        fail(f"{label} is too large")
    try:
        return json.loads(data.decode("utf-8"), object_pairs_hook=json_object)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is not canonicalizable JSON: {error}")


def read_member(archive: tarfile.TarFile, member: tarfile.TarInfo, label: str) -> bytes:
    if not member.isreg() or member.size > MAX_JSON_BYTES:
        fail(f"{label} must be a bounded regular file")
    source = archive.extractfile(member)
    if source is None:
        fail(f"{label} cannot be read")
    data = source.read(member.size + 1)
    if len(data) != member.size:
        fail(f"{label} has a short or oversized read")
    return data


def normalized_name(member: tarfile.TarInfo) -> str:
    name = member.name[:-1] if member.isdir() and member.name.endswith("/") else member.name
    if not name or name.startswith("/") or "\\" in name or "\x00" in name:
        fail(f"archive contains unsafe path: {member.name!r}")
    parts = PurePosixPath(name).parts
    if any(part in ("", ".", "..") for part in parts):
        fail(f"archive contains non-canonical path: {member.name!r}")
    return name


def open_regular_nofollow(path: Path, label: str) -> tuple[BinaryIO, os.stat_result]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except FileNotFoundError:
        fail(f"{label} does not exist")
    except OSError as error:
        fail(f"{label} cannot be opened safely: {error}")
    try:
        info = os.fstat(descriptor)
    except Exception:
        os.close(descriptor)
        raise
    if not stat.S_ISREG(info.st_mode):
        os.close(descriptor)
        fail(f"{label} must be a regular non-symlink file")
    return os.fdopen(descriptor, "rb"), info


def sha256_stream(source: BinaryIO) -> str:
    digest = hashlib.sha256()
    while True:
        chunk = source.read(1024 * 1024)
        if not chunk:
            return digest.hexdigest()
        digest.update(chunk)


def validate_application_layer(source: BinaryIO, label: str, epoch: int) -> None:
    """Reject layer structures this Dockerfile never emits or needs."""
    try:
        with tarfile.open(fileobj=source, mode="r:") as layer:
            members = layer.getmembers()
    except tarfile.TarError as error:
        fail(f"{label} is not an uncompressed tar archive: {error}")
    if len(members) > MAX_LAYER_MEMBERS:
        fail(f"{label} contains too many members")
    names: set[str] = set()
    for member in members:
        if not (member.isreg() or member.isdir()):
            fail(f"{label} contains a link or special member: {member.name}")
        if member.pax_headers or member.sparse is not None:
            fail(f"{label} contains unsupported extended or sparse metadata: {member.name}")
        if member.mtime != epoch:
            fail(f"{label} member mtime does not equal SOURCE_DATE_EPOCH: {member.name}")
        name = normalized_name(member)
        if name in names:
            fail(f"{label} contains a duplicate path: {name}")
        names.add(name)
        basename = PurePosixPath(name).name
        if basename.startswith(".wh."):
            if not member.isreg() or member.size != 0 or basename == ".wh.":
                fail(f"{label} contains an invalid whiteout: {name}")


def parse_timestamp(value: Any, label: str) -> dt.datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        fail(f"{label} must be an RFC3339 UTC timestamp")
    try:
        parsed = dt.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError:
        fail(f"{label} must be an RFC3339 UTC timestamp")
    if parsed.utcoffset() != dt.timedelta(0):
        fail(f"{label} must be UTC")
    return parsed


def split_tag(reference: str) -> tuple[str, str]:
    slash = reference.rfind("/")
    colon = reference.rfind(":")
    if colon <= slash or not reference[:colon] or not reference[colon + 1 :]:
        fail("repository tag must contain an explicit tag")
    return reference[:colon], reference[colon + 1 :]


def validate_config(
    config: dict[str, Any],
    *,
    commit: str,
    epoch: int,
    version: str,
    diff_ids: list[str],
    base_diff_ids: list[str],
) -> None:
    required_config_keys = {"created", "architecture", "os", "config", "rootfs", "history"}
    allowed_config_keys = required_config_keys | {
        "author",
        "variant",
        "os.version",
        "os.features",
        "docker_version",
    }
    if not required_config_keys.issubset(config) or not set(config).issubset(allowed_config_keys):
        fail("image config contains missing or unsupported top-level fields")
    if config.get("os") != "linux" or config.get("architecture") != "arm64":
        fail("image config platform must be linux/arm64")
    if config.get("variant") not in (None, "v8"):
        fail("image config ARM64 variant must be absent or v8")
    if parse_timestamp(config.get("created"), "image config created").timestamp() != epoch:
        fail("image config created timestamp does not equal SOURCE_DATE_EPOCH")

    runtime = config.get("config")
    if not isinstance(runtime, dict):
        fail("image runtime config is missing")
    expected_runtime_keys = {
        "Labels",
        "User",
        "Env",
        "Entrypoint",
        "Cmd",
        "WorkingDir",
        "ArgsEscaped",
    }
    if set(runtime) != expected_runtime_keys:
        fail("image runtime config contains missing or unsupported fields")
    labels = runtime.get("Labels")
    expected_labels = {
        "org.opencontainers.image.title": "Teslatlas Hub",
        "org.opencontainers.image.version": version,
        "org.opencontainers.image.source": SOURCE_URL,
        "org.opencontainers.image.revision": commit,
    }
    if not isinstance(labels, dict):
        fail("image source labels are missing")
    if labels != expected_labels:
        fail("image labels do not match the exact selected source and product")
    expected_environment = [
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt",
    ]
    expected_runtime = {
        "User": "10001:10001",
        "Env": expected_environment,
        "Entrypoint": ["/usr/local/bin/teslatlas-hub"],
        "Cmd": ["--config", "/etc/teslatlas-hub/config.toml", "serve"],
        "WorkingDir": "/var/lib/teslatlas-hub",
    }
    for key, value in expected_runtime.items():
        if runtime.get(key) != value:
            fail(f"image runtime config {key} does not match the supported contract")
    if runtime.get("ArgsEscaped") is not True:
        fail("image runtime config ArgsEscaped does not match the supported contract")
    rootfs = config.get("rootfs")
    if (
        not isinstance(rootfs, dict)
        or set(rootfs) != {"type", "diff_ids"}
        or rootfs.get("type") != "layers"
    ):
        fail("image rootfs must use layers")
    recorded = rootfs.get("diff_ids")
    if recorded != diff_ids:
        fail("image rootfs diff IDs do not match the archived layer bytes")
    if diff_ids[: len(base_diff_ids)] != base_diff_ids:
        fail("image rootfs does not begin with the pinned Debian ARM64 base")
    application_layer_count = len(diff_ids) - len(base_diff_ids)
    if application_layer_count != 2 or diff_ids[-1] != EMPTY_LAYER_DIGEST:
        fail("image must contain the staged Hub layer and canonical empty WORKDIR layer")

    history = config.get("history")
    if not isinstance(history, list) or not all(isinstance(item, dict) for item in history):
        fail("image history is missing or invalid")
    allowed_history_keys = {"created", "author", "created_by", "comment", "empty_layer"}
    for index, item in enumerate(history):
        if not set(item).issubset(allowed_history_keys):
            fail(f"image history entry {index} contains unsupported fields")
        if "empty_layer" in item and not isinstance(item["empty_layer"], bool):
            fail(f"image history entry {index} empty_layer must be boolean")
    nonempty = [index for index, item in enumerate(history) if item.get("empty_layer", False) is False]
    if len(nonempty) != len(diff_ids):
        fail("image history and rootfs layer counts disagree")
    application_start = nonempty[-application_layer_count]
    history_timestamps = [
        parse_timestamp(item.get("created"), f"image history entry {index} created")
        for index, item in enumerate(history)
    ]
    while application_start > 0 and history_timestamps[application_start - 1].timestamp() == epoch:
        application_start -= 1
    for created in history_timestamps[:application_start]:
        if created.timestamp() > epoch:
            fail("base history contains a timestamp after SOURCE_DATE_EPOCH")
    for created in history_timestamps[application_start:]:
        if created.timestamp() != epoch:
            fail("BuildKit did not normalize the Hub application history suffix")


def validate_base_lock(path: Path) -> list[str]:
    source, info = open_regular_nofollow(path, "base image lock")
    with source:
        if info.st_size > MAX_JSON_BYTES:
            fail("base image lock is too large")
        data = source.read(MAX_JSON_BYTES + 1)
    if len(data) != info.st_size:
        fail("base image lock changed while it was read")
    lock = parse_json(data, "base image lock")
    try:
        diff_ids = lock["images"]["runtime"]["linux_arm64_rootfs_diff_ids"]
    except (KeyError, TypeError):
        fail("base image lock is missing runtime rootfs diff IDs")
    if not isinstance(diff_ids, list) or not diff_ids or not all(
        isinstance(value, str) and DIFF_ID.fullmatch(value) for value in diff_ids
    ):
        fail("base image lock has invalid runtime rootfs diff IDs")
    return diff_ids


def validate_descriptor(
    value: Any,
    *,
    media_type: str,
    digest: str,
    size: int,
    label: str,
) -> None:
    if not isinstance(value, dict) or set(value) != {"mediaType", "digest", "size"}:
        fail(f"{label} descriptor has unsupported or missing fields")
    if value != {"mediaType": media_type, "digest": digest, "size": size}:
        fail(f"{label} descriptor does not match its content-addressed blob")


def validate_empty_layer(source: BinaryIO, label: str) -> None:
    data = source.read(EMPTY_LAYER_BYTES + 1)
    if data != b"\0" * EMPTY_LAYER_BYTES:
        fail(f"{label} is not the canonical 1024-byte empty tar layer")
    try:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as layer:
            if layer.getmembers():
                fail(f"{label} must contain zero members")
    except tarfile.TarError as error:
        fail(f"{label} is not a valid empty tar layer: {error}")


def parse_legacy_timestamp(value: Any, label: str) -> dt.datetime:
    if not isinstance(value, str):
        fail(f"{label} must be a timestamp")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        fail(f"{label} must be an ISO-8601 timestamp")
    if parsed.utcoffset() is None:
        fail(f"{label} must include an offset")
    return parsed.astimezone(dt.timezone.utc)


def validate_legacy_metadata_chain(
    blobs: list[dict[str, Any]], *, commit: str, epoch: int, version: str
) -> None:
    if len(blobs) != 3:
        fail("hybrid archive must contain exactly three legacy metadata blobs")
    empty_container_config = {
        "Hostname": "",
        "Domainname": "",
        "User": "",
        "AttachStdin": False,
        "AttachStdout": False,
        "AttachStderr": False,
        "Tty": False,
        "OpenStdin": False,
        "StdinOnce": False,
        "Env": None,
        "Cmd": None,
        "Image": "",
        "Volumes": None,
        "WorkingDir": "",
        "Entrypoint": None,
        "OnBuild": None,
        "Labels": None,
    }
    by_id: dict[str, dict[str, Any]] = {}
    for index, blob in enumerate(blobs):
        identifier = blob.get("id") if isinstance(blob, dict) else None
        if not isinstance(identifier, str) or not HEX64.fullmatch(identifier) or identifier in by_id:
            fail(f"legacy metadata blob {index} has an invalid or duplicate ID")
        if blob.get("container_config") != empty_container_config or blob.get("os") != "linux":
            fail(f"legacy metadata blob {index} has unsupported container metadata")
        by_id[identifier] = blob
    roots = [blob for blob in blobs if "parent" not in blob]
    if len(roots) != 1:
        fail("legacy metadata must have exactly one chain root")
    ordered = [roots[0]]
    while len(ordered) < 3:
        children = [blob for blob in blobs if blob.get("parent") == ordered[-1]["id"]]
        if len(children) != 1:
            fail("legacy metadata parent chain is incomplete or ambiguous")
        ordered.append(children[0])
    if len({blob["id"] for blob in ordered}) != 3:
        fail("legacy metadata parent chain contains a cycle")

    if set(ordered[0]) != {"id", "created", "container_config", "os"}:
        fail("legacy root metadata has unsupported fields")
    if set(ordered[1]) != {"id", "parent", "created", "container_config", "os"}:
        fail("legacy middle metadata has unsupported fields")
    if set(ordered[2]) != {
        "id",
        "parent",
        "created",
        "container_config",
        "config",
        "architecture",
        "os",
    }:
        fail("legacy final metadata has unsupported fields")
    for index, blob in enumerate(ordered[:2]):
        if parse_legacy_timestamp(blob["created"], f"legacy metadata {index} created").timestamp() != 0:
            fail("legacy base metadata timestamp must be the Unix epoch")
    if parse_legacy_timestamp(ordered[2]["created"], "legacy final metadata created").timestamp() != epoch:
        fail("legacy final metadata timestamp does not equal SOURCE_DATE_EPOCH")
    expected_labels = {
        "org.opencontainers.image.title": "Teslatlas Hub",
        "org.opencontainers.image.version": version,
        "org.opencontainers.image.source": SOURCE_URL,
        "org.opencontainers.image.revision": commit,
    }
    expected_final_config = {
        **empty_container_config,
        "User": "10001:10001",
        "Env": [
            "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt",
        ],
        "Cmd": ["--config", "/etc/teslatlas-hub/config.toml", "serve"],
        "ArgsEscaped": True,
        "WorkingDir": "/var/lib/teslatlas-hub",
        "Entrypoint": ["/usr/local/bin/teslatlas-hub"],
        "Labels": expected_labels,
    }
    if ordered[2].get("architecture") != "arm64" or ordered[2].get("config") != expected_final_config:
        fail("legacy final metadata does not match the supported runtime contract")


def add_member(
    output: tarfile.TarFile,
    source: tarfile.TarFile,
    member: tarfile.TarInfo,
    name: str,
    epoch: int,
    replacement: bytes | None = None,
) -> None:
    target = tarfile.TarInfo(name + ("/" if member.isdir() else ""))
    target.type = tarfile.DIRTYPE if member.isdir() else tarfile.REGTYPE
    target.mode = 0o755 if member.isdir() else 0o644
    target.uid = target.gid = 0
    target.uname = target.gname = ""
    target.mtime = epoch
    target.size = 0 if member.isdir() else (len(replacement) if replacement is not None else member.size)
    if member.isdir():
        output.addfile(target)
        return
    stream = io.BytesIO(replacement) if replacement is not None else source.extractfile(member)
    if stream is None:
        fail(f"archive member cannot be read: {name}")
    output.addfile(target, stream)


def atomic_publish_noreplace(parent_descriptor: int, temporary_name: str, output_name: str) -> None:
    library = ctypes.CDLL(None, use_errno=True)
    old_name = os.fsencode(temporary_name)
    new_name = os.fsencode(output_name)
    if sys.platform == "darwin":
        operation = library.renameatx_np
        operation.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        operation.restype = ctypes.c_int
        result = operation(parent_descriptor, old_name, parent_descriptor, new_name, 0x00000004)
    elif sys.platform.startswith("linux"):
        try:
            operation = library.renameat2
        except AttributeError:
            fail("platform libc does not provide renameat2 for exclusive publication")
        operation.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        operation.restype = ctypes.c_int
        result = operation(parent_descriptor, old_name, parent_descriptor, new_name, 0x00000001)
    else:
        fail("platform does not provide supported exclusive atomic publication")
    if result != 0:
        error_number = ctypes.get_errno()
        if error_number == errno.EEXIST:
            fail("output path appeared before exclusive publication")
        raise OSError(error_number, os.strerror(error_number), output_name)


def canonicalize(args: argparse.Namespace) -> dict[str, Any]:
    input_path = Path(args.input)
    output_path = Path(args.output)
    if output_path.exists() or output_path.is_symlink():
        fail("output path already exists")
    if not output_path.parent.is_dir() or output_path.parent.is_symlink():
        fail("output parent must be a real directory")
    if not COMMIT.fullmatch(args.source_commit):
        fail("source commit must be exactly 40 lowercase hexadecimal characters")
    if not re.fullmatch(r"[0-9]{1,10}", args.source_date_epoch):
        fail("SOURCE_DATE_EPOCH must be explicit Unix seconds")
    epoch = int(args.source_date_epoch)
    input_repository, input_tag = split_tag(args.input_repository_tag)
    output_repository, output_tag = split_tag(args.output_repository_tag)
    if not args.version or any(character.isspace() for character in args.version):
        fail("version is invalid")
    image_id = args.image_id.removeprefix("sha256:")
    if not HEX64.fullmatch(image_id):
        fail("image ID must be a sha256 digest")
    base_diff_ids = validate_base_lock(Path(args.base_lock))
    input_source, input_info = open_regular_nofollow(input_path, "input archive")
    if input_info.st_size > MAX_ARCHIVE_BYTES:
        input_source.close()
        fail("input archive is too large")

    output_parent = output_path.parent
    output_name = output_path.name
    parent_flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_DIRECTORY", 0)
    parent_flags |= getattr(os, "O_NOFOLLOW", 0)
    parent_descriptor = os.open(output_parent, parent_flags)
    temporary_name = f".{output_name}.tmp.{secrets.token_hex(12)}"
    temporary_identity: tuple[int, int] | None = None
    try:
        with input_source, tarfile.open(fileobj=input_source, mode="r:") as archive:
            members = archive.getmembers()
            if len(members) > MAX_MEMBERS:
                fail("input archive contains too many members")
            indexed: dict[str, tarfile.TarInfo] = {}
            for member in members:
                if not (member.isreg() or member.isdir()):
                    fail(f"input archive contains a link or special member: {member.name}")
                if member.pax_headers:
                    fail(f"input archive contains ambiguous PAX metadata: {member.name}")
                name = normalized_name(member)
                if name in indexed:
                    fail(f"input archive contains duplicate member: {name}")
                indexed[name] = member

            manifest_member = indexed.get("manifest.json")
            if manifest_member is None:
                fail("input archive is missing manifest.json")
            manifest = parse_json(read_member(archive, manifest_member, "manifest.json"), "manifest.json")
            if not isinstance(manifest, list) or len(manifest) != 1 or not isinstance(manifest[0], dict):
                fail("input archive must contain exactly one image")
            image = manifest[0]
            if set(image) != {"Config", "RepoTags", "Layers", "LayerSources"}:
                fail("manifest has unsupported or missing fields")
            if image.get("RepoTags") != [args.input_repository_tag]:
                fail("manifest repository tag does not match the private input cohort")
            blob_members: dict[str, tarfile.TarInfo] = {}
            blob_sizes: dict[str, int] = {}
            for name, member in indexed.items():
                match = BLOB_PATH.fullmatch(name)
                if match is None:
                    continue
                if not member.isreg():
                    fail(f"content-addressed blob is not regular: {name}")
                digest = match.group(1)
                stream = archive.extractfile(member)
                if stream is None or sha256_stream(stream) != digest:
                    fail(f"content-addressed blob digest does not match its path: {name}")
                blob_members[digest] = member
                blob_sizes[digest] = member.size

            config_name = image.get("Config")
            config_match = BLOB_PATH.fullmatch(config_name) if isinstance(config_name, str) else None
            if config_match is None:
                fail("manifest config path is invalid")
            config_digest = config_match.group(1)
            config_member = indexed.get(config_name)
            if config_member is None:
                fail("manifest config is missing")
            config_bytes = read_member(archive, config_member, "image config")
            if config_digest != image_id:
                fail("image config bytes, filename, and requested image ID disagree")
            config = parse_json(config_bytes, "image config")
            if not isinstance(config, dict):
                fail("image config must be an object")

            layer_names = image.get("Layers")
            if not isinstance(layer_names, list) or not layer_names:
                fail("manifest layer list is missing")
            layer_digests: list[str] = []
            diff_ids: list[str] = []
            for layer_index, layer_name in enumerate(layer_names):
                match = BLOB_PATH.fullmatch(layer_name) if isinstance(layer_name, str) else None
                if match is None:
                    fail("manifest contains a non-canonical layer path")
                layer_digest = match.group(1)
                if layer_digest in layer_digests:
                    fail("manifest contains a duplicate layer digest")
                layer_digests.append(layer_digest)
                layer_member = indexed.get(layer_name)
                if layer_member is None or not layer_member.isreg():
                    fail("manifest layer is missing or not regular")
                diff_ids.append(f"sha256:{layer_digest}")
                if layer_index == len(base_diff_ids):
                    validation_stream = archive.extractfile(layer_member)
                    if validation_stream is None:
                        fail("manifest application layer cannot be read")
                    validate_application_layer(
                        validation_stream,
                        "staged Hub application layer",
                        epoch,
                    )
                elif layer_index == len(base_diff_ids) + 1:
                    if layer_member.size != EMPTY_LAYER_BYTES or diff_ids[-1] != EMPTY_LAYER_DIGEST:
                        fail("WORKDIR layer is not the canonical empty layer")
                    validation_stream = archive.extractfile(layer_member)
                    if validation_stream is None:
                        fail("empty WORKDIR layer cannot be read")
                    validate_empty_layer(validation_stream, "empty WORKDIR layer")

            validate_config(
                config,
                commit=args.source_commit,
                epoch=epoch,
                version=args.version,
                diff_ids=diff_ids,
                base_diff_ids=base_diff_ids,
            )

            layer_sources = image.get("LayerSources")
            if not isinstance(layer_sources, dict) or set(layer_sources) != set(diff_ids):
                fail("manifest LayerSources does not exactly describe every layer")
            for digest, name in zip(diff_ids, layer_names):
                validate_descriptor(
                    layer_sources[digest],
                    media_type=OCI_LAYER_MEDIA_TYPE,
                    digest=digest,
                    size=indexed[name].size,
                    label=f"manifest LayerSources {digest}",
                )

            layout_member = indexed.get("oci-layout")
            index_member = indexed.get("index.json")
            if layout_member is None or index_member is None:
                fail("hybrid archive is missing OCI layout metadata")
            layout = parse_json(read_member(archive, layout_member, "oci-layout"), "oci-layout")
            if layout != {"imageLayoutVersion": "1.0.0"}:
                fail("OCI layout version is unsupported")
            oci_index = parse_json(read_member(archive, index_member, "index.json"), "index.json")
            if (
                not isinstance(oci_index, dict)
                or set(oci_index) != {"schemaVersion", "mediaType", "manifests"}
                or oci_index.get("schemaVersion") != 2
                or oci_index.get("mediaType") != OCI_INDEX_MEDIA_TYPE
                or not isinstance(oci_index.get("manifests"), list)
                or len(oci_index["manifests"]) != 1
            ):
                fail("OCI index must contain exactly one supported image manifest")
            oci_descriptor = oci_index["manifests"][0]
            if not isinstance(oci_descriptor, dict) or set(oci_descriptor) != {
                "mediaType",
                "digest",
                "size",
                "annotations",
            }:
                fail("OCI index manifest descriptor has unsupported or missing fields")
            expected_annotations = {
                "io.containerd.image.name": f"docker.io/library/{args.input_repository_tag}",
                "org.opencontainers.image.ref.name": input_tag,
            }
            if oci_descriptor.get("annotations") != expected_annotations:
                fail("OCI index annotations do not bind the private input cohort")
            oci_manifest_digest = oci_descriptor.get("digest")
            if not isinstance(oci_manifest_digest, str) or not DIFF_ID.fullmatch(oci_manifest_digest):
                fail("OCI index manifest digest is invalid")
            oci_manifest_hash = oci_manifest_digest.removeprefix("sha256:")
            oci_manifest_member = blob_members.get(oci_manifest_hash)
            if oci_manifest_member is None:
                fail("OCI image manifest blob is missing")
            validate_descriptor(
                {key: oci_descriptor[key] for key in ("mediaType", "digest", "size")},
                media_type=OCI_MANIFEST_MEDIA_TYPE,
                digest=oci_manifest_digest,
                size=oci_manifest_member.size,
                label="OCI index image manifest",
            )
            oci_manifest = parse_json(
                read_member(archive, oci_manifest_member, "OCI image manifest"),
                "OCI image manifest",
            )
            if not isinstance(oci_manifest, dict) or set(oci_manifest) != {
                "schemaVersion",
                "mediaType",
                "config",
                "layers",
            } or oci_manifest.get("schemaVersion") != 2 or oci_manifest.get("mediaType") != OCI_MANIFEST_MEDIA_TYPE:
                fail("OCI image manifest has unsupported or missing fields")
            validate_descriptor(
                oci_manifest.get("config"),
                media_type=OCI_CONFIG_MEDIA_TYPE,
                digest=f"sha256:{config_digest}",
                size=config_member.size,
                label="OCI image config",
            )
            oci_layers = oci_manifest.get("layers")
            if not isinstance(oci_layers, list) or len(oci_layers) != len(layer_names):
                fail("OCI image manifest layer list is invalid")
            for descriptor_value, digest, name in zip(oci_layers, diff_ids, layer_names):
                validate_descriptor(
                    descriptor_value,
                    media_type=OCI_LAYER_MEDIA_TYPE,
                    digest=digest,
                    size=indexed[name].size,
                    label=f"OCI image layer {digest}",
                )

            repositories_member = indexed.get("repositories")
            if repositories_member is None:
                fail("input archive is missing repositories")
            repositories = parse_json(
                read_member(archive, repositories_member, "repositories"), "repositories"
            )
            if repositories != {input_repository: {input_tag: layer_digests[-1]}}:
                fail("repositories does not bind the private input cohort to the final layer")

            referenced_blob_hashes = {config_digest, oci_manifest_hash, *layer_digests}
            legacy_blob_hashes = set(blob_members) - referenced_blob_hashes
            legacy_blobs = [
                parse_json(
                    read_member(archive, blob_members[digest], f"legacy metadata blob {digest}"),
                    f"legacy metadata blob {digest}",
                )
                for digest in sorted(legacy_blob_hashes)
            ]
            validate_legacy_metadata_chain(
                legacy_blobs,
                commit=args.source_commit,
                epoch=epoch,
                version=args.version,
            )

            expected_input = {
                "blobs",
                "blobs/sha256",
                "index.json",
                "manifest.json",
                "oci-layout",
                "repositories",
                *(f"blobs/sha256/{digest}" for digest in blob_members),
            }
            if set(indexed) != expected_input:
                fail("input archive contains missing, extra, or ambiguous members")

            output_manifest = json.dumps(
                [
                    {
                        "Config": config_name,
                        "RepoTags": [args.output_repository_tag],
                        "Layers": layer_names,
                        "LayerSources": layer_sources,
                    }
                ],
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
            output_repositories = json.dumps(
                {output_repository: {output_tag: layer_digests[-1]}},
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
            replacements = {
                "manifest.json": output_manifest,
                "repositories": output_repositories,
            }

            output_names = {
                "blobs",
                "blobs/sha256",
                "manifest.json",
                "repositories",
                config_name,
                *layer_names,
            }

            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
            descriptor = os.open(temporary_name, flags, 0o644, dir_fd=parent_descriptor)
            descriptor_info = os.fstat(descriptor)
            temporary_identity = (descriptor_info.st_dev, descriptor_info.st_ino)
            with os.fdopen(descriptor, "wb") as raw_output:
                with tarfile.open(fileobj=raw_output, mode="w", format=tarfile.PAX_FORMAT) as output:
                    for name in sorted(output_names):
                        add_member(
                            output,
                            archive,
                            indexed[name],
                            name,
                            epoch,
                            replacements.get(name),
                        )
                raw_output.flush()
                os.fsync(raw_output.fileno())

        read_flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        current_temporary = os.stat(
            temporary_name, dir_fd=parent_descriptor, follow_symlinks=False
        )
        if (current_temporary.st_dev, current_temporary.st_ino) != temporary_identity:
            fail("private output temporary was replaced before publication")
        result_descriptor = os.open(temporary_name, read_flags, dir_fd=parent_descriptor)
        with os.fdopen(result_descriptor, "rb") as result:
            result_info = os.fstat(result.fileno())
            if (result_info.st_dev, result_info.st_ino) != temporary_identity:
                fail("private output temporary changed before publication")
            archive_bytes = os.fstat(result.fileno()).st_size
            archive_sha256 = sha256_stream(result)
        atomic_publish_noreplace(parent_descriptor, temporary_name, output_name)
        current_output = os.stat(output_name, dir_fd=parent_descriptor, follow_symlinks=False)
        if (current_output.st_dev, current_output.st_ino) != temporary_identity:
            fail("published output identity does not match the validated temporary")
        os.fsync(parent_descriptor)
        return {
            "schema_version": 1,
            "repository_tag": args.output_repository_tag,
            "source_commit": args.source_commit,
            "source_date_epoch": epoch,
            "platform": "linux/arm64",
            "image_id": f"sha256:{image_id}",
            "rootfs_diff_ids": diff_ids,
            "archive_sha256": archive_sha256,
            "archive_bytes": archive_bytes,
            "normalization": "minimal Docker archive; outer metadata normalized, requested tag rebound, config and layer blobs unchanged",
        }
    except BaseException:
        # Never remove a path after an identity check: another process may have
        # replaced it before removal. Exclusive rename removes the private name
        # atomically on success; a randomized hidden partial may remain after a
        # pre-publication failure and is safer than deleting foreign state.
        try:
            os.fsync(parent_descriptor)
        except OSError:
            pass
        raise
    finally:
        os.close(parent_descriptor)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--input", required=True)
    result.add_argument("--output", required=True)
    result.add_argument("--base-lock", required=True)
    result.add_argument("--source-commit", required=True)
    result.add_argument("--source-date-epoch", required=True)
    result.add_argument("--input-repository-tag", required=True)
    result.add_argument("--output-repository-tag", required=True)
    result.add_argument("--version", required=True)
    result.add_argument("--image-id", required=True)
    return result


def main() -> int:
    def interrupted(signum: int, _frame: object) -> None:
        raise InterruptedError(f"interrupted by signal {signum}")

    for selected_signal in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(selected_signal, interrupted)
    try:
        result = canonicalize(parser().parse_args())
    except (ArchiveError, OSError, tarfile.TarError) as error:
        print(f"canonicalize-docker-archive: {error}", file=sys.stderr)
        return 65
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
