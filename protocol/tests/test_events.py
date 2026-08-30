from __future__ import annotations

import json
import unittest

from jsonschema import Draft202012Validator, FormatChecker

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
                data = json.loads(next(line[5:] for line in lines if line.startswith("data:")))
                self.assertEqual(data["event_id"], event_id.strip())
                self.assertEqual(data["event_type"], event_type.strip())
                self.assertTrue(example["wire"].endswith("\n\n"))
                event_validator.validate(data)


if __name__ == "__main__":
    unittest.main()
