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
