# SPDX-License-Identifier: AGPL-3.0-only
import json
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "finalize-container-runtime-evidence.py"
COMMIT = "a" * 40
PRE_FILES = (
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
IMAGE_ID = "sha256:" + "5" * 64
COHORT = {
    "compose_project": "hub-f6-test-r2",
    "image_id": IMAGE_ID,
    "container_names": ["hub-f6-test-r2-hub-1", "hub-f6-test-r2-volume-init-1"],
    "network_name": "hub-f6-test-r2_default",
    "volume_name": "hub-f6-test-r2_hub-data",
    "guest_runtime_root": "/var/tmp/hub-f6-test-r2",
    "host_runtime_root": "/tmp/hub-f6-test-r2",
}
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


def fixture(path: Path) -> None:
    path.mkdir()
    source_content = b"ok\n"
    source_manifest = hashlib.sha256(source_content).hexdigest().encode() + b"  Cargo.toml\n"
    archive_buffer = io.BytesIO()
    with tarfile.open(
        fileobj=archive_buffer,
        mode="w",
        format=tarfile.PAX_FORMAT,
        pax_headers={"comment": COMMIT},
    ) as archive:
        directory = tarfile.TarInfo("source/")
        directory.type = tarfile.DIRTYPE
        directory.mode = 0o755
        directory.mtime = 0
        archive.addfile(directory)
        member = tarfile.TarInfo("source/Cargo.toml")
        member.size = len(source_content)
        member.mode = 0o644
        member.mtime = 0
        archive.addfile(member, io.BytesIO(source_content))
    source_archive = archive_buffer.getvalue()
    blob = hashlib.sha1(b"blob 3\0" + source_content).digest()
    source_tree = hashlib.sha1(b"tree 38\0" + b"100644 Cargo.toml\0" + blob).hexdigest()
    defaults = {name: b"ok\n" for name in PRE_FILES}
    defaults.update(
        {
            "source-archive.json": json.dumps(
                {
                    "source_commit": COMMIT,
                    "archive_sha256": hashlib.sha256(source_archive).hexdigest(),
                    "archive_bytes": len(source_archive),
                    "tree": source_tree,
                    "manifest_sha256": hashlib.sha256(source_manifest).hexdigest(),
                    "member_count": 1,
                }
            ).encode(),
            "source-archive.tar": source_archive,
            "host-source-manifest.sha256": source_manifest,
            "guest-source-manifest.sha256": source_manifest,
            "build-summary.json": json.dumps(
                {"status": "passed", "source_commit": COMMIT, "cohort": COHORT}
            ).encode(),
            "tooling.json": b'{"tooling_installed_during_run":false,"package_manager_mutated":false}\n',
            "image-inspect.json": json.dumps(
                [{"Id": IMAGE_ID, "Os": "linux", "Architecture": "arm64", "Config": {"Env": ["PATH=/usr/bin"]}}]
            ).encode(),
            "binary-sha256.txt": b"3" * 64 + b"  /usr/local/bin/teslatlas-hub\n",
            "database-sha256.txt": b"4" * 64 + b"  hub.sqlite\n",
            "source-output.txt": (
                f"https://github.com/magrathean-uk/teslatlas-hub/tree/{COMMIT}\n"
            ).encode(),
            "compose-rendered.json": json.dumps(
                {
                    "services": {
                        "hub": {
                            "depends_on": {
                                "volume-init": {
                                    "condition": "service_completed_successfully",
                                    "required": True,
                                }
                            }
                        },
                        "volume-init": {},
                    }
                }
            ).encode(),
            "volume-init.json": b'{"status":"passed"}\n',
            "volume-permissions.json": b'{"root":"10001:10001:700"}\n',
            "runtime-security.json": b'{"uid":10001,"gid":10001,"read_only_root":true,"cap_drop":["ALL"],"no_new_privileges":true}\n',
            "tls-positive.json": b'{"passed":true}\n',
            "tls-negative.json": b'{"wrong_name_rejected":true}\n',
            "restart.json": b'{"healthy_after":true,"identity_continuity":true,"status_continuity":true}\n',
            "health-before.json": b'{"status":"ok"}\n',
            "doctor-before.json": b'{"status":"ok"}\n',
            "status-before.json": b'{"status":"ok"}\n',
            "health-after.json": b'{"status":"ok"}\n',
            "doctor-after.json": b'{"status":"ok"}\n',
            "status-after.json": b'{"status":"ok"}\n',
        }
    )
    for name, data in defaults.items():
        (path / name).write_bytes(data)


def cleanup_record(bundle: Path) -> dict[str, object]:
    manifest = json.loads((bundle / "manifest.json").read_text())
    return {
        **{flag: True for flag in CLEANUP_FLAGS},
        "source_commit": manifest["source_commit"],
        "evidence_run_id": manifest["evidence_run_id"],
        "cohort": manifest["cohort"],
    }


class EvidenceBundleTests(unittest.TestCase):
    def run_script(self, *arguments: object) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(SCRIPT), *(str(argument) for argument in arguments)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_two_phase_bundle_is_hash_bound_and_complete(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            bundle = root / "bundle"
            cleanup = root / "cleanup.json"
            fixture(source)
            prepared = self.run_script(
                "prepare", "--input", source, "--output", bundle, "--source-commit", COMMIT
            )
            self.assertEqual(prepared.returncode, 0, prepared.stderr)
            manifest = json.loads((bundle / "manifest.json").read_text())
            self.assertEqual(manifest["state"], "READY_FOR_CLEANUP")
            self.assertEqual(len(manifest["files"]), len(PRE_FILES))
            cleanup.write_text(json.dumps(cleanup_record(bundle)))
            finalized = self.run_script("finalize", "--bundle", bundle, "--cleanup", cleanup)
            self.assertEqual(finalized.returncode, 0, finalized.stderr)
            manifest = json.loads((bundle / "manifest.json").read_text())
            self.assertEqual(manifest["state"], "COMPLETE")
            self.assertEqual(len(manifest["files"]), len(PRE_FILES) + 1)
            self.assertRegex(manifest["aggregate_manifest_sha256"], r"^[0-9a-f]{64}$")
            self.assertEqual(os.stat(bundle / "files" / "cleanup.json").st_mode & 0o777, 0o600)

    def test_prepare_rejects_missing_secret_or_symlink_input(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            fixture(source)
            (source / "restart.json").unlink()
            result = self.run_script(
                "prepare", "--input", source, "--output", root / "missing", "--source-commit", COMMIT
            )
            self.assertNotEqual(result.returncode, 0)
            fixture_file = source / "restart.json"
            fixture_file.write_text('{"healthy_after":true,"identity_continuity":true,"status_continuity":true}\n')
            (source / "tls-positive.json").write_text("-----BEGIN PRIVATE KEY-----\n")
            result = self.run_script(
                "prepare", "--input", source, "--output", root / "secret", "--source-commit", COMMIT
            )
            self.assertNotEqual(result.returncode, 0)
            (source / "tls-positive.json").write_text('{"passed":true}\n')
            target = root / "source-target.txt"
            target.write_text("ok\n")
            (source / "source-output.txt").unlink()
            (source / "source-output.txt").symlink_to(target)
            result = self.run_script(
                "prepare", "--input", source, "--output", root / "symlink", "--source-commit", COMMIT
            )
            self.assertNotEqual(result.returncode, 0)

    def test_prepare_rejects_manifest_not_bound_to_archive_record(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            fixture(source)
            archive = json.loads((source / "source-archive.json").read_text())
            archive["member_count"] = 2
            (source / "source-archive.json").write_text(json.dumps(archive))
            result = self.run_script(
                "prepare", "--input", source, "--output", root / "bundle", "--source-commit", COMMIT
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("Traceback", result.stderr)

            fixture(source := root / "source-two")
            archive = json.loads((source / "source-archive.json").read_text())
            archive["tree"] = "f" * 40
            (source / "source-archive.json").write_text(json.dumps(archive))
            result = self.run_script(
                "prepare", "--input", source, "--output", root / "tree-mismatch", "--source-commit", COMMIT
            )
            self.assertNotEqual(result.returncode, 0)

            fixture(source := root / "source-three")
            archive = json.loads((source / "source-archive.json").read_text())
            archive["archive_sha256"] = "f" * 64
            (source / "source-archive.json").write_text(json.dumps(archive))
            result = self.run_script(
                "prepare", "--input", source, "--output", root / "archive-mismatch", "--source-commit", COMMIT
            )
            self.assertNotEqual(result.returncode, 0)

    def test_prepare_rejects_common_secret_spellings(self) -> None:
        for index, secret in enumerate(
            (
                b"access_token=SHOULD_NOT_SURVIVE\n",
                b"api_key=SHOULD_NOT_SURVIVE\n",
                b"authorization: bearer SHOULD_NOT_SURVIVE\n",
                b"-----BEGIN OPENSSH PRIVATE KEY-----\n",
                b"-----BEGIN ENCRYPTED PRIVATE KEY-----\n",
            )
        ):
            with self.subTest(secret=secret):
                with tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    source = root / "source"
                    fixture(source)
                    (source / "version.txt").write_bytes(secret)
                    result = self.run_script(
                        "prepare", "--input", source, "--output", root / f"bundle-{index}", "--source-commit", COMMIT
                    )
                    self.assertNotEqual(result.returncode, 0)
        for index, structured in enumerate(
            (
                {"nested": {"token": "SHOULD_NOT_SURVIVE"}},
                {"Secret": "SHOULD_NOT_SURVIVE"},
                {"apiToken": "SHOULD_NOT_SURVIVE"},
                {"access_token": "SHOULD_NOT_SURVIVE"},
            )
        ):
            with self.subTest(structured=structured):
                with tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    source = root / "source"
                    fixture(source)
                    (source / "health-before.json").write_text(
                        json.dumps({"status": "ok", **structured})
                    )
                    result = self.run_script(
                        "prepare", "--input", source, "--output", root / f"structured-{index}", "--source-commit", COMMIT
                    )
                    self.assertNotEqual(result.returncode, 0)

    def test_finalize_rejects_incomplete_cleanup(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            bundle = root / "bundle"
            cleanup = root / "cleanup.json"
            fixture(source)
            self.assertEqual(
                self.run_script(
                    "prepare", "--input", source, "--output", bundle, "--source-commit", COMMIT
                ).returncode,
                0,
            )
            values = cleanup_record(bundle)
            values["heavy_build_lock_released"] = False
            cleanup.write_text(json.dumps(values))
            result = self.run_script("finalize", "--bundle", bundle, "--cleanup", cleanup)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(json.loads((bundle / "manifest.json").read_text())["state"], "READY_FOR_CLEANUP")

    def test_finalize_rejects_cleanup_for_another_cohort(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            bundle = root / "bundle"
            cleanup = root / "cleanup.json"
            fixture(source)
            self.assertEqual(
                self.run_script(
                    "prepare", "--input", source, "--output", bundle, "--source-commit", COMMIT
                ).returncode,
                0,
            )
            values = cleanup_record(bundle)
            values["source_commit"] = "b" * 40
            values["cohort"] = {**COHORT, "compose_project": "foreign-cohort"}
            cleanup.write_text(json.dumps(values))
            result = self.run_script("finalize", "--bundle", bundle, "--cleanup", cleanup)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(json.loads((bundle / "manifest.json").read_text())["state"], "READY_FOR_CLEANUP")

    def test_finalize_rejects_pre_cleanup_file_tampering(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            bundle = root / "bundle"
            cleanup = root / "cleanup.json"
            fixture(source)
            self.assertEqual(
                self.run_script(
                    "prepare", "--input", source, "--output", bundle, "--source-commit", COMMIT
                ).returncode,
                0,
            )
            (bundle / "files" / "restart.json").write_text("changed\n")
            cleanup.write_text(json.dumps(cleanup_record(bundle)))
            result = self.run_script("finalize", "--bundle", bundle, "--cleanup", cleanup)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((bundle / "files" / "cleanup.json").exists())


if __name__ == "__main__":
    unittest.main()
