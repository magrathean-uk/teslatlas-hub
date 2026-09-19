import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest

from . import adapter_wire


DIGEST = "a" * 64
SESSION = "11111111-1111-4111-8111-111111111111"


class AdapterWireTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def tearDown(self):
        self.temporary.cleanup()

    def _write(self, name, value):
        path = self.root / name
        raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\n"
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as stream:
            stream.write(raw)
        return {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}

    def test_strict_json_rejects_duplicate_and_trailing_values(self):
        for raw in (b'{"a":1,"a":2}\n', b'{"a":1} {"b":2}\n', b'{"x":NaN}\n'):
            with self.subTest(raw=raw):
                with self.assertRaises(adapter_wire.WireError):
                    adapter_wire.strict_json(raw)

    def test_viewer_wire_accepts_only_closed_https_cross_origin_contract(self):
        certificate = self._write("viewer-ca.pem", {"certificate": "fixture"})
        executable = self._write("chromium", {"binary": "fixture"})
        viewer = {
            "page": {
                "origin": "https://127.0.0.1:18481",
                "server_authority": "runner-owned-installed-viewer",
                "artifact_role": "viewer_package_tarball",
            },
            "hub": {
                "public_origin": "https://127.0.0.1:18480",
                "cors_allowed_origin": "https://127.0.0.1:18481",
                "cross_origin": True,
            },
            "trusted_ca": {
                "certificate": {
                    "id": "viewer_trusted_ca",
                    "root": certificate,
                    "local": certificate,
                },
                "certificate_der_sha256": DIGEST,
            },
            "browser": {
                "authority": "runner-bound-executable",
                "engine": "chromium",
                "version": "151.0.7922.34",
                "executable": executable,
            },
            "reservations": {
                name: str(self.root / name)
                for name in ("raw_evidence_dir", "browser_log", "close_record", "supplement")
            },
        }

        adapter_wire.validate_viewer_session_contract(viewer)

        invalid = [
            dict(viewer, page=dict(viewer["page"], origin="http://127.0.0.1:18481")),
            dict(viewer, hub=dict(viewer["hub"], cors_allowed_origin="https://localhost:18481")),
            dict(viewer, hub=dict(viewer["hub"], public_origin="https://127.0.0.1:18481")),
            dict(viewer, browser=dict(viewer["browser"], engine="webkit")),
            dict(viewer, extra=True),
        ]
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(adapter_wire.WireError):
                adapter_wire.validate_viewer_session_contract(value)

    def test_ready_is_bound_to_fresh_file_bytes_and_exact_identity(self):
        evidence = self._write("completion.json", {"schema_version": 1})
        ready = {
            "schema_version": 1, "type": "ready", "session_id": SESSION,
            "cell_id": "typescript_node__macos_arm64",
            "session_input_sha256": "b" * 64, "instance_nonce": "c" * 64,
            "sequence": 1, "phase": "evidence_ready",
            "observation": {"session_sequence": 7, "proof_sha256": "d" * 64},
            "evidence": evidence,
        }
        admitted = adapter_wire.validate_ready(
            ready, session_id=SESSION, cell_id=ready["cell_id"],
            session_input_sha256="b" * 64, instance_nonce="c" * 64,
            sequence=1, allowed_phases={"evidence_ready"},
            observation={"session_sequence": 7, "proof_sha256": "d" * 64},
        )
        self.assertEqual(b'{"schema_version":1}\n', admitted.evidence_bytes)
        Path(evidence["path"]).write_bytes(b"changed\n")
        with self.assertRaises(adapter_wire.WireError):
            adapter_wire.validate_ready(
                ready, session_id=SESSION, cell_id=ready["cell_id"],
                session_input_sha256="b" * 64, instance_nonce="c" * 64,
                sequence=1, allowed_phases={"evidence_ready"},
                observation={"session_sequence": 7, "proof_sha256": "d" * 64},
            )

    def test_ack_is_exclusive_and_binds_exact_ready_bytes(self):
        ready = self._write("ready-000001.json", {"schema_version": 1, "type": "ready"})
        target = self.root / "ack-000001.json"
        ack = adapter_wire.write_ack(
            target, session_id=SESSION, cell_id="typescript_node__macos_arm64",
            session_input_sha256="b" * 64, instance_nonce="c" * 64,
            sequence=1, ready_binding=ready, phase="evidence_ready",
            status="accepted", action="close_completed", result=None,
        )
        self.assertEqual(hashlib.sha256(Path(ready["path"]).read_bytes()).hexdigest(), ack["ready_sha256"])
        self.assertEqual(0o600, target.stat().st_mode & 0o777)
        with self.assertRaises(adapter_wire.WireError):
            adapter_wire.write_ack(
                target, session_id=SESSION, cell_id="typescript_node__macos_arm64",
                session_input_sha256="b" * 64, instance_nonce="c" * 64,
                sequence=1, ready_binding=ready, phase="evidence_ready",
                status="accepted", action="close_completed", result=None,
            )

    def test_reviewed_contract_loader_rejects_unregistered_and_changed_validator(self):
        with self.assertRaises(adapter_wire.ContractPending):
            adapter_wire.load_reviewed_contract("swift", {})
        validator = self.root / "contract.py"
        validator.write_text(
            "ADAPTER_ID='typescript_node'\nCONTRACT_REVISION=1\n"
            "DECISION_CODES=frozenset({'accepted'})\n"
            "def admit_case(case, context): return ('passed','accepted')\n",
            encoding="utf-8",
        )
        validator.chmod(0o600)
        manifest = self._write("manifest.json", {
            "schema_version": 1, "adapter_id": "typescript_node", "revision": 1,
            "required_cases": ["candidate_artifact_identity"], "actors": [],
            "cases": [], "raw_schemas": [], "phases": [],
            "validator": {"path": str(validator), "sha256": adapter_wire.sha256_file(validator)},
        })
        registry = {"typescript_node": adapter_wire.ReviewedContract(
            manifest_path=manifest["path"], manifest_sha256=manifest["sha256"],
            validator_path=str(validator), validator_sha256=adapter_wire.sha256_file(validator),
        )}
        loaded = adapter_wire.load_reviewed_contract("typescript_node", registry)
        self.assertEqual("typescript_node", loaded.manifest["adapter_id"])
        validator.write_text("changed\n", encoding="utf-8")
        with self.assertRaises(adapter_wire.WireError):
            adapter_wire.load_reviewed_contract("typescript_node", registry)

    def test_reviewed_contract_loader_resolves_portable_relative_validator(self):
        validator = self.root / "matrix_contract.py"
        validator.write_text(
            "ADAPTER_ID='protocol_actual_hub'\nCONTRACT_REVISION=1\n"
            "DECISION_CODES=frozenset({'accepted'})\n"
            "def admit_case(case, context): return ('passed','accepted')\n",
            encoding="utf-8",
        )
        validator.chmod(0o600)
        validator_sha256 = adapter_wire.sha256_file(validator)
        manifest = self._write("matrix-contract.json", {
            "schema_version": 1, "adapter_id": "protocol_actual_hub", "revision": 1,
            "required_cases": ["candidate_artifact_identity"], "actors": [],
            "cases": [], "raw_schemas": [], "phases": [],
            "validator": {"path": "matrix_contract.py", "sha256": validator_sha256},
        })
        registry = {"protocol_actual_hub": adapter_wire.ReviewedContract(
            manifest_path=manifest["path"], manifest_sha256=manifest["sha256"],
            validator_path=str(validator), validator_sha256=validator_sha256,
        )}

        loaded = adapter_wire.load_reviewed_contract("protocol_actual_hub", registry)

        self.assertEqual("matrix_contract.py", loaded.manifest["validator"]["path"])
        self.assertEqual("protocol_actual_hub", loaded.module.ADAPTER_ID)

    def test_reviewed_contract_loader_hashes_manifest_declared_raw_schema_bytes(self):
        validator = self.root / "matrix_contract.py"
        validator.write_text(
            "ADAPTER_ID='protocol_actual_hub'\nCONTRACT_REVISION=1\n"
            "DECISION_CODES=frozenset({'accepted'})\n"
            "def admit_case(case, context): return ('passed','accepted')\n",
            encoding="utf-8",
        )
        validator.chmod(0o600)
        schema = self._write("protocol-http-v1.schema.json", {"type": "object"})
        validator_sha256 = adapter_wire.sha256_file(validator)
        manifest = self._write("schema-contract.json", {
            "schema_version": 1, "adapter_id": "protocol_actual_hub", "revision": 1,
            "required_cases": ["candidate_artifact_identity"], "actors": [],
            "cases": [],
            "raw_schemas": [{
                "id": "protocol-http-v1",
                "schema": {"path": "protocol-http-v1.schema.json", "sha256": schema["sha256"]},
            }],
            "phases": [],
            "validator": {"path": "matrix_contract.py", "sha256": validator_sha256},
        })
        registry = {"protocol_actual_hub": adapter_wire.ReviewedContract(
            manifest_path=manifest["path"], manifest_sha256=manifest["sha256"],
            validator_path=str(validator), validator_sha256=validator_sha256,
        )}

        adapter_wire.load_reviewed_contract("protocol_actual_hub", registry)
        Path(schema["path"]).write_text('{"type":"array"}\n', encoding="utf-8")

        with self.assertRaisesRegex(adapter_wire.WireError, "raw schema"):
            adapter_wire.load_reviewed_contract("protocol_actual_hub", registry)

    def test_reviewed_contract_loader_rejects_raw_schema_aliases_and_foreign_targets(self):
        validator = self.root / "matrix_contract.py"
        validator.write_text(
            "ADAPTER_ID='protocol_actual_hub'\nCONTRACT_REVISION=1\n"
            "DECISION_CODES=frozenset({'accepted'})\n"
            "def admit_case(case, context): return ('passed','accepted')\n",
            encoding="utf-8",
        )
        validator.chmod(0o600)
        schema = self._write("protocol-http-v1.schema.json", {"type": "object"})
        foreign_root = Path(tempfile.mkdtemp()).resolve()
        self.addCleanup(lambda: __import__("shutil").rmtree(foreign_root))
        foreign = foreign_root / "protocol-http-v1.schema.json"
        foreign.write_bytes(Path(schema["path"]).read_bytes())
        foreign.chmod(0o600)
        alias = self.root / "schema-alias.json"
        alias.symlink_to(Path(schema["path"]))
        unsafe_paths = [
            "./protocol-http-v1.schema.json",
            "../protocol-http-v1.schema.json",
            "schema-alias.json",
            str(foreign),
            f"{self.root}/./protocol-http-v1.schema.json",
        ]
        validator_sha256 = adapter_wire.sha256_file(validator)
        for index, unsafe_path in enumerate(unsafe_paths):
            with self.subTest(path=unsafe_path):
                manifest = self._write(f"unsafe-schema-{index}.json", {
                    "schema_version": 1, "adapter_id": "protocol_actual_hub", "revision": 1,
                    "required_cases": ["candidate_artifact_identity"], "actors": [],
                    "cases": [],
                    "raw_schemas": [{
                        "id": "protocol-http-v1",
                        "schema": {"path": unsafe_path, "sha256": schema["sha256"]},
                    }],
                    "phases": [],
                    "validator": {"path": "matrix_contract.py", "sha256": validator_sha256},
                })
                registry = {"protocol_actual_hub": adapter_wire.ReviewedContract(
                    manifest_path=manifest["path"], manifest_sha256=manifest["sha256"],
                    validator_path=str(validator), validator_sha256=validator_sha256,
                )}
                with self.assertRaises(adapter_wire.WireError):
                    adapter_wire.load_reviewed_contract("protocol_actual_hub", registry)

    def test_reviewed_contract_loader_rejects_relative_aliases_and_foreign_targets(self):
        validator = self.root / "matrix_contract.py"
        validator.write_text(
            "ADAPTER_ID='protocol_actual_hub'\nCONTRACT_REVISION=1\n"
            "DECISION_CODES=frozenset({'accepted'})\n"
            "def admit_case(case, context): return ('passed','accepted')\n",
            encoding="utf-8",
        )
        validator.chmod(0o600)
        validator_sha256 = adapter_wire.sha256_file(validator)
        foreign = self.root / "foreign.py"
        foreign.write_bytes(validator.read_bytes())
        foreign.chmod(0o600)
        alias = self.root / "alias.py"
        alias.symlink_to(validator)
        unsafe_bindings = [
            {"path": "./matrix_contract.py", "sha256": validator_sha256},
            {"path": f"../{self.root.name}/matrix_contract.py", "sha256": validator_sha256},
            {"path": "alias.py", "sha256": validator_sha256},
            {"path": "foreign.py", "sha256": validator_sha256},
            {"path": str(foreign), "sha256": validator_sha256},
            {"path": f"{self.root.parent}/./{self.root.name}/matrix_contract.py", "sha256": validator_sha256},
            {"path": "matrix_contract.py", "sha256": validator_sha256, "extra": True},
            ["matrix_contract.py", validator_sha256],
        ]
        for index, validator_binding in enumerate(unsafe_bindings):
            with self.subTest(validator_binding=validator_binding):
                manifest = self._write(f"unsafe-{index}.json", {
                    "schema_version": 1, "adapter_id": "protocol_actual_hub", "revision": 1,
                    "required_cases": ["candidate_artifact_identity"], "actors": [],
                    "cases": [], "raw_schemas": [], "phases": [],
                    "validator": validator_binding,
                })
                registry = {"protocol_actual_hub": adapter_wire.ReviewedContract(
                    manifest_path=manifest["path"], manifest_sha256=manifest["sha256"],
                    validator_path=str(validator), validator_sha256=validator_sha256,
                )}
                with self.assertRaises(adapter_wire.WireError):
                    adapter_wire.load_reviewed_contract("protocol_actual_hub", registry)

    def test_reviewed_contract_loader_rejects_nonregular_validator(self):
        validator = self.root / "validator-directory"
        validator.mkdir(mode=0o700)
        manifest = self._write("nonregular.json", {
            "schema_version": 1, "adapter_id": "protocol_actual_hub", "revision": 1,
            "required_cases": ["candidate_artifact_identity"], "actors": [],
            "cases": [], "raw_schemas": [], "phases": [],
            "validator": {"path": str(validator), "sha256": DIGEST},
        })
        registry = {"protocol_actual_hub": adapter_wire.ReviewedContract(
            manifest_path=manifest["path"], manifest_sha256=manifest["sha256"],
            validator_path=str(validator), validator_sha256=DIGEST,
        )}

        with self.assertRaises(adapter_wire.WireError):
            adapter_wire.load_reviewed_contract("protocol_actual_hub", registry)

    def test_profile_manifest_is_hash_bound_sha256sums_with_eighteen_members(self):
        members = []
        lines = []
        for index in range(17):
            name = "profile.json" if index == 0 else f"member-{index:02d}.json"
            value = {"profile_id": "hub-http-v1@1.0.0"} if index == 0 else {"index": index}
            binding = self._write(name, value)
            lines.append(f"{binding['sha256']}  {name}\n")
            members.append({"id": name.replace(".json", ""), "root": binding, "local": binding})
        checksum_path = self.root / "SHA256SUMS"
        checksum_path.write_text("".join(lines), encoding="utf-8")
        checksum_path.chmod(0o600)
        checksum = {"path": str(checksum_path), "sha256": adapter_wire.sha256_file(checksum_path)}
        members.append({"id": "SHA256SUMS", "root": checksum, "local": checksum})
        manifest = {"id": "profile_manifest", "root": checksum, "local": checksum}
        adapter_wire.validate_profile_inputs(manifest, members, checksum["sha256"])
        checksum_path.write_text('{"members": []}\n', encoding="utf-8")
        changed = {"path": str(checksum_path), "sha256": adapter_wire.sha256_file(checksum_path)}
        with self.assertRaises(adapter_wire.WireError):
            adapter_wire.validate_profile_inputs(
                {"id": "profile_manifest", "root": changed, "local": changed},
                members[:-1] + [{"id": "SHA256SUMS", "root": changed, "local": changed}],
                changed["sha256"],
            )


if __name__ == "__main__":
    unittest.main()
