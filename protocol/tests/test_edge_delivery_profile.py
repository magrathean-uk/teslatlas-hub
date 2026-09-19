"""Independent Edge delivery v2 contract tests; no Edge or Hub imports."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

from jsonschema import Draft202012Validator


ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/edge-delivery-v2/2.0.0"
PROFILE_ID = "edge-delivery-v2@2.0.0"
BASE_ID = "8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9"
BASE_ALIAS = "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd"
REPLAY_ALIAS = "5fe1588aca9e75808ce9d1292f7b683e489d9724d5772fae5dfd941714fe0bd1"
CHANGED_ID = "8960ff655b442db799b8e61c949e7b97d0d07454b75f5c7a580a8686a893be14"
ALERT_ID = "42424c8baa6532299915b8026625a0dc271769fd97e19647b6366df523866d1f"
NOTICE_ID = "73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2"
BATCH_ID = "01d91f49aa9e06ae80070976797616a9ec74cf6ef22c6862f615b5a28371a0ef"


def compact(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def jcs_fixture(value):
    # These fixtures use integers, strings, objects and arrays only. For that
    # deliberately narrow domain, sorted compact UTF-8 JSON is RFC 8785 JCS.
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode()


def digest(domain, body):
    return hashlib.sha256(domain + body).hexdigest()


class BundlePresenceTests(unittest.TestCase):
    def test_edge_delivery_v2_profile_bundle_exists(self):
        self.assertTrue(PROFILE.is_dir(), "Edge delivery v2 profile bundle must exist")


@unittest.skipUnless(PROFILE.is_dir(), "profile bundle is not implemented yet")
class EdgeDeliveryProfileTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile = json.loads((PROFILE / "profile.json").read_text())
        cls.vectors = json.loads((PROFILE / "vectors.json").read_text())
        cls.envelope_schema = json.loads((PROFILE / "receiver-envelope.schema.json").read_text())
        cls.delivery_schema = json.loads((PROFILE / "delivery.schema.json").read_text())
        cls.disposition_schema = json.loads(
            (PROFILE / "consumer-disposition.schema.json").read_text()
        )

    def test_bundle_is_deterministic_and_does_not_mutate_current_hub_profile(self):
        result = subprocess.run(
            [sys.executable, str(ROOT / "tools/build_edge_delivery_profile.py"), "--check"],
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(self.profile["profile_id"], PROFILE_ID)
        current_hub_manifest = ROOT / "profiles/hub-http-v1/1.0.0/SHA256SUMS"
        self.assertEqual(
            hashlib.sha256(current_hub_manifest.read_bytes()).hexdigest(),
            "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926",
        )

    def test_literal_stable_and_legacy_ids_match_independent_hashes(self):
        identity = self.vectors["identity"]
        base = identity["base_envelope"]
        replay = identity["changed_arrival_envelope"]
        changed = identity["changed_payload_envelope"]
        self.assertEqual(identity["stable_record_id"], BASE_ID)
        self.assertEqual(identity["legacy_record_id"], BASE_ALIAS)
        self.assertEqual(identity["changed_arrival_legacy_record_id"], REPLAY_ALIAS)
        self.assertEqual(identity["changed_payload_stable_record_id"], CHANGED_ID)

        def stable_id(envelope):
            stable = copy.deepcopy(envelope)
            del stable["received_at_ms"]
            stable["version"] = 2
            return digest(b"teslatlas-edge-record-v2\0", jcs_fixture(stable))

        def legacy_id(envelope):
            return digest(b"teslatlas-edge-record-v1\0", jcs_fixture(envelope))

        self.assertEqual(stable_id(base), BASE_ID)
        self.assertEqual(stable_id(replay), BASE_ID)
        self.assertEqual(stable_id(changed), CHANGED_ID)
        self.assertEqual(legacy_id(base), BASE_ALIAS)
        self.assertEqual(legacy_id(replay), REPLAY_ALIAS)
        self.assertNotEqual(legacy_id(base), legacy_id(replay))

    def test_reenqueue_after_completed_ack_is_a_new_sequence_duplicate_disposition(self):
        replay = self.vectors.get("re_enqueued_after_completed_ack")
        self.assertIsNotNone(replay)
        self.assertEqual(replay["spool_seq"], 13)
        self.assertEqual(replay["record_id"], BASE_ID)
        self.assertEqual(replay["legacy_record_id"], REPLAY_ALIAS)
        self.assertEqual(replay["expected_disposition"], "duplicate")
        self.assertEqual(replay["application_count_after_commit"], 1)
        self.assertTrue(replay["ack_eligible_after_atomic_commit"])

    def test_literal_merged_order_batch_id_and_ack_bytes(self):
        sequence = self.vectors["sequence"]
        self.assertEqual(
            sequence["merged_order"],
            [
                {"spool_seq": 10, "kind": "record", "id": BASE_ID},
                {"spool_seq": 11, "kind": "gap", "id": NOTICE_ID},
                {"spool_seq": 12, "kind": "record", "id": ALERT_ID},
            ],
        )
        material = bytearray(b"teslatlas-edge-batch-v2\0")
        for item in sequence["merged_order"]:
            material.extend(b"r" if item["kind"] == "record" else b"g")
            material.extend(item["spool_seq"].to_bytes(8, "big"))
            material.extend(item["id"].encode())
            material.append(0)
        self.assertEqual(hashlib.sha256(material).hexdigest(), BATCH_ID)
        self.assertEqual(sequence["batch"]["batch_id"], BATCH_ID)

        prefix = sequence["ack_prefixes"][2]
        self.assertEqual(prefix["through_spool_seq"], 11)
        self.assertEqual(
            prefix["request_body_utf8"],
            '{"version":2,"batch_id":"01d91f49aa9e06ae80070976797616a9ec74cf6ef22c6862f615b5a28371a0ef","accepted_record_ids":["8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9"],"accepted_gap_notice_ids":["73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2"]}',
        )
        self.assertEqual(json.loads(prefix["request_body_utf8"]), prefix["request"])
        self.assertEqual(
            prefix["response_body_utf8"],
            '{"version":2,"acknowledged_record_ids":["8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9"],"acknowledged_gap_notice_ids":["73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2"],"unknown_record_ids":[],"unknown_gap_notice_ids":[]}',
        )
        self.assertEqual(json.loads(prefix["response_body_utf8"]), prefix["response"])

    def test_gap_digest_is_literal_and_independently_recomputed(self):
        gap = self.vectors["gap"]
        evidence_material = (
            b"teslatlas-edge-gap-evidence-v2\0"
            + (11).to_bytes(8, "big")
            + gap["lost_stable_record_id"].encode()
            + b"retention_expired"
        )
        self.assertEqual(
            hashlib.sha256(evidence_material).hexdigest(),
            "c2fceaa38e73c41b78386cad195a383e5ae4505db9f71c0e43e7f669a8380e53",
        )
        notice_material = (
            b"teslatlas-edge-gap-notice-v2\0"
            + (11).to_bytes(8, "big")
            + b"retention_expired"
            + gap["notice"]["evidence_sha256"].encode()
        )
        self.assertEqual(hashlib.sha256(notice_material).hexdigest(), NOTICE_ID)
        self.assertEqual(gap["notice"]["notice_id"], NOTICE_ID)

    def test_examples_validate_and_schemas_reject_unknown_or_malformed_identity(self):
        validators = {
            "receiver-envelope": Draft202012Validator(self.envelope_schema),
            "batch-v2": Draft202012Validator(
                self.delivery_schema["$defs"]["batch_v2"]
            ),
            "ack-v2": Draft202012Validator(self.delivery_schema["$defs"]["ack_v2"]),
            "ack-result-v2": Draft202012Validator(
                self.delivery_schema["$defs"]["ack_result_v2"]
            ),
            "consumer-disposition": Draft202012Validator(self.disposition_schema),
            "consumer-gap-disposition": Draft202012Validator(self.disposition_schema),
        }
        for example in self.profile["examples"]:
            value = json.loads((PROFILE / example["path"]).read_text())
            self.assertEqual(
                list(validators[example["kind"]].iter_errors(value)), [], example["path"]
            )
        invalid = copy.deepcopy(self.vectors["identity"]["base_envelope"])
        invalid["vin"] = "invalid"
        self.assertTrue(list(validators["receiver-envelope"].iter_errors(invalid)))
        invalid = copy.deepcopy(self.vectors["sequence"]["batch"])
        invalid["records"][0]["record_id"] = "A" * 64
        self.assertTrue(list(validators["batch-v2"].iter_errors(invalid)))
        invalid = copy.deepcopy(self.vectors["sequence"]["batch"])
        invalid["unexpected"] = True
        self.assertTrue(list(validators["batch-v2"].iter_errors(invalid)))

    def test_gap_disposition_requires_complete_evidence_and_matching_category(self):
        validator = Draft202012Validator(self.disposition_schema)
        gap_examples = [
            item
            for item in self.profile["examples"]
            if item["kind"] == "consumer-gap-disposition"
        ]
        self.assertEqual(len(gap_examples), 1)
        gap_example = gap_examples[0]
        complete = json.loads((PROFILE / gap_example["path"]).read_text())
        self.assertEqual(list(validator.iter_errors(complete)), [])
        self.assertEqual(complete["item_kind"], "gap")
        self.assertEqual(complete["category"], "durable_gap")
        self.assertEqual(complete["evidence_sha256"], self.vectors["gap"]["notice"]["evidence_sha256"])
        self.assertEqual(complete["occurred_at_ms"], self.vectors["gap"]["notice"]["occurred_at_ms"])

        for missing in ("evidence_sha256", "occurred_at_ms"):
            invalid = copy.deepcopy(complete)
            del invalid[missing]
            self.assertTrue(list(validator.iter_errors(invalid)), missing)

        invalid = copy.deepcopy(complete)
        invalid["category"] = "durable_non_projection_event"
        self.assertTrue(list(validator.iter_errors(invalid)))

        record = json.loads(
            (PROFILE / "examples/non-projection-disposition.json").read_text()
        )
        record["category"] = "durable_gap"
        self.assertTrue(list(validator.iter_errors(record)))
        record = json.loads(
            (PROFILE / "examples/non-projection-disposition.json").read_text()
        )
        record["evidence_sha256"] = self.vectors["gap"]["notice"]["evidence_sha256"]
        record["occurred_at_ms"] = self.vectors["gap"]["notice"]["occurred_at_ms"]
        self.assertTrue(list(validator.iter_errors(record)))

    def test_profile_hash_parameters_are_exact_and_drive_literal_witnesses(self):
        parameters = self.profile.get("hash_parameters")
        self.assertIsNotNone(parameters)
        domains = parameters["domains"]
        expected_prefixes = {
            "legacy_record_id": b"teslatlas-edge-record-v1",
            "stable_record_id": b"teslatlas-edge-record-v2",
            "gap_evidence": b"teslatlas-edge-gap-evidence-v2",
            "gap_notice": b"teslatlas-edge-gap-notice-v2",
            "batch": b"teslatlas-edge-batch-v2",
        }
        decoded = {}
        for name, prefix in expected_prefixes.items():
            definition = domains[name]
            value = bytes.fromhex(definition["bytes_hex"])
            self.assertEqual(definition["prefix_utf8"].encode(), prefix)
            self.assertEqual(definition["terminal_hex"], "00")
            self.assertEqual(value, prefix + b"\0")
            self.assertFalse(value[:-1].endswith(b"\0"))
            decoded[name] = value

        self.assertEqual(
            parameters["batch_kind_bytes"],
            {
                "record": {"ascii": "r", "hex": "72"},
                "gap": {"ascii": "g", "hex": "67"},
            },
        )
        self.assertEqual(
            parameters["spool_seq_u64"],
            {"byte_order": "big", "width_bytes": 8},
        )
        self.assertEqual(parameters["batch_item_terminator_hex"], "00")
        self.assertEqual(
            parameters["hex_encoding"],
            "bytes_hex and every field ending in _hex represent bytes decoded from lowercase hexadecimal; the hexadecimal text itself is never hashed.",
        )

        base = self.vectors["identity"]["base_envelope"]
        stable = copy.deepcopy(base)
        del stable["received_at_ms"]
        stable["version"] = 2
        self.assertEqual(digest(decoded["legacy_record_id"], jcs_fixture(base)), BASE_ALIAS)
        self.assertEqual(digest(decoded["stable_record_id"], jcs_fixture(stable)), BASE_ID)

        gap = self.vectors["gap"]
        seq = gap["notice"]["spool_seq"].to_bytes(
            parameters["spool_seq_u64"]["width_bytes"],
            parameters["spool_seq_u64"]["byte_order"],
        )
        evidence = hashlib.sha256(
            decoded["gap_evidence"]
            + seq
            + gap["lost_stable_record_id"].encode()
            + gap["notice"]["reason"].encode()
        ).hexdigest()
        self.assertEqual(evidence, gap["notice"]["evidence_sha256"])
        notice = hashlib.sha256(
            decoded["gap_notice"]
            + seq
            + gap["notice"]["reason"].encode()
            + evidence.encode()
        ).hexdigest()
        self.assertEqual(notice, NOTICE_ID)

        material = bytearray(decoded["batch"])
        for item in self.vectors["sequence"]["merged_order"]:
            material.extend(bytes.fromhex(parameters["batch_kind_bytes"][item["kind"]]["hex"]))
            material.extend(
                item["spool_seq"].to_bytes(
                    parameters["spool_seq_u64"]["width_bytes"],
                    parameters["spool_seq_u64"]["byte_order"],
                )
            )
            material.extend(item["id"].encode())
            material.extend(bytes.fromhex(parameters["batch_item_terminator_hex"]))
        self.assertEqual(hashlib.sha256(material).hexdigest(), BATCH_ID)

    def test_disposition_vectors_require_durable_evidence_before_ack(self):
        by_category = {
            item["category"]: item for item in self.vectors["dispositions"]
        }
        self.assertEqual(
            set(by_category),
            {
                "projected_telemetry",
                "durable_non_projection_event",
                "durable_gap",
                "duplicate",
                "rejected_invalid_envelope",
            },
        )
        for category in (
            "projected_telemetry",
            "durable_non_projection_event",
            "durable_gap",
            "duplicate",
        ):
            case = by_category[category]
            self.assertTrue(case["ack_eligible_after_commit"])
            self.assertEqual(case["receipt_transaction"], "same_as_hub_data_effect")
        rejected = by_category["rejected_invalid_envelope"]
        self.assertFalse(rejected["ack_eligible_after_commit"])
        self.assertEqual(rejected["ack_prefix_effect"], "no_advance")
        non_projection = by_category["durable_non_projection_event"]
        self.assertEqual(non_projection["record_id"], ALERT_ID)
        self.assertIn("reason", non_projection["required_durable_fields"])
        self.assertIn("payload_sha256", non_projection["required_durable_fields"])
        gap = by_category["durable_gap"]
        self.assertIn("occurred_at_ms", gap["required_durable_fields"])
        self.assertIn("evidence_sha256", gap["required_durable_fields"])

    def test_alert_error_and_connectivity_vectors_have_literal_identity_and_payload_evidence(self):
        expected = {
            "alerts": (
                ALERT_ID,
                "e9e9bee52f0bda000acdd63a1d79c245e72bdfeca9c3b9c4932c6f5ec0e8e618",
                "2b8aa76b27b1b29810087e1692c1efcfe3b04e340be83700464570f0924e8fd7",
            ),
            "errors": (
                "6f1d19ab8fc5a1b9ec2b6d4b0323a2fd16948b8d3389fdb5fd0a681bd37e06f7",
                "b13b7660c9d9e9256298660a696961431af720fc16a857a45fe80d51391a6fd3",
                "215c31ed5e908b2375188f04c103c1f304364386e843a179006487c9cad0c250",
            ),
            "connectivity": (
                "42b51e71fd1afb779369fbaf3f63cc1ee2741e44bbe5ad412717303a68fe24c2",
                "ecf44df6ac19d8194669379fd1d6dadb7805a951730d4ee01954e0dcaf8a2611",
                "d94c7553aab24fd59e0342acb897099ebf83dc9f84a28791a05592f3d0772d07",
            ),
        }
        by_kind = {item["kind"]: item for item in self.vectors["non_projection_envelopes"]}
        self.assertEqual(set(by_kind), set(expected))
        for kind, (stable, legacy, payload_hash) in expected.items():
            case = by_kind[kind]
            self.assertEqual(case["record_id"], stable)
            self.assertEqual(case.get("legacy_record_id"), legacy)
            self.assertEqual(case["payload_sha256"], payload_hash)
            self.assertEqual(
                hashlib.sha256(jcs_fixture(case["envelope"]["payload"])).hexdigest(),
                payload_hash,
            )
            self.assertEqual(
                case.get("expected_disposition"), "durable_non_projection_event"
            )
            self.assertRegex(case.get("reason", ""), r"^[a-z0-9_]+$")
            self.assertTrue(case.get("ack_eligible_after_atomic_commit"))

    def test_negative_vectors_cover_fail_closed_boundaries(self):
        cases = {case["id"]: case for case in self.vectors["negative_cases"]}
        self.assertEqual(
            set(cases),
            {
                "missing-client-certificate",
                "missing-or-invalid-bearer",
                "stale-batch-correlation",
                "duplicate-json-key",
                "duplicate-accepted-id",
                "duplicate-record-id",
                "record-alias-conflict",
                "non-contiguous-prefix",
                "oversized-batch-items",
                "oversized-batch-bytes",
                "oversized-ack-body",
                "lost-ack-response",
            },
        )
        for case_id, case in cases.items():
            if case_id == "lost-ack-response":
                self.assertEqual(case["retry_result"], "same_ack_result_bytes")
                self.assertEqual(case["apply_count"], 1)
            else:
                self.assertEqual(case["ack_prefix_effect"], "no_advance", case_id)
        self.assertEqual(cases["oversized-batch-items"]["item_count"], 1025)
        self.assertEqual(
            cases["oversized-batch-bytes"]["encoded_item_bytes_sum"],
            4 * 1024 * 1024 + 1,
        )
        self.assertEqual(cases["oversized-ack-body"]["body_bytes"], 128 * 1024 + 1)

    def test_negative_wire_files_are_exact_and_executable(self):
        cases = {case["id"]: case for case in self.vectors["negative_cases"]}
        for case_id in (
            "duplicate-json-key",
            "oversized-batch-items",
            "oversized-ack-body",
            "stale-batch-correlation",
            "non-contiguous-prefix",
        ):
            self.assertIn("wire_path", cases[case_id], case_id)
        duplicate = (PROFILE / cases["duplicate-json-key"]["wire_path"]).read_bytes()
        self.assertEqual(duplicate.decode(), cases["duplicate-json-key"]["request_body_utf8"])
        self.assertEqual(duplicate.count(b'"batch_id"'), 2)

        oversized_batch_path = PROFILE / cases["oversized-batch-items"]["wire_path"]
        oversized_batch = json.loads(oversized_batch_path.read_bytes())
        self.assertEqual(len(oversized_batch["records"]) + len(oversized_batch["gaps"]), 1025)
        self.assertLess(oversized_batch_path.stat().st_size, 4 * 1024 * 1024)
        validator = Draft202012Validator(self.delivery_schema["$defs"]["batch_v2"])
        self.assertTrue(list(validator.iter_errors(oversized_batch)))

        oversized_ack_path = PROFILE / cases["oversized-ack-body"]["wire_path"]
        self.assertEqual(oversized_ack_path.stat().st_size, 128 * 1024 + 1)
        self.assertEqual(
            json.loads(oversized_ack_path.read_bytes()),
            self.vectors["sequence"]["ack_prefixes"][2]["request"],
        )

        for case_id in ("stale-batch-correlation", "non-contiguous-prefix"):
            raw = (PROFILE / cases[case_id]["wire_path"]).read_bytes()
            self.assertEqual(json.loads(raw), cases[case_id]["request"])

    def test_profile_keeps_v1_and_spool2_and_forbids_implicit_downgrade(self):
        self.assertEqual(self.profile["spool_format"], 2)
        self.assertEqual(self.profile["retained_wire_versions"], [1, 2])
        self.assertEqual(self.profile["selection"]["required_for_hub_consumer"], 2)
        self.assertFalse(self.profile["selection"]["automatic_v1_downgrade"])
        self.assertEqual(self.profile["limits"]["batch_max_items_hard"], 1024)
        self.assertEqual(self.profile["limits"]["batch_max_bytes_hard"], 4 * 1024 * 1024)
        self.assertEqual(
            self.profile["limits"].get("batch_max_bytes_measurement"),
            "sum_of_compact_json_encoded_records_and_gaps",
        )
        self.assertEqual(
            self.profile["limits"].get("batch_response_body_bytes_hard"),
            4 * 1024 * 1024 + 1137,
        )
        self.assertEqual(self.profile["limits"]["ack_body_bytes"], 128 * 1024)
        self.assertEqual(
            self.profile.get("timestamp_semantics"),
            {
                "batch_record_received_at_ms": "edge_durable_admission_time",
                "envelope_received_at_ms": "receiver_supplied_arrival_time_in_v1_alias_only",
                "envelope_timestamp_ms": "telemetry_event_time",
            },
        )


if __name__ == "__main__":
    unittest.main()
