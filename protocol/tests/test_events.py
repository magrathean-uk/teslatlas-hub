from __future__ import annotations

import copy
import json
import unittest

from jsonschema import Draft202012Validator, FormatChecker

from conformance.runner import validate_event_semantics
from tests.support import load_json, load_schema_registry


class EventStreamContractTests(unittest.TestCase):
    @property
    def contract(self) -> dict[str, object]:
        return load_json("events/teslatlas-v1.sse.json")

    def setUp(self) -> None:
        self.schemas, self.registry = load_schema_registry()

    def test_sse_contract_is_schema_valid(self) -> None:
        schema_id = "urn:teslatlas:protocol:schema:sse-contract:1.2.0"
        self.assertIn(schema_id, self.schemas)
        validator = Draft202012Validator(
            self.schemas[schema_id],
            registry=self.registry,
            format_checker=FormatChecker(),
        )
        validator.validate(self.contract)

    def test_sse_framing_replay_reset_and_terminal_status_are_explicit(self) -> None:
        self.assertEqual("/v1/events", self.contract["endpoint"])
        self.assertEqual("text/event-stream", self.contract["media_type"])
        self.assertEqual("utf-8", self.contract["encoding"])
        self.assertEqual("Last-Event-ID", self.contract["replay"]["request_header"])
        self.assertEqual("strictly-after", self.contract["replay"]["resume"])
        self.assertEqual(86400, self.contract["replay"]["minimum_retention_seconds"])
        self.assertEqual("event_replay_expired", self.contract["replay"]["expired_error_code"])
        self.assertEqual(410, self.contract["replay"]["expired_status"])
        self.assertEqual("empty-id-field", self.contract["replay"]["reset"])
        self.assertEqual(204, self.contract["reconnect"]["terminal_status"])
        self.assertEqual("blank-line", self.contract["framing"]["dispatch_boundary"])

    def test_each_sse_example_has_matching_id_type_and_valid_json_data(self) -> None:
        event_validator = Draft202012Validator(
            self.schemas["urn:teslatlas:protocol:schema:event:1.2.0"],
            registry=self.registry,
            format_checker=FormatChecker(),
        )
        for example in self.contract["examples"]:
            with self.subTest(name=example["name"]):
                lines = example["wire"].splitlines()
                event_id = next(line[3:] for line in lines if line.startswith("id:"))
                event_type = next(line[6:] for line in lines if line.startswith("event:"))
                data_lines = [line[5:] for line in lines if line.startswith("data:")]
                self.assertGreaterEqual(len(data_lines), 2)
                data = json.loads("\n".join(data_lines))
                self.assertTrue(example["wire"].endswith("\n\n"))
                self.assertEqual(data["event_id"], event_id.strip())
                self.assertEqual(data["event_type"], event_type.strip())
                event_validator.validate(data)
                self.assertEqual([], validate_event_semantics(data))

    def test_v1_event_shape_is_stable_and_semantic_equality_is_executable(self) -> None:
        event = load_json("examples/event-envelope.json")
        validator = Draft202012Validator(
            self.schemas["urn:teslatlas:protocol:schema:event:1.2.0"],
            registry=self.registry,
            format_checker=FormatChecker(),
        )
        validator.validate(event)
        self.assertEqual([], validate_event_semantics(event))
        self.assertEqual(
            {
                "event_id",
                "event_type",
                "occurred_at",
                "vehicle_id",
                "resource_id",
                "revision",
                "data",
            },
            set(event),
        )

        mismatched_type = copy.deepcopy(event)
        mismatched_type["event_type"] = "command.changed"
        self.assertTrue(list(validator.iter_errors(mismatched_type)))

        for field, value in (
            ("vehicle_id", "vehicle_demo_other"),
            ("resource_id", "vehicle_demo_other"),
            ("revision", 999),
        ):
            mismatched = copy.deepcopy(event)
            mismatched[field] = value
            self.assertTrue(validate_event_semantics(mismatched), field)

    def test_event_catalogue_and_typed_schema_branches_cover_the_same_names(self) -> None:
        event_schema = self.schemas["urn:teslatlas:protocol:schema:event:1.2.0"]
        declared = set(event_schema["properties"]["event_type"]["enum"])
        catalogued = {item["name"] for item in self.contract["events"]}
        branched: set[str] = set()
        for branch in event_schema["allOf"]:
            selector = branch["if"]["properties"]["event_type"]
            branched.update(selector.get("enum", [selector.get("const")]))
        self.assertEqual(declared, catalogued)
        self.assertEqual(declared, branched)


if __name__ == "__main__":
    unittest.main()
