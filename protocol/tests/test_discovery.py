from __future__ import annotations

import copy
import unittest

from jsonschema import Draft202012Validator, FormatChecker

from conformance.runner import validate_discovery_semantics
from tests.support import load_json


class DiscoveryContractTests(unittest.TestCase):
    def test_discovery_requires_identity_versions_capabilities_and_limits(self) -> None:
        schema = load_json("schemas/discovery.schema.json")
        example = load_json("examples/discovery.json")

        Draft202012Validator.check_schema(schema)
        validator = Draft202012Validator(schema, format_checker=FormatChecker())
        validator.validate(example)

        for required in ("hub_id", "protocol", "capabilities", "endpoints", "limits"):
            broken = dict(example)
            broken.pop(required)
            errors = list(validator.iter_errors(broken))
            self.assertTrue(errors, f"missing {required} must be rejected")

    def test_async_command_capability_publishes_supported_commands_and_scopes(self) -> None:
        schema = load_json("schemas/discovery.schema.json")
        example = load_json("examples/discovery.json")
        validator = Draft202012Validator(schema, format_checker=FormatChecker())
        validator.validate(example)

        capability = next(
            item for item in example["capabilities"] if item["id"] == "commands.async"
        )
        self.assertIn("commands", capability)
        commands = capability["commands"]
        self.assertGreaterEqual(len(commands), 2)
        self.assertEqual(len(commands), len({item["name"] for item in commands}))
        for command in commands:
            self.assertIn("command_class", command)
            self.assertIn("required_scope", command)
            self.assertIn("retry_policy", command)
            self.assertIn("confirmation_required", command)
            Draft202012Validator.check_schema(command["parameters_schema"])
            Draft202012Validator.check_schema(command["expected_state_schema"])

    def test_discovery_cross_field_version_and_identity_rules_are_executable(self) -> None:
        example = load_json("examples/discovery.json")
        self.assertEqual([], validate_discovery_semantics(example))

        mutations = []
        wrong_current = {**example, "protocol": dict(example["protocol"])}
        wrong_current["protocol"]["current_version"] = "1.1.0"
        mutations.append(wrong_current)

        wrong_minimum = {**example, "protocol": dict(example["protocol"])}
        wrong_minimum["protocol"]["minimum_client_version"] = "1.1.0"
        mutations.append(wrong_minimum)

        wrong_order = {**example, "protocol": dict(example["protocol"])}
        wrong_order["protocol"]["supported_versions"] = ["1.1.0", "1.0.0", "1.2.0"]
        mutations.append(wrong_order)

        duplicate_capability = {**example, "capabilities": list(example["capabilities"])}
        duplicate_capability["capabilities"].append(dict(example["capabilities"][0]))
        mutations.append(duplicate_capability)

        for document in mutations:
            with self.subTest(protocol=document["protocol"]):
                self.assertTrue(validate_discovery_semantics(document))

    def test_malformed_discovery_semantics_return_errors_without_crashing(self) -> None:
        example = load_json("examples/discovery.json")

        mutations = []
        empty_versions = copy.deepcopy(example)
        empty_versions["protocol"]["supported_versions"] = []
        mutations.append(empty_versions)

        invalid_version_type = copy.deepcopy(example)
        invalid_version_type["protocol"]["supported_versions"][0] = []
        mutations.append(invalid_version_type)

        invalid_capability_id = copy.deepcopy(example)
        invalid_capability_id["capabilities"][0]["id"] = []
        mutations.append(invalid_capability_id)

        invalid_command_name = copy.deepcopy(example)
        command_capability = next(
            item for item in invalid_command_name["capabilities"] if "commands" in item
        )
        command_capability["commands"][0]["name"] = []
        mutations.append(invalid_command_name)

        for document in mutations:
            with self.subTest(document=document):
                errors = validate_discovery_semantics(document)
                self.assertTrue(errors)

    def test_deprecation_can_be_unscheduled_but_scheduled_sunset_follows_deprecation(self) -> None:
        schema = load_json("schemas/discovery.schema.json")
        example = load_json("examples/discovery.json")
        validator = Draft202012Validator(schema, format_checker=FormatChecker())
        capability = dict(example["capabilities"][0])
        capability["status"] = "deprecated"
        capability["deprecation"] = {
            "deprecated_at": "2030-01-01T00:00:00.000Z",
            "sunset_at": None,
            "successor": "query.vehicles.next",
            "documentation": "https://protocol.example.invalid/deprecations/vehicles",
        }
        unscheduled = {**example, "capabilities": [capability, *example["capabilities"][1:]]}
        validator.validate(unscheduled)
        self.assertEqual([], validate_discovery_semantics(unscheduled))

        capability["deprecation"]["sunset_at"] = "2029-12-31T23:59:59.000Z"
        self.assertTrue(validate_discovery_semantics(unscheduled))


if __name__ == "__main__":
    unittest.main()
