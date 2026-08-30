from __future__ import annotations

import unittest

from jsonschema import Draft202012Validator, FormatChecker

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


if __name__ == "__main__":
    unittest.main()
