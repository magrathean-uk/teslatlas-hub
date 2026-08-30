from __future__ import annotations

import copy
import unittest

from jsonschema import Draft202012Validator, FormatChecker

from conformance.runner import validate_metadata_semantics
from tests.support import load_json, load_schema_registry


REQUIRED_SCHEMA_IDS = {
    "urn:teslatlas:protocol:schema:common:1.2.0",
    "urn:teslatlas:protocol:schema:observation:1.2.0",
    "urn:teslatlas:protocol:schema:data-quality:1.2.0",
    "urn:teslatlas:protocol:schema:resources:1.2.0",
    "urn:teslatlas:protocol:schema:projection:1.2.0",
    "urn:teslatlas:protocol:schema:error:1.2.0",
    "urn:teslatlas:protocol:schema:command:1.2.0",
    "urn:teslatlas:protocol:schema:metadata:1.2.0",
    "urn:teslatlas:protocol:schema:event:1.2.0",
    "urn:teslatlas:protocol:schema:fixture:1.2.0",
    "urn:teslatlas:protocol:schema:sse-contract:1.2.0",
    "urn:teslatlas:protocol:schema:conformance:1.2.0",
}


class DomainSchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.schemas, cls.registry = load_schema_registry()
        cls.format_checker = FormatChecker()

    def validator(self, schema_id: str) -> Draft202012Validator:
        return Draft202012Validator(
            self.schemas[schema_id],
            registry=self.registry,
            format_checker=self.format_checker,
        )

    def test_all_domain_schemas_are_draft_2020_12_and_well_formed(self) -> None:
        self.assertEqual(REQUIRED_SCHEMA_IDS, REQUIRED_SCHEMA_IDS & self.schemas.keys())
        for schema_id in sorted(REQUIRED_SCHEMA_IDS):
            schema = self.schemas[schema_id]
            self.assertEqual(
                "https://json-schema.org/draft/2020-12/schema", schema["$schema"]
            )
            Draft202012Validator.check_schema(schema)

    def test_every_valid_example_validates_against_its_declared_schema(self) -> None:
        manifest = load_json("examples/manifest.json")
        self.assertGreaterEqual(len(manifest["valid"]), 15)
        for entry in manifest["valid"]:
            with self.subTest(path=entry["path"]):
                self.validator(entry["schema"]).validate(load_json(entry["path"]))

    def test_every_negative_example_is_rejected_for_the_expected_reason(self) -> None:
        manifest = load_json("examples/manifest.json")
        self.assertGreaterEqual(len(manifest["invalid"]), 8)
        for entry in manifest["invalid"]:
            with self.subTest(path=entry["path"]):
                errors = list(
                    self.validator(entry["schema"]).iter_errors(load_json(entry["path"]))
                )
                self.assertTrue(errors)
                rendered = "\n".join(error.message for error in errors)
                self.assertIn(entry["message_contains"], rendered)

    def test_observation_rejects_non_utc_time_and_missing_provenance(self) -> None:
        observation = load_json("examples/observation.json")
        validator = self.validator("urn:teslatlas:protocol:schema:observation:1.2.0")

        non_utc = copy.deepcopy(observation)
        non_utc["provider_timestamp"] = "2026-08-30T12:00:00.000+01:00"
        self.assertTrue(list(validator.iter_errors(non_utc)))

        without_hash = copy.deepcopy(observation)
        without_hash.pop("raw_payload_hash")
        self.assertTrue(list(validator.iter_errors(without_hash)))

    def test_command_job_is_asynchronous_and_uses_closed_states(self) -> None:
        job = load_json("examples/command-job.json")
        validator = self.validator("urn:teslatlas:protocol:schema:command:1.2.0")
        validator.validate(job)

        synchronous = copy.deepcopy(job)
        synchronous["state"] = "completed"
        self.assertTrue(list(validator.iter_errors(synchronous)))

    def test_metadata_requires_positive_revision_and_audit_identity(self) -> None:
        record = load_json("examples/metadata-record.json")
        validator = self.validator("urn:teslatlas:protocol:schema:metadata:1.2.0")
        validator.validate(record)

        stale = copy.deepcopy(record)
        stale["revision"] = 0
        self.assertTrue(list(validator.iter_errors(stale)))

        anonymous = copy.deepcopy(record)
        anonymous.pop("updated_by")
        self.assertTrue(list(validator.iter_errors(anonymous)))

    def test_metadata_tombstone_preserves_deletion_audit_and_hash_rules(self) -> None:
        validator = self.validator("urn:teslatlas:protocol:schema:metadata:1.2.0")
        tombstone = load_json("examples/metadata-tombstone.json")
        validator.validate(tombstone)

        self.assertNotIn("revision", tombstone)
        self.assertNotIn("deleted_at", tombstone)
        self.assertNotIn("deleted_by", tombstone)
        self.assertEqual("deleted", tombstone["audit"]["deletion"]["action"])

        deletion_in_history = copy.deepcopy(tombstone)
        deletion_in_history["audit"]["history"].append(
            copy.deepcopy(tombstone["audit"]["deletion"])
        )
        self.assertTrue(list(validator.iter_errors(deletion_in_history)))

        non_deletion_terminal = copy.deepcopy(tombstone)
        non_deletion_terminal["audit"]["deletion"]["action"] = "updated"
        self.assertTrue(list(validator.iter_errors(non_deletion_terminal)))

    def test_metadata_audit_chain_and_terminal_revision_are_semantic_contracts(self) -> None:
        record = load_json("examples/metadata-record.json")
        tombstone = load_json("examples/metadata-tombstone.json")
        self.assertEqual([], validate_metadata_semantics(record))
        self.assertEqual([], validate_metadata_semantics(tombstone))

        broken_chain = copy.deepcopy(tombstone)
        broken_chain["audit"]["history"][1]["previous_hash"] = "cd" * 32
        self.assertTrue(validate_metadata_semantics(broken_chain))

        broken_terminal = copy.deepcopy(tombstone)
        broken_terminal["audit"]["deletion"]["revision"] = 99
        self.assertTrue(validate_metadata_semantics(broken_terminal))


if __name__ == "__main__":
    unittest.main()
