from __future__ import annotations

import unittest
from pathlib import Path
from typing import Any

from openapi_spec_validator import validate

from tests.support import ROOT, load_json


REQUIRED_OPERATIONS = {
    ("/.well-known/teslatlas-hub", "get"),
    ("/v1/vehicles", "get"),
    ("/v1/vehicles/{vehicle_id}/current", "get"),
    ("/v1/vehicles/{vehicle_id}/drives", "get"),
    ("/v1/drives/{drive_id}", "get"),
    ("/v1/drives/{drive_id}/positions", "get"),
    ("/v1/vehicles/{vehicle_id}/charges", "get"),
    ("/v1/charges/{charge_id}", "get"),
    ("/v1/charges/{charge_id}/samples", "get"),
    ("/v1/vehicles/{vehicle_id}/states", "get"),
    ("/v1/vehicles/{vehicle_id}/updates", "get"),
    ("/v1/events", "get"),
    ("/v1/data-quality", "get"),
    ("/v1/commands", "post"),
    ("/v1/commands/{command_id}", "get"),
    ("/v1/vehicles/{vehicle_id}/metadata", "get"),
    ("/v1/vehicles/{vehicle_id}/metadata", "post"),
    ("/v1/metadata/{metadata_id}", "get"),
    ("/v1/metadata/{metadata_id}", "put"),
    ("/v1/metadata/{metadata_id}", "delete"),
}

PAGINATED_OPERATIONS = {
    ("/v1/vehicles", "get"),
    ("/v1/vehicles/{vehicle_id}/drives", "get"),
    ("/v1/drives/{drive_id}/positions", "get"),
    ("/v1/vehicles/{vehicle_id}/charges", "get"),
    ("/v1/charges/{charge_id}/samples", "get"),
    ("/v1/vehicles/{vehicle_id}/states", "get"),
    ("/v1/vehicles/{vehicle_id}/updates", "get"),
    ("/v1/data-quality", "get"),
    ("/v1/vehicles/{vehicle_id}/metadata", "get"),
}


def resolve_local(document: dict[str, Any], value: dict[str, Any]) -> dict[str, Any]:
    while "$ref" in value:
        reference = value["$ref"]
        if not reference.startswith("#/"):
            return value
        current: Any = document
        for token in reference[2:].split("/"):
            current = current[token.replace("~1", "/").replace("~0", "~")]
        value = current
    return value


class OpenAPIContractTests(unittest.TestCase):
    relative_path = "openapi/teslatlas-v1.openapi.json"

    @property
    def document(self) -> dict[str, Any]:
        return load_json(self.relative_path)

    def operation(self, path: str, method: str) -> dict[str, Any]:
        return self.document["paths"][path][method]

    def parameters(self, path: str, method: str) -> dict[str, dict[str, Any]]:
        path_item = self.document["paths"][path]
        raw = [*path_item.get("parameters", []), *self.operation(path, method).get("parameters", [])]
        resolved = [resolve_local(self.document, parameter) for parameter in raw]
        return {parameter["name"].lower(): parameter for parameter in resolved}

    def response(self, path: str, method: str, status: str) -> dict[str, Any]:
        raw = self.operation(path, method)["responses"][status]
        return resolve_local(self.document, raw)

    def test_openapi_is_valid_3_1_with_explicit_2020_12_dialect(self) -> None:
        self.assertEqual("3.1.1", self.document["openapi"])
        self.assertEqual(
            "https://json-schema.org/draft/2020-12/schema",
            self.document["jsonSchemaDialect"],
        )
        path = ROOT / self.relative_path
        validate(self.document, base_uri=path.as_uri())

    def test_complete_v1_query_command_event_and_metadata_surface_is_declared(self) -> None:
        actual = {
            (path, method)
            for path, path_item in self.document["paths"].items()
            for method in path_item
            if method in {"get", "post", "put", "delete", "patch"}
        }
        self.assertEqual(REQUIRED_OPERATIONS, actual)

    def test_pagination_exposes_only_opaque_cursor_and_bounded_limit(self) -> None:
        for path, method in sorted(PAGINATED_OPERATIONS):
            with self.subTest(path=path):
                parameters = self.parameters(path, method)
                self.assertIn("cursor", parameters)
                self.assertIn("limit", parameters)
                self.assertEqual(500, parameters["limit"]["schema"]["maximum"])
                self.assertFalse({"offset", "page", "page_size"} & parameters.keys())

        cursor_policy = self.document["x-teslatlas-cursors"]
        self.assertTrue(cursor_policy["opaque"])
        self.assertEqual(
            ["endpoint", "normalized_query", "principal", "authorization_revision", "snapshot"],
            cursor_policy["bound_to"],
        )
        self.assertEqual(
            ["invalid_cursor", "cursor_expired", "cursor_query_mismatch", "cursor_scope_changed"],
            cursor_policy["errors"],
        )

    def test_conditional_gets_define_etag_if_none_match_and_304(self) -> None:
        for path, method in sorted(REQUIRED_OPERATIONS):
            if method != "get" or path == "/v1/events":
                continue
            with self.subTest(path=path):
                parameters = self.parameters(path, method)
                self.assertIn("if-none-match", parameters)
                ok = self.response(path, method, "200")
                self.assertIn("ETag", ok["headers"])
                not_modified = self.response(path, method, "304")
                self.assertIn("ETag", not_modified["headers"])
                self.assertNotIn("content", not_modified)

        etag_policy = self.document["x-teslatlas-etags"]
        self.assertEqual("representation", etag_policy["validator_of"])
        self.assertEqual("weak", etag_policy["if_none_match_comparison"])
        self.assertEqual("strong", etag_policy["if_match_comparison"])

    def test_problem_details_use_stable_rfc_9457_shape(self) -> None:
        response = resolve_local(self.document, self.document["components"]["responses"]["Problem"])
        media = response["content"]["application/problem+json"]
        self.assertEqual(
            "#/components/schemas/Problem", media["schema"]["$ref"]
        )
        self.assertIn("Teslatlas-Protocol-Version", response["headers"])

    def test_commands_are_idempotency_keyed_asynchronous_jobs(self) -> None:
        operation = self.operation("/v1/commands", "post")
        parameters = self.parameters("/v1/commands", "post")
        self.assertTrue(parameters["idempotency-key"]["required"])
        accepted = self.response("/v1/commands", "post", "202")
        self.assertIn("Location", accepted["headers"])
        self.assertEqual(
            "#/components/schemas/Command/$defs/command_job",
            accepted["content"]["application/json"]["schema"]["$ref"],
        )
        self.assertNotIn("200", operation["responses"])
        self.assertIn("409", operation["responses"])

    def test_metadata_writes_require_if_match_and_define_revision_conflict(self) -> None:
        for method in ("put", "delete"):
            with self.subTest(method=method):
                parameters = self.parameters("/v1/metadata/{metadata_id}", method)
                self.assertTrue(parameters["if-match"]["required"])
                operation = self.operation("/v1/metadata/{metadata_id}", method)
                self.assertIn("409", operation["responses"])
                self.assertIn("428", operation["responses"])

    def test_contract_limits_are_machine_readable_and_match_discovery(self) -> None:
        discovery_limits = load_json("examples/discovery.json")["limits"]
        self.assertEqual(discovery_limits, self.document["x-teslatlas-limits"])


if __name__ == "__main__":
    unittest.main()
