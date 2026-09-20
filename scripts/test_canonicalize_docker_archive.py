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


class Fixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.base = layer_bytes("base", b"base", EPOCH - 100)
        self.application = layer_bytes("hub", b"hub", EPOCH)
        self.base_diff = f"sha256:{hashlib.sha256(self.base).hexdigest()}"
        self.application_diff = f"sha256:{hashlib.sha256(self.application).hexdigest()}"
        self.layer_ids = ["a" * 64, "b" * 64]
        self.lock = root / "base-images.json"
        self.lock.write_bytes(
            json_bytes(
                {
                    "images": {
                        "runtime": {"linux_arm64_rootfs_diff_ids": [self.base_diff]}
                    }
                }
            )
        )

    def config(self) -> dict:
        return {
            "architecture": "arm64",
            "variant": "v8",
            "os": "linux",
            "created": "2023-11-14T22:13:20Z",
            "config": {
                "User": "10001:10001",
                "Env": [
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                    "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt",
                ],
                "Entrypoint": ["/usr/local/bin/teslatlas-hub"],
                "Cmd": ["--config", "/etc/teslatlas-hub/config.toml", "serve"],
                "WorkingDir": "/var/lib/teslatlas-hub",
                "Labels": {
                    "org.opencontainers.image.title": "Teslatlas Hub",
                    "org.opencontainers.image.version": VERSION,
                    "org.opencontainers.image.source": SOURCE_URL,
                    "org.opencontainers.image.revision": COMMIT,
                },
            },
            "rootfs": {"type": "layers", "diff_ids": [self.base_diff, self.application_diff]},
            "history": [
                {"created": "2023-11-14T22:11:40Z", "created_by": "base"},
                {"created": "2023-11-14T22:11:40Z", "created_by": "base cmd", "empty_layer": True},
                {"created": "2023-11-14T22:13:20Z", "created_by": "COPY hub"},
                {"created": "2023-11-14T22:13:20Z", "created_by": "USER", "empty_layer": True},
            ],
        }

    def write(
        self,
        path: Path,
        *,
        mtime: int,
        reverse: bool = False,
        mutate_config=None,
        add_extra: bool = False,
        duplicate_manifest: bool = False,
        symlink_member: bool = False,
        fifo_member: bool = False,
        unsafe_member: bool = False,
        pax_member: bool = False,
        corrupt_application_layer: bool = False,
        duplicate_config_key: bool = False,
        repository_tag: str = INPUT_TAG,
    ) -> str:
        config = self.config()
        if mutate_config is not None:
            mutate_config(config)
        if duplicate_config_key:
            config_data = b'{"architecture":"arm64","architecture":"arm64"}'
        else:
            config_data = json_bytes(config)
        image_id = hashlib.sha256(config_data).hexdigest()
        manifest = [
            {
                "Config": f"{image_id}.json",
                "RepoTags": [repository_tag],
                "Layers": [f"{value}/layer.tar" for value in self.layer_ids],
            }
        ]
        files: list[tuple[str, bytes]] = [
            (f"{image_id}.json", config_data),
            (f"{self.layer_ids[0]}/VERSION", b"1.0"),
            (f"{self.layer_ids[0]}/json", json_bytes({"id": self.layer_ids[0]})),
            (f"{self.layer_ids[0]}/layer.tar", self.base),
            (f"{self.layer_ids[1]}/VERSION", b"1.0"),
            (
                f"{self.layer_ids[1]}/json",
                json_bytes({"id": self.layer_ids[1], "parent": self.layer_ids[0]}),
            ),
            (
                f"{self.layer_ids[1]}/layer.tar",
                self.application + (b"corrupt" if corrupt_application_layer else b""),
            ),
            ("manifest.json", json_bytes(manifest)),
            (
                "repositories",
                json_bytes(
                    {
                        repository_tag.rsplit(":", 1)[0]: {
                            repository_tag.rsplit(":", 1)[1]: self.layer_ids[-1]
                        }
                    }
                ),
            ),
        ]
        if add_extra:
            files.append(("unreferenced", b"no"))
        if reverse:
            files.reverse()
        with tarfile.open(path, "w", format=tarfile.PAX_FORMAT) as archive:
            for layer_id in (reversed(self.layer_ids) if reverse else self.layer_ids):
                directory = tarfile.TarInfo(f"{layer_id}/")
                directory.type = tarfile.DIRTYPE
                directory.mode = 0o700
                directory.uid = 501
                directory.gid = 20
                directory.mtime = mtime
                archive.addfile(directory)
            for name, data in files:
                member = tarfile.TarInfo(name)
                member.mode = 0o600
                member.uid = 501
                member.gid = 20
                member.mtime = mtime
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
                if duplicate_manifest and name == "manifest.json":
                    archive.addfile(member, io.BytesIO(data))
            if symlink_member:
                member = tarfile.TarInfo("unsafe-link")
                member.type = tarfile.SYMTYPE
                member.linkname = "manifest.json"
                archive.addfile(member)
            if fifo_member:
                member = tarfile.TarInfo("unsafe-fifo")
                member.type = tarfile.FIFOTYPE
                archive.addfile(member)
            if unsafe_member:
                member = tarfile.TarInfo("../escape")
                member.size = 1
                archive.addfile(member, io.BytesIO(b"x"))
            if pax_member:
                member = tarfile.TarInfo("pax-member")
                member.pax_headers = {"comment": "ambiguous"}
                member.size = 1
                archive.addfile(member, io.BytesIO(b"x"))
        return f"sha256:{image_id}"

    def run(
        self,
        source: Path,
        output: Path,
        image_id: str,
        *,
        epoch: str = str(EPOCH),
        input_tag: str = INPUT_TAG,
        output_tag: str = OUTPUT_TAG,
    ):
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--input",
                str(source),
                "--output",
                str(output),
                "--base-lock",
                str(self.lock),
                "--source-commit",
                COMMIT,
                "--source-date-epoch",
                epoch,
                "--input-repository-tag",
                input_tag,
                "--output-repository-tag",
                output_tag,
                "--version",
                VERSION,
                "--image-id",
                image_id,
            ],
            text=True,
            capture_output=True,
            check=False,
        )


class CanonicalizeDockerArchiveTests(unittest.TestCase):
    def test_outer_transport_variance_canonicalizes_to_identical_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = Fixture(root)
            first = root / "first.tar"
            second = root / "second.tar"
            first_input_tag = "teslatlas-hub-build-cohort:first"
            second_input_tag = "teslatlas-hub-build-cohort:second"
            first_id = fixture.write(
                first, mtime=EPOCH + 10, reverse=False, repository_tag=first_input_tag
            )
            second_id = fixture.write(
                second, mtime=EPOCH + 99, reverse=True, repository_tag=second_input_tag
            )
            self.assertEqual(first_id, second_id)
            first_output = root / "first-canonical.tar"
            second_output = root / "second-canonical.tar"
            first_result = fixture.run(first, first_output, first_id, input_tag=first_input_tag)
            second_result = fixture.run(second, second_output, second_id, input_tag=second_input_tag)
            self.assertEqual(first_result.returncode, 0, first_result.stderr)
            self.assertEqual(second_result.returncode, 0, second_result.stderr)
            self.assertEqual(first_output.read_bytes(), second_output.read_bytes())
            self.assertEqual(list(root.glob(".*.tmp.*")), [])
            report = json.loads(first_result.stdout)
            self.assertEqual(report["image_id"], first_id)
            self.assertEqual(report["repository_tag"], OUTPUT_TAG)
            self.assertIn("config and layer payload bytes unchanged", report["normalization"])
            with tarfile.open(first_output, "r:") as archive:
                manifest = json.load(archive.extractfile("manifest.json"))
                repositories = json.load(archive.extractfile("repositories"))
            self.assertEqual(manifest[0]["RepoTags"], [OUTPUT_TAG])
            self.assertEqual(repositories, {"teslatlas-hub": {"test": fixture.layer_ids[-1]}})

    def test_rejects_non_buildkit_and_wrong_bindings(self) -> None:
        cases = {
            "wall clock config": lambda config: config.__setitem__(
                "created", "2023-11-14T22:13:21Z"
            ),
            "wall clock history": lambda config: config["history"][-1].__setitem__(
                "created", "2023-11-14T22:13:21Z"
            ),
            "wrong source": lambda config: config["config"]["Labels"].__setitem__(
                "org.opencontainers.image.revision", "f" * 40
            ),
            "wrong platform": lambda config: config.__setitem__("architecture", "amd64"),
            "wrong runtime": lambda config: config["config"].__setitem__("User", "0:0"),
            "unknown runtime field": lambda config: config["config"].__setitem__(
                "NetworkDisabled", True
            ),
            "unknown config field": lambda config: config.__setitem__("unexpected", True),
            "invalid empty layer": lambda config: config["history"][-1].__setitem__(
                "empty_layer", "true"
            ),
            "wrong rootfs": lambda config: config["rootfs"].__setitem__(
                "diff_ids", ["sha256:" + "0" * 64, config["rootfs"]["diff_ids"][1]]
            ),
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for index, (name, mutation) in enumerate(cases.items()):
                with self.subTest(name=name):
                    case_root = root / str(index)
                    case_root.mkdir()
                    fixture = Fixture(case_root)
                    source = case_root / "source.tar"
                    image_id = fixture.write(source, mtime=EPOCH, mutate_config=mutation)
                    output = case_root / "output.tar"
                    result = fixture.run(source, output, image_id)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(output.exists())

    def test_rejects_ambiguous_or_unreferenced_archive_members(self) -> None:
        options = (
            {"add_extra": True},
            {"duplicate_manifest": True},
            {"symlink_member": True},
            {"fifo_member": True},
            {"unsafe_member": True},
            {"pax_member": True},
            {"corrupt_application_layer": True},
            {"duplicate_config_key": True},
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for index, option in enumerate(options):
                with self.subTest(option=option):
                    case_root = root / str(index)
                    case_root.mkdir()
                    fixture = Fixture(case_root)
                    source = case_root / "source.tar"
                    image_id = fixture.write(source, mtime=EPOCH, **option)
                    output = case_root / "output.tar"
                    result = fixture.run(source, output, image_id)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(output.exists())

    def test_rejects_special_application_layer(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = Fixture(root)
            fixture.application = special_layer_bytes()
            fixture.application_diff = f"sha256:{hashlib.sha256(fixture.application).hexdigest()}"
            source = root / "source.tar"
            image_id = fixture.write(source, mtime=EPOCH)
            output = root / "output.tar"
            result = fixture.run(source, output, image_id)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("link or special", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_application_layer_member_after_epoch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = Fixture(root)
            fixture.application = layer_bytes("hub", b"hub", EPOCH + 999)
            fixture.application_diff = f"sha256:{hashlib.sha256(fixture.application).hexdigest()}"
            source = root / "source.tar"
            image_id = fixture.write(source, mtime=EPOCH)
            output = root / "output.tar"
            result = fixture.run(source, output, image_id)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("mtime does not equal SOURCE_DATE_EPOCH", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_malformed_epoch_and_existing_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = Fixture(root)
            source = root / "source.tar"
            image_id = fixture.write(source, mtime=EPOCH)
            malformed = root / "malformed.tar"
            result = fixture.run(source, malformed, image_id, epoch="1700000000junk")
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(malformed.exists())
            existing = root / "existing.tar"
            existing.write_bytes(b"preserve")
            result = fixture.run(source, existing, image_id)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(existing.read_bytes(), b"preserve")

    def test_rejects_input_cohort_tag_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = Fixture(root)
            source = root / "source.tar"
            image_id = fixture.write(source, mtime=EPOCH)
            output = root / "output.tar"
            result = fixture.run(
                source,
                output,
                image_id,
                input_tag="teslatlas-hub-build-cohort:foreign",
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())

    def test_rejects_symlink_input_and_base_lock(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture = Fixture(root)
            source = root / "source.tar"
            image_id = fixture.write(source, mtime=EPOCH)

            linked_source = root / "linked-source.tar"
            linked_source.symlink_to(source.name)
            output = root / "source-output.tar"
            result = fixture.run(linked_source, output, image_id)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())

            real_lock = root / "real-lock.json"
            fixture.lock.replace(real_lock)
            fixture.lock.symlink_to(real_lock.name)
            output = root / "lock-output.tar"
            result = fixture.run(source, output, image_id)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())

    def test_atomic_publication_refuses_existing_output_without_deleting_either(self) -> None:
        spec = importlib.util.spec_from_file_location("canonicalize_docker_archive", SCRIPT)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            private = root / ".output.tar.tmp.private"
            output = root / "output.tar"
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
