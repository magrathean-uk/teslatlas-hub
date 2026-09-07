import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest

from . import receipt_validation


class ReceiptValidationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)
        self.artifact = self._write("artifact.bin", {"candidate": "2026.36.2"})
        self.cohort = self._write("cohort.json", {
            "schema_version": 2, "cohort_id": "B-2026.36.2",
            "builds": [{"outputs": [self.artifact]}],
        })
        self.context = {"cohort_id": "B-2026.36.2", "cohort_inputs": self.cohort}

    def tearDown(self):
        self.temporary.cleanup()

    def _write(self, name, value):
        path = self.root / name
        raw = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
        path.write_bytes(raw)
        path.chmod(0o600)
        return {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}

    def _bytes(self, name, raw):
        path = self.root / name
        path.write_bytes(raw); path.chmod(0o600)
        return {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}

    def _validate(self, kind, scope, value):
        binding = self._write(kind + ".json", value)
        receipt = {"kind": kind, "scope": scope, **binding}
        return receipt_validation.validate(receipt, Path(binding["path"]).read_bytes(), self.context)

    def _check(self, name, kind, facts=None, cohort=None):
        defaults = {
            "package-install": {"candidate_version": "2026.36.2", "package_bytes_verified": True, "install_layout_verified": True},
            "bootstrap-source": {"candidate_version": "2026.36.2", "source_snapshots_verified": True, "outputs_verified": True},
            "private-lane": {"authorization_verified": True, "disposition": "passed"},
            "historical-receipt": {"receipt_bindings_verified": True, "original_failure_preserved": True},
        }
        return self._write(name, {
            "schema_version": 1, "kind": kind, "status": "passed", "platform": "cohort",
            "cohort_inputs": cohort or self.cohort, "artifacts": [self.artifact],
            "facts": facts or defaults[kind],
        })

    def _gate(self, kind, checks, extra_inputs=()):
        log = self._write(kind + "-command.log", {"exit": 0})
        return {
            "id": "gate", "platform": "cohort", "status": "passed",
            "command": {"argv": ["/fixed/runner"], "cwd": str(self.root), "exit_code": 0, "outcome": "passed", "log": log},
            "inputs": [self.cohort, *extra_inputs], "outputs": [self.artifact],
            "checks": [{"kind": check_kind, "binding": binding} for check_kind, binding in checks],
        }

    def test_each_gate_kind_requires_its_semantic_payload(self):
        package = self._check("package-check.json", "package-install")
        bootstrap = self._check("bootstrap-check.json", "bootstrap-source")
        private = self._check("private-check.json", "private-lane")
        harness = self._write("compare-upgrade-stores.py", {"source": "fixture"})
        comparator = self._write("upgrade-comparison.json", {
            "status": "passed", "scope": "offline row-content preservation comparison only",
            "original_tables": 60, "added_empty_tables": receipt_validation.EDGE_TABLES,
            "before_schema": 57, "after_schema": 59, "paired_credentials_preserved": True,
            "allowed_change": "only nondecreasing paired-device last_authenticated_at_ms",
            "before_database_sha256": "a" * 64, "after_database_sha256": "b" * 64,
            "harness_sha256": harness["sha256"],
        })
        upgrade_checks = [
            ("upgrade-store-comparison", comparator),
            ("upgrade-package", self._check("upgrade-package.json", "upgrade-package", {"candidate_version": "2026.36.2"})),
            ("upgrade-lifecycle", self._check("upgrade-lifecycle.json", "upgrade-lifecycle", {"overinstall_completed": True})),
            ("upgrade-authentication", self._check("upgrade-auth.json", "upgrade-authentication", {"preserved_credentials_reauthenticated": True})),
        ]
        common = {
            "cohort_id": self.context["cohort_id"], "cohort_inputs": self.cohort,
            "status": "passed",
        }
        fixtures = {
            "package": ("local-built-not-published", {"schema_version": 1, "kind": "hub-package-receipt", **common, "gates": [self._gate("package", [("package-install", package)])], "artifacts": [self.artifact]}),
            "bootstrap": ("local-source-candidate", {"schema_version": 2, "kind": "hub-bootstrap-receipt", **common, "gates": [self._gate("bootstrap", [("bootstrap-source", bootstrap)])], "source_snapshots": [bootstrap], "outputs": [self.artifact]}),
            "upgrade": ("isolated-upgrade", {"schema_version": 1, "kind": "hub-upgrade-receipt", **common, "gates": [self._gate("upgrade", upgrade_checks, [harness])], "source_version": "2026.35.1", "target_version": "2026.36.2"}),
            "private-lane": ("authorized-private-lane", {"schema_version": 1, "kind": "hub-private-lane-receipt", **common, "gates": [self._gate("private", [("private-lane", private)])], "lane_receipts": [private]}),
        }
        for kind, (scope, value) in fixtures.items():
            with self.subTest(kind=kind):
                if kind == "upgrade":
                    value["gates"][0]["command"]["argv"].append(harness["path"])
                result = self._validate(kind, scope, value)
                self.assertEqual("passed", result["status"])
                self.assertEqual(["gate"], result["gate_ids"])

    def test_historical_uses_distinct_rehashed_inputs(self):
        historical_inputs = self._write("historical-inputs.json", {"schema_version": 2, "cohort_id": "A", "builds": []})
        evidence = self._write("historical-receipt.json", {"status": "passed"})
        check = self._check("historical-check.json", "historical-receipt", cohort=historical_inputs)
        log = self._write("historical-command.log", {"exit": 0})
        gate = self._gate("historical", [("historical-receipt", check)])
        gate["inputs"] = [historical_inputs]
        gate["outputs"] = []
        value = {
            "schema_version": 1, "kind": "historical-evidence", "cohort_id": self.context["cohort_id"],
            "historical_tested_inputs": historical_inputs, "status": "passed",
            "gates": [gate],
            "receipts": [evidence],
            "attempts": [{"id": "failed-original-build-legal", "argv": ["/fixed/old"], "exit_code": 1, "outcome": "failed", "log": log},
                         {"id": "accepted-A", "argv": ["/fixed/final"], "exit_code": 0, "outcome": "passed", "log": log}],
        }
        result = self._validate("historical", "historical-local-evidence", value)
        self.assertEqual(historical_inputs, result["historical_tested_inputs"])
        self.assertIsNone(result["tested_cohort_inputs"])

    def test_review_requires_bound_accepted_verdict_and_gate_lines(self):
        text = (
            "# Independent final review\n\nStatus: ACCEPTED\n\n"
            "C0/I0/M0\n"
            f"Cohort: {self.context['cohort_id']}\n"
            f"Cohort inputs: {self.cohort['path']} SHA256 {self.cohort['sha256']}\n"
            "Gate: independent-final-review\n"
        ).encode()
        path = self.root / "review.md"
        path.write_bytes(text); path.chmod(0o600)
        receipt = {"kind": "review", "scope": "independent-review", "path": str(path), "sha256": hashlib.sha256(text).hexdigest()}
        result = receipt_validation.validate(receipt, text, self.context)
        self.assertTrue(result["accepted_review_verdict"])
        with self.assertRaises(receipt_validation.ReceiptValidationError):
            receipt_validation.validate(receipt, text.replace(b"C0/I0/M0", b"C0/I1/M0"), self.context)

    def test_generic_success_dictionary_cannot_become_a_gate(self):
        with self.assertRaises(receipt_validation.ReceiptValidationError):
            self._validate("package", "local-built-not-published", {"status": "passed", "gate_ids": ["gate"]})

    def test_large_artifact_hashing_is_streamed_beyond_json_record_limit(self):
        path = self.root / "large-package.pkg"
        path.write_bytes(b"x" * (receipt_validation.MAX_RECORD + 1024))
        path.chmod(0o600)
        binding = {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
        self.assertEqual(binding, receipt_validation._binding(binding, "large package"))
        with self.assertRaises(receipt_validation.ReceiptValidationError):
            receipt_validation._document(binding, "structured record")

    def test_supplement_binds_journal_ready_ack_actor_and_command_semantics(self):
        session_id = "11111111-1111-4111-8111-111111111111"
        cell_id = "typescript_node__macos_arm64"
        proofs = [{"sequence": 1, "state": "running"}, {"sequence": 2, "state": "running"}]
        observations = self._write("observations.json", {"schema_version": 1, "session_id": session_id, "observations": proofs})
        rows = [{"kind": "opened", "session_id": session_id, "host_id": "host"}]
        rows += [{"kind": "result", "operation": "verify", "sequence": proof["sequence"], "state": "running", "store_id": "store", "proof_sha256": hashlib.sha256(json.dumps(proof, sort_keys=True, separators=(",", ":")).encode()).hexdigest()} for proof in proofs]
        rows.append({"errors": [], "kind": "closed"})
        journal = self._bytes("journal.jsonl", b"".join((json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n").encode() for row in rows))
        final_value = {"status": "stopped", "session_id": session_id, "service": {"state": "stopped", "cleanup_errors": [], "owned_generation": {"stop_evidence": {"operation_sequence": 3}}}, "listener": {"owner_pid": None}}
        final_stopped = self._write("final-stopped.json", final_value)
        transport = self._write("transport.json", {"schema_version": 1, "session_id": session_id, "status": "passed", "resources": []})
        normalized = self._write("normalized.json", {"cell_id": cell_id})
        raw_evidence = self._write("case-raw.json", {"request_ids": ["request-1"]})
        manifest = self._write("actor-manifest.json", {"schema_version": 1})
        runtime = self._write("actor-runtime.json", {"schema_version": 1})
        actor = {"id": "sdk_node", "kind": "packed_sdk_node", "runtime_ref": "root_node", "entrypoint_ref": "sdk_node_worker", "artifact_roles": ["typescript_sdk_tarball"], "source_roles": ["typescript_sdk_source"], "installed_manifest": manifest, "raw_evidence": [{"id": "raw", "schema_id": "node-http", "binding": raw_evidence}]}
        invocation = {"id": "invoke", "case_id": "case", "actor_id": "sdk_node", "operation": "verify", "session_sequence_before": 1, "session_sequence_after": 2, "evidence_id": "raw", "request_ids": ["request-1"]}
        actor_evidence = self._write("actors.json", {"schema_version": 1, "session_id": session_id, "cell_id": cell_id, "session_input_sha256": "a" * 64, "actors": [actor], "invocations": [invocation]})
        adapter_completion = self._write("adapter-completion.json", {"schema_version": 1, "session_id": session_id, "cell_id": cell_id, "session_input_sha256": "a" * 64, "normalized": normalized, "actor_evidence": actor_evidence})
        ready_value = {"schema_version": 1, "type": "ready", "session_id": session_id, "cell_id": cell_id, "session_input_sha256": "a" * 64, "instance_nonce": "b" * 64, "sequence": 1, "phase": "evidence_ready", "observation": {"session_sequence": 2, "proof_sha256": rows[2]["proof_sha256"]}, "evidence": adapter_completion}
        ready = self._write("ready.json", ready_value)
        close = self._write("close.json", {"schema_version": 1, "session_id": session_id, "state": "closed", "journal": journal, "final_stopped": final_value, "cleanup_errors": [], "local_transport": {"verified": True}})
        ack = self._write("ack.json", {"schema_version": 1, "type": "ack", "session_id": session_id, "cell_id": cell_id, "session_input_sha256": "a" * 64, "instance_nonce": "b" * 64, "sequence": 1, "ready_sha256": ready["sha256"], "phase": "evidence_ready", "status": "accepted", "action": "close_completed", "result": close})
        command = self._write("command.json", {"schema_version": 1, "session_id": session_id, "cell_id": cell_id, "exit_code": 0, "outcome": "passed", "logs": []})
        augmented = {**actor, "runtime_evidence": runtime}
        supplement = self._write("supplement.json", {"schema_version": 2, "cell_id": cell_id, "session_id": session_id, "actors": [augmented], "case_bindings": [{"case_id": "case", "actor_ids": ["sdk_node"], "invocations": [invocation]}], "controller_evidence": {"registration_sha256": "c" * 64, "session_config_sha256": "d" * 64, "observations": observations, "journal": journal, "final_stopped": final_stopped, "transport_cleanup": transport}, "completion": {"adapter_completion": adapter_completion, "ready": ready, "ack": ack, "normalized": normalized, "actor_evidence": actor_evidence, "command_outcome": command, "status": "passed"}})
        cell = {"cell_id": cell_id, "client_id": "typescript_node", "case_results": [{"id": "case"}]}
        receipt_validation._supplement(supplement, cell)
        altered = json.loads(Path(supplement["path"]).read_text())
        altered["completion"]["ack"] = ready
        rejected = self._write("supplement-altered.json", altered)
        with self.assertRaises(receipt_validation.ReceiptValidationError):
            receipt_validation._supplement(rejected, cell)


if __name__ == "__main__":
    unittest.main()
