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
        raw_facts = facts or defaults[kind]
        structured_facts = {}
        for fact_name, fact_value in raw_facts.items():
            observation = self._write(name + "-" + fact_name + ".observation.json", {
                "schema_version": 1, "kind": "producer-observation",
                "fact": kind + "." + fact_name, "status": "passed",
                "inputs": [cohort or self.cohort], "outputs": [self.artifact],
                "observed": fact_value,
            })
            structured_facts[fact_name] = {"value": fact_value, "observation": observation}
        return self._write(name, {
            "schema_version": 1, "kind": kind, "status": "passed", "platform": "cohort",
            "cohort_inputs": cohort or self.cohort, "artifacts": [self.artifact],
            "facts": structured_facts,
        })

    def _gate(self, kind, checks, extra_inputs=()):
        command = {"argv": ["/fixed/runner"], "cwd": str(self.root), "exit_code": 0, "outcome": "passed"}
        log = self._write(kind + "-command.log", {"schema_version": 1, **command})
        return {
            "id": "gate", "platform": "cohort", "status": "passed",
            "command": {**command, "log": log},
            "inputs": [self.cohort, *extra_inputs], "outputs": [self.artifact],
            "checks": [{"kind": check_kind, "binding": binding} for check_kind, binding in checks],
        }

    def test_each_gate_kind_requires_its_semantic_payload(self):
        package = self._check("package-check.json", "package-install")
        bootstrap = self._check("bootstrap-check.json", "bootstrap-source")
        private = self._check("private-check.json", "private-lane")
        harness = self._write("compare-upgrade-stores.py", {"source": "fixture"})
        before_database = self._bytes("before.sqlite", b"before-database")
        after_database = self._bytes("after.sqlite", b"after-database")
        comparator = self._write("upgrade-comparison.json", {
            "status": "passed", "scope": "offline row-content preservation comparison only",
            "original_tables": 60, "added_empty_tables": receipt_validation.EDGE_TABLES,
            "before_schema": 57, "after_schema": 59, "paired_credentials_preserved": True,
            "allowed_change": "only nondecreasing paired-device last_authenticated_at_ms",
            "before_database_sha256": before_database["sha256"], "after_database_sha256": after_database["sha256"],
            "before_database": before_database, "after_database": after_database,
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
            "upgrade": ("isolated-upgrade", {"schema_version": 1, "kind": "hub-upgrade-receipt", **common, "gates": [self._gate("upgrade", upgrade_checks, [harness, before_database, after_database])], "source_version": "2026.35.1", "target_version": "2026.36.2"}),
            "private-lane": ("authorized-private-lane", {"schema_version": 1, "kind": "hub-private-lane-receipt", **common, "gates": [self._gate("private", [("private-lane", private)])], "lane_receipts": [private]}),
        }
        for kind, (scope, value) in fixtures.items():
            with self.subTest(kind=kind):
                if kind == "upgrade":
                    value["gates"][0]["command"]["argv"].append(harness["path"])
                    command = value["gates"][0]["command"]
                    command["log"] = self._write("upgrade-command-bound.log", {
                        "schema_version": 1, "argv": command["argv"], "cwd": command["cwd"],
                        "exit_code": command["exit_code"], "outcome": command["outcome"],
                    })
                result = self._validate(kind, scope, value)
                self.assertEqual("passed", result["status"])
                self.assertEqual(["gate"], result["gate_ids"])

    def test_historical_uses_distinct_rehashed_inputs(self):
        historical_inputs = self._write("historical-inputs.json", {"schema_version": 2, "cohort_id": "A", "builds": []})
        evidence = self._write("historical-receipt.json", {"status": "passed"})
        check = self._check("historical-check.json", "historical-receipt", cohort=historical_inputs)
        failed_log = self._write("historical-failed.log", {"schema_version": 1, "argv": ["/fixed/old"], "cwd": str(self.root), "exit_code": 1, "outcome": "failed"})
        passed_log = self._write("historical-passed.log", {"schema_version": 1, "argv": ["/fixed/final"], "cwd": str(self.root), "exit_code": 0, "outcome": "passed"})
        gate = self._gate("historical", [("historical-receipt", check)])
        gate["inputs"] = [historical_inputs]
        gate["outputs"] = []
        value = {
            "schema_version": 1, "kind": "historical-evidence", "cohort_id": self.context["cohort_id"],
            "historical_tested_inputs": historical_inputs, "status": "passed",
            "gates": [gate],
            "receipts": [evidence],
            "attempts": [{"id": "failed-original-build-legal", "argv": ["/fixed/old"], "cwd": str(self.root), "exit_code": 1, "outcome": "failed", "log": failed_log, "prior_failed_id": None},
                         {"id": "accepted-A", "argv": ["/fixed/final"], "cwd": str(self.root), "exit_code": 0, "outcome": "passed", "log": passed_log, "prior_failed_id": "failed-original-build-legal"}],
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

    def test_boolean_gate_fact_is_not_producer_evidence(self):
        binding = self._write("boolean-package-check.json", {
            "schema_version": 1, "kind": "package-install", "status": "passed",
            "platform": "cohort", "cohort_inputs": self.cohort,
            "artifacts": [self.artifact],
            "facts": {
                "candidate_version": "2026.36.2",
                "package_bytes_verified": True,
                "install_layout_verified": True,
            },
        })
        with self.assertRaisesRegex(receipt_validation.ReceiptValidationError, "producer observation"):
            receipt_validation._check(
                "package-install", binding, "cohort", self.cohort,
                {(self.artifact["path"], self.artifact["sha256"])}, "passed",
            )

    def test_execution_log_reads_and_binds_command_record(self):
        command = ["typescript_node", "fixed-reviewed-entrypoint"]
        stdout = self._bytes("stdout.log", b"stdout\n")
        stderr = self._bytes("stderr.log", b"")
        record = self._write("command-record.json", {
            "argv": command, "cwd": str(self.root), "started_at": "2026-09-08T00:00:00Z",
            "ended_at": "2026-09-08T00:00:01Z", "exit_code": 0, "outcome": "exited_zero",
        })
        value = {
            "stdout": {**stdout, "bytes": 7, "truncated": False},
            "stderr": {**stderr, "bytes": 0, "truncated": False},
            "command_record": record, "duration_ms": 1,
        }
        receipt_validation._execution_log(value, "row execution", command, 0)
        altered = dict(value)
        altered["command_record"] = self._write("foreign-command-record.json", {
            "argv": ["foreign"], "cwd": str(self.root), "started_at": "2026-09-08T00:00:00Z",
            "ended_at": "2026-09-08T00:00:01Z", "exit_code": 0, "outcome": "exited_zero",
        })
        with self.assertRaisesRegex(receipt_validation.ReceiptValidationError, "not bound"):
            receipt_validation._execution_log(altered, "row execution", command, 0)

    def test_identity_role_subset_is_rejected(self):
        source = lambda role: {
            "role": role, "repo": "/" + role, "head": "a" * 40,
            "dirty_patch_sha256": "b" * 64, "untracked_source_manifest_sha256": "c" * 64,
        }
        artifact = lambda role: {
            "role": role, "name": role, "path": "/" + role,
            "embedded_version": "2026.36.2", "sha256": "d" * 64,
        }
        cohort = {
            "repository_observations": [
                {"source_identity": source(role)} for role in (
                    "hub_source", "protocol_source", "typescript_sdk_source")
            ],
            "builds": [{"outputs": [artifact("hub_executable"), artifact("typescript_sdk_tarball")]}],
        }
        identity = {
            "sources": [source("hub_source"), source("protocol_source")],
            "artifacts": [artifact("hub_executable"), artifact("typescript_sdk_tarball")],
        }
        with self.assertRaisesRegex(receipt_validation.ReceiptValidationError, "source identity set"):
            receipt_validation._identity_sets(identity, "row identity", "typescript_node", cohort)

    def test_upgrade_comparator_hashes_must_bind_retained_database_files(self):
        before = self._bytes("before.sqlite", b"before")
        after = self._bytes("after.sqlite", b"after")
        harness = self._write("compare-upgrade-stores.py", {"source": "fixture"})
        comparison = self._write("comparison.json", {
            "status": "passed", "scope": "offline row-content preservation comparison only",
            "original_tables": 60, "added_empty_tables": receipt_validation.EDGE_TABLES,
            "before_schema": 57, "after_schema": 59, "paired_credentials_preserved": True,
            "allowed_change": "only nondecreasing paired-device last_authenticated_at_ms",
            "before_database_sha256": "0" * 64, "after_database_sha256": after["sha256"],
            "before_database": before, "after_database": after,
            "harness_sha256": harness["sha256"],
        })
        gate = self._gate("upgrade", [("upgrade-store-comparison", comparison)], [harness, before, after])
        gate["command"]["argv"].append(harness["path"])
        gate["command"]["log"] = self._write("comparison-command.log", {
            "schema_version": 1, "argv": gate["command"]["argv"], "cwd": gate["command"]["cwd"],
            "exit_code": gate["command"]["exit_code"], "outcome": gate["command"]["outcome"],
        })
        value = {
            "schema_version": 1, "kind": "hub-upgrade-receipt", "cohort_id": self.context["cohort_id"],
            "cohort_inputs": self.cohort, "status": "passed", "gates": [gate],
            "source_version": "2026.35.1", "target_version": "2026.36.2",
        }
        with self.assertRaisesRegex(receipt_validation.ReceiptValidationError, "upgrade comparator semantics"):
            self._validate("upgrade", "isolated-upgrade", value)

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
        observations_rows = [{
            "schema_version": 1, "session_id": session_id, "sequence": proof["sequence"],
            "operation": "verify", "state": "running", "started_monotonic_ns": proof["sequence"],
            "finished_monotonic_ns": proof["sequence"] + 1, "observed_at_ms": 1000 + proof["sequence"],
            "result_sha256": "b" * 64,
            "proof_sha256": hashlib.sha256(json.dumps(proof, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
            "scenario_sha256": "c" * 64, "seed_sha256": "d" * 64, "store_id": "store",
            "store_schema_version": 59, "hub_id": "hub", "service_generation": "generation",
            "invitations": {"active": {"pairing_id": "pair-active", "expires_at_ms": 2000}, "expired": {"pairing_id": "pair-expired", "expires_at_ms": 900}},
            "transition": None,
        } for proof in proofs]
        observations = self._write("observations.json", {"schema_version": 1, "session_id": session_id, "observations": observations_rows})
        rows = [{"kind": "opened", "session_id": session_id, "host_id": "host"}]
        result_bindings = {proof["sequence"]: self._bytes("operation-{:06d}-result.json".format(proof["sequence"]), ("result-{}".format(proof["sequence"])).encode()) for proof in proofs}
        rows += [{
            "kind": "operation-record", "operation": "verify", "request": {"op": "verify"},
            "request_binding": {"sequence": proof["sequence"], "challenge": "a" * 64, "op": "verify"},
            "processed_binding": {"sequence": proof["sequence"], "challenge": "a" * 64, "op": "verify"},
            "status": "admitted", "failure": None, "state": "running",
            "started_monotonic_ns": proof["sequence"], "finished_monotonic_ns": proof["sequence"] + 1,
            "observed_at_ms": 1000 + proof["sequence"], "result_binding": {"name": Path(result_bindings[proof["sequence"]]["path"]).name, "sha256": result_bindings[proof["sequence"]]["sha256"]},
            "result_sha256": result_bindings[proof["sequence"]]["sha256"], "proof": proof, "invitation": None,
            "expired_invitation": None, "advance": None, "events_sha256": "c" * 64,
        } for proof in proofs]
        rows.append({"errors": [], "kind": "closed"})
        journal = self._bytes("journal.jsonl", b"".join((json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n").encode() for row in rows))
        final_value = {"schema_version": 1, "status": "stopped", "session_id": session_id, "host_id": "host", "service": {"state": "stopped", "generation": None, "forced_escalation": False, "normal_exit": True, "owned_generation": {"service": {"pid": 1}, "stop_evidence": {"operation": "stop", "operation_sequence": 3}}, "cleanup_errors": []}, "listener": {"host": "127.0.0.1", "port": 18480, "owner_pid": None}}
        final_stopped = self._write("final-stopped.json", final_value)
        transport = self._write("transport.json", {"schema_version": 1, "session_id": session_id, "status": "passed", "resources": [{"resource": "broker", "status": "closed"}]})
        normalized = self._write("normalized.json", {"cell_id": cell_id})
        raw_evidence = self._write("case-raw.json", {"schema_version": 1, "request_ids": ["request-1"]})
        manifest = self._write("actor-manifest.json", {"schema_version": 1, "artifact_sha256": "e" * 64, "files": [{"path": "package/index.mjs", "bytes": 1, "mode": 420, "sha256": "f" * 64}]})
        runtime = self._write("actor-runtime.json", {"schema_version": 1, "runtime_ref": "root_node", "runtime_kind": "node", "identity_sha256": "e" * 64})
        actor = {"id": "sdk_node", "kind": "packed_sdk_node", "runtime_ref": "root_node", "entrypoint_ref": "sdk_node_worker", "artifact_roles": ["typescript_sdk_tarball"], "source_roles": ["typescript_sdk_source"], "installed_manifest": manifest, "raw_evidence": [{"id": "raw", "schema_id": "node-http", "binding": raw_evidence}]}
        invocation = {"id": "invoke", "case_id": "case", "actor_id": "sdk_node", "operation": "verify", "session_sequence_before": 1, "session_sequence_after": 2, "evidence_id": "raw", "request_ids": ["request-1"]}
        actor_evidence = self._write("actors.json", {"schema_version": 1, "session_id": session_id, "cell_id": cell_id, "session_input_sha256": "a" * 64, "actors": [actor], "invocations": [invocation]})
        adapter_completion = self._write("adapter-completion.json", {"schema_version": 1, "session_id": session_id, "cell_id": cell_id, "session_input_sha256": "a" * 64, "normalized": normalized, "actor_evidence": actor_evidence})
        ready_value = {"schema_version": 1, "type": "ready", "session_id": session_id, "cell_id": cell_id, "session_input_sha256": "a" * 64, "instance_nonce": "b" * 64, "sequence": 1, "phase": "evidence_ready", "observation": {"session_sequence": 2, "proof_sha256": observations_rows[1]["proof_sha256"]}, "evidence": adapter_completion}
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
