#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/canonicalize-docker-archive.py"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
EPOCH = 1_700_000_000
VERSION = "2026.36.2"
INPUT_TAG = "teslatlas-hub-build-cohort:test"
OUTPUT_TAG = "teslatlas-hub:test"
SOURCE_URL = "https://github.com/magrathean-uk/teslatlas-hub"
EMPTY_LAYER = b"\0" * 1024
EMPTY_DIGEST = hashlib.sha256(EMPTY_LAYER).hexdigest()
OCI_LAYER = "application/vnd.oci.image.layer.v1.tar"


def json_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def layer_bytes(name: str, data: bytes, mtime: int) -> bytes:
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as archive:
        directory = tarfile.TarInfo("usr/")
        directory.type = tarfile.DIRTYPE
        directory.mode = 0o755
        directory.mtime = mtime
        archive.addfile(directory)
        member = tarfile.TarInfo(f"usr/{name}")
        member.mode = 0o755
        member.mtime = mtime
        member.size = len(data)
        archive.addfile(member, io.BytesIO(data))
    return output.getvalue()


def special_layer_bytes() -> bytes:
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.PAX_FORMAT) as archive:
        member = tarfile.TarInfo("unsafe-fifo")
        member.type = tarfile.FIFOTYPE
        member.mtime = EPOCH
        archive.addfile(member)
    return output.getvalue()


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


class Fixture:
    def __init__(self, root: Path) -> None:
        self.base = layer_bytes("base", b"base", EPOCH - 100)
        self.application = layer_bytes("hub", b"hub", EPOCH)
        self.empty = EMPTY_LAYER
        self.base_digest = digest(self.base)
        self.application_digest = digest(self.application)
        self.lock = root / "base-images.json"
        self.lock.write_bytes(json_bytes({"images": {"runtime": {
            "linux_arm64_rootfs_diff_ids": [f"sha256:{self.base_digest}"]
        }}}))

    @staticmethod
    def labels() -> dict[str, str]:
        return {
            "org.opencontainers.image.title": "Teslatlas Hub",
            "org.opencontainers.image.version": VERSION,
            "org.opencontainers.image.source": SOURCE_URL,
            "org.opencontainers.image.revision": COMMIT,
        }

    @staticmethod
    def empty_container_config() -> dict:
        return {
            "Hostname": "", "Domainname": "", "User": "", "AttachStdin": False,
            "AttachStdout": False, "AttachStderr": False, "Tty": False,
            "OpenStdin": False, "StdinOnce": False, "Env": None, "Cmd": None,
            "Image": "", "Volumes": None, "WorkingDir": "", "Entrypoint": None,
            "OnBuild": None, "Labels": None,
        }

    def legacy_runtime(self) -> dict:
        return {
            **self.empty_container_config(),
            "User": "10001:10001",
            "Env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt",
            ],
            "Cmd": ["--config", "/etc/teslatlas-hub/config.toml", "serve"],
            "ArgsEscaped": True,
            "WorkingDir": "/var/lib/teslatlas-hub",
            "Entrypoint": ["/usr/local/bin/teslatlas-hub"],
            "Labels": self.labels(),
        }

    def config(self) -> dict:
        return {
            "architecture": "arm64", "os": "linux", "created": "2023-11-14T22:13:20Z",
            "config": {
                "User": "10001:10001",
                "Env": [
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt",
                ],
                "Entrypoint": ["/usr/local/bin/teslatlas-hub"],
                "Cmd": ["--config", "/etc/teslatlas-hub/config.toml", "serve"],
                "WorkingDir": "/var/lib/teslatlas-hub", "Labels": self.labels(),
                "ArgsEscaped": True,
            },
            "rootfs": {"type": "layers", "diff_ids": [
                f"sha256:{self.base_digest}", f"sha256:{self.application_digest}",
                f"sha256:{EMPTY_DIGEST}",
            ]},
            "history": [
                {"created": "2023-11-14T22:11:40Z", "created_by": "base"},
                {"created": "2023-11-14T22:13:20Z", "created_by": "ARG", "empty_layer": True},
                {"created": "2023-11-14T22:13:20Z", "created_by": "COPY hub"},
                {"created": "2023-11-14T22:13:20Z", "created_by": "LABEL", "empty_layer": True},
                {"created": "2023-11-14T22:13:20Z", "created_by": "WORKDIR"},
                {"created": "2023-11-14T22:13:20Z", "created_by": "CMD", "empty_layer": True},
            ],
        }

    def write(
        self, path: Path, *, mtime: int, reverse: bool = False, mutate_config=None,
        add_extra: bool = False, duplicate_manifest: bool = False,
        symlink_member: bool = False, fifo_member: bool = False,
        unsafe_member: bool = False, pax_member: bool = False,
        corrupt_application_layer: bool = False, duplicate_config_key: bool = False,
        bad_oci_descriptor: bool = False, bad_legacy_chain: bool = False,
        repository_tag: str = INPUT_TAG,
    ) -> str:
        config = self.config()
        if mutate_config is not None:
            mutate_config(config)
        config_data = (b'{"architecture":"arm64","architecture":"arm64"}'
                       if duplicate_config_key else json_bytes(config))
        config_digest = digest(config_data)
        application_data = self.application + (b"corrupt" if corrupt_application_layer else b"")
        layer_digests = [self.base_digest, self.application_digest, EMPTY_DIGEST]
        layer_data = [self.base, application_data, self.empty]
        layer_paths = [f"blobs/sha256/{value}" for value in layer_digests]
        layer_sources = {
            f"sha256:{value}": {"mediaType": OCI_LAYER, "size": len(data),
                                 "digest": f"sha256:{value}"}
            for value, data in zip(layer_digests, layer_data)
        }
        oci_layers = list(layer_sources.values())
        if bad_oci_descriptor:
            oci_layers[0] = {**oci_layers[0], "size": oci_layers[0]["size"] + 1}
        oci_manifest_data = json_bytes({
            "schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                       "digest": f"sha256:{config_digest}", "size": len(config_data)},
            "layers": oci_layers,
        })
        oci_manifest_digest = digest(oci_manifest_data)
        root_id, middle_id, final_id = "1" * 64, "2" * 64, "3" * 64
        legacy = [
            {"id": root_id, "created": "1970-01-01T01:00:00+01:00",
             "container_config": self.empty_container_config(), "os": "linux"},
            {"id": middle_id, "parent": "f" * 64 if bad_legacy_chain else root_id,
             "created": "1970-01-01T01:00:00+01:00",
             "container_config": self.empty_container_config(), "os": "linux"},
            {"id": final_id, "parent": middle_id, "created": "2023-11-14T22:13:20Z",
             "container_config": self.empty_container_config(),
             "config": self.legacy_runtime(), "architecture": "arm64", "os": "linux"},
        ]
        blobs = {config_digest: config_data, self.base_digest: self.base,
                 self.application_digest: application_data, EMPTY_DIGEST: self.empty,
                 oci_manifest_digest: oci_manifest_data}
        for value in legacy:
            data = json_bytes(value)
            blobs[digest(data)] = data
        outer_manifest = [{"Config": f"blobs/sha256/{config_digest}",
                           "RepoTags": [repository_tag], "Layers": layer_paths,
                           "LayerSources": layer_sources}]
        repository, tag = repository_tag.rsplit(":", 1)
        index = {"schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json",
                 "manifests": [{"mediaType": "application/vnd.oci.image.manifest.v1+json",
                                "digest": f"sha256:{oci_manifest_digest}",
                                "size": len(oci_manifest_data),
                                "annotations": {
                                    "io.containerd.image.name": f"docker.io/library/{repository_tag}",
                                    "org.opencontainers.image.ref.name": tag,
                                }}]}
        files = [(f"blobs/sha256/{name}", data) for name, data in blobs.items()]
        files += [("index.json", json_bytes(index)), ("manifest.json", json_bytes(outer_manifest)),
                  ("oci-layout", json_bytes({"imageLayoutVersion": "1.0.0"})),
                  ("repositories", json_bytes({repository: {tag: layer_digests[-1]}}))]
        if add_extra:
            extra = b"unreferenced"
            files.append((f"blobs/sha256/{digest(extra)}", extra))
        if reverse:
            files.reverse()
        with tarfile.open(path, "w", format=tarfile.PAX_FORMAT) as archive:
            directories = ("blobs/sha256/", "blobs/") if reverse else ("blobs/", "blobs/sha256/")
            for directory_name in directories:
                directory = tarfile.TarInfo(directory_name)
                directory.type, directory.mode, directory.uid, directory.gid = tarfile.DIRTYPE, 0o700, 501, 20
                directory.mtime = mtime
                archive.addfile(directory)
            for name, data in files:
                member = tarfile.TarInfo(name)
                member.mode, member.uid, member.gid, member.mtime, member.size = 0o600, 501, 20, mtime, len(data)
                archive.addfile(member, io.BytesIO(data))
                if duplicate_manifest and name == "manifest.json":
                    archive.addfile(member, io.BytesIO(data))
            for enabled, name, kind in ((symlink_member, "unsafe-link", tarfile.SYMTYPE),
                                        (fifo_member, "unsafe-fifo", tarfile.FIFOTYPE)):
                if enabled:
                    member = tarfile.TarInfo(name)
                    member.type = kind
                    member.linkname = "manifest.json" if kind == tarfile.SYMTYPE else ""
                    archive.addfile(member)
            if unsafe_member:
                member = tarfile.TarInfo("../escape")
                member.size = 1
                archive.addfile(member, io.BytesIO(b"x"))
            if pax_member:
                member = tarfile.TarInfo("pax-member")
                member.pax_headers, member.size = {"comment": "ambiguous"}, 1
                archive.addfile(member, io.BytesIO(b"x"))
        return f"sha256:{config_digest}"

    def run(self, source: Path, output: Path, image_id: str, *, epoch: str = str(EPOCH),
            input_tag: str = INPUT_TAG, output_tag: str = OUTPUT_TAG):
        return subprocess.run([
            sys.executable, str(SCRIPT), "--input", str(source), "--output", str(output),
            "--base-lock", str(self.lock), "--source-commit", COMMIT,
            "--source-date-epoch", epoch, "--input-repository-tag", input_tag,
            "--output-repository-tag", output_tag, "--version", VERSION,
            "--image-id", image_id,
        ], text=True, capture_output=True, check=False)


class CanonicalizeDockerArchiveTests(unittest.TestCase):
    def test_hybrid_transport_variance_canonicalizes_to_identical_minimal_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root, fixture = Path(temporary), Fixture(Path(temporary))
            first, second = root / "first.tar", root / "second.tar"
            first_tag, second_tag = "teslatlas-hub-build-cohort:first", "teslatlas-hub-build-cohort:second"
            first_id = fixture.write(first, mtime=EPOCH + 10, repository_tag=first_tag)
            second_id = fixture.write(second, mtime=EPOCH + 99, reverse=True, repository_tag=second_tag)
            first_output, second_output = root / "first-out.tar", root / "second-out.tar"
            first_result = fixture.run(first, first_output, first_id, input_tag=first_tag)
            second_result = fixture.run(second, second_output, second_id, input_tag=second_tag)
            self.assertEqual(first_result.returncode, 0, first_result.stderr)
            self.assertEqual(second_result.returncode, 0, second_result.stderr)
            self.assertEqual(first_output.read_bytes(), second_output.read_bytes())
            self.assertEqual(list(root.glob(".*.tmp.*")), [])
            with tarfile.open(first_output, "r:") as archive:
                names = set(archive.getnames())
                manifest = json.load(archive.extractfile("manifest.json"))
                repositories = json.load(archive.extractfile("repositories"))
            expected = {"blobs", "blobs/sha256", "manifest.json", "repositories",
                        manifest[0]["Config"], *manifest[0]["Layers"]}
            self.assertEqual(names, expected)
            self.assertEqual(manifest[0]["RepoTags"], [OUTPUT_TAG])
            self.assertEqual(repositories, {"teslatlas-hub": {"test": EMPTY_DIGEST}})

    def test_rejects_wrong_config_bindings_and_unknown_runtime_fields(self) -> None:
        cases = {
            "wall clock config": lambda v: v.__setitem__("created", "2023-11-14T22:13:21Z"),
            "wall clock history": lambda v: v["history"][-1].__setitem__("created", "2023-11-14T22:13:21Z"),
            "wrong source": lambda v: v["config"]["Labels"].__setitem__("org.opencontainers.image.revision", "f" * 40),
            "wrong platform": lambda v: v.__setitem__("architecture", "amd64"),
            "wrong runtime": lambda v: v["config"].__setitem__("User", "0:0"),
            "unknown runtime": lambda v: v["config"].__setitem__("NetworkDisabled", True),
            "wrong ArgsEscaped": lambda v: v["config"].__setitem__("ArgsEscaped", False),
            "invalid empty layer": lambda v: v["history"][-1].__setitem__("empty_layer", "true"),
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for index, (name, mutation) in enumerate(cases.items()):
                with self.subTest(name=name):
                    case_root = root / str(index)
                    case_root.mkdir()
                    fixture = Fixture(case_root)
                    source, output = case_root / "source.tar", case_root / "output.tar"
                    image_id = fixture.write(source, mtime=EPOCH, mutate_config=mutation)
                    result = fixture.run(source, output, image_id)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(output.exists())

    def test_rejects_ambiguous_corrupt_or_unreferenced_hybrid_members(self) -> None:
        options = ({"add_extra": True}, {"duplicate_manifest": True}, {"symlink_member": True},
                   {"fifo_member": True}, {"unsafe_member": True}, {"pax_member": True},
                   {"corrupt_application_layer": True}, {"duplicate_config_key": True},
                   {"bad_oci_descriptor": True}, {"bad_legacy_chain": True})
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for index, option in enumerate(options):
                with self.subTest(option=option):
                    case_root = root / str(index)
                    case_root.mkdir()
                    fixture = Fixture(case_root)
                    source, output = case_root / "source.tar", case_root / "output.tar"
                    image_id = fixture.write(source, mtime=EPOCH, **option)
                    result = fixture.run(source, output, image_id)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(output.exists())

    def test_rejects_special_or_wrong_mtime_staged_application_layer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for index, application in enumerate((special_layer_bytes(), layer_bytes("hub", b"hub", EPOCH + 999))):
                case_root = root / str(index)
                case_root.mkdir()
                fixture = Fixture(case_root)
                fixture.application, fixture.application_digest = application, digest(application)
                source, output = case_root / "source.tar", case_root / "output.tar"
                image_id = fixture.write(source, mtime=EPOCH)
                result = fixture.run(source, output, image_id)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(output.exists())

    def test_rejects_input_tag_mismatch_and_existing_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root, fixture = Path(temporary), Fixture(Path(temporary))
            source = root / "source.tar"
            image_id = fixture.write(source, mtime=EPOCH)
            mismatch = root / "mismatch.tar"
            result = fixture.run(source, mismatch, image_id,
                                 input_tag="teslatlas-hub-build-cohort:foreign")
            self.assertNotEqual(result.returncode, 0)
            existing = root / "existing.tar"
            existing.write_bytes(b"preserve")
            result = fixture.run(source, existing, image_id)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(existing.read_bytes(), b"preserve")

    def test_rejects_symlink_input_and_base_lock(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root, fixture = Path(temporary), Fixture(Path(temporary))
            source = root / "source.tar"
            image_id = fixture.write(source, mtime=EPOCH)
            linked = root / "linked.tar"
            linked.symlink_to(source.name)
            self.assertNotEqual(fixture.run(linked, root / "out.tar", image_id).returncode, 0)
            real_lock = root / "real-lock.json"
            fixture.lock.replace(real_lock)
            fixture.lock.symlink_to(real_lock.name)
            self.assertNotEqual(fixture.run(source, root / "lock-out.tar", image_id).returncode, 0)

    def test_atomic_publication_refuses_existing_output_without_deleting_either(self) -> None:
        spec = importlib.util.spec_from_file_location("canonicalize_docker_archive", SCRIPT)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            private, output = root / ".output.tar.tmp.private", root / "output.tar"
            private.write_bytes(b"candidate")
            output.write_bytes(b"foreign")
            parent = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            try:
                with self.assertRaises(module.ArchiveError):
                    module.atomic_publish_noreplace(parent, private.name, output.name)
            finally:
                os.close(parent)
            self.assertEqual(private.read_bytes(), b"candidate")
            self.assertEqual(output.read_bytes(), b"foreign")


if __name__ == "__main__":
    unittest.main()
