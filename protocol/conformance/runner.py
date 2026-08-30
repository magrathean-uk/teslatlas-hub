from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource


ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = re.compile(r"^\$\{([^}]+)\}$")


class ConformanceError(Exception):
    pass


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def load_schemas() -> tuple[dict[str, Any], Registry]:
    schemas: dict[str, Any] = {}
    registry = Registry()
    for path in sorted((ROOT / "schemas").glob("*.schema.json")):
        schema = load_json(path)
        schema_id = schema["$id"]
        schemas[schema_id] = schema
        registry = registry.with_resource(schema_id, Resource.from_contents(schema))
    return schemas, registry


def semver(value: str) -> tuple[int, int, int]:
    parts = tuple(int(part) for part in value.split("."))
    if len(parts) != 3:
        raise ConformanceError(f"invalid semantic version: {value}")
    return parts  # type: ignore[return-value]


def mapping_value(mapping: dict[str, Any], key: str) -> Any:
    if key in mapping:
        return mapping[key]
    matches = [value for candidate, value in mapping.items() if candidate.lower() == key.lower()]
    if len(matches) == 1:
        return matches[0]
    raise KeyError(key)


def lookup_tokens(value: Any, tokens: list[str]) -> Any:
    current = value
    for token in tokens:
        if isinstance(current, dict):
            current = mapping_value(current, token)
        elif isinstance(current, list):
            current = current[int(token)]
        else:
            raise KeyError(token)
    return current


def lookup_reference(context: dict[str, Any], reference: str) -> Any:
    tokens = reference.split(".")
    if not tokens:
        raise KeyError(reference)
    return lookup_tokens(mapping_value(context, tokens[0]), tokens[1:])


def lookup_pointer(value: Any, pointer: str) -> Any:
    if pointer == "":
        return value
    if not pointer.startswith("/"):
        raise KeyError(pointer)
    tokens = [token.replace("~1", "/").replace("~0", "~") for token in pointer[1:].split("/")]
    return lookup_tokens(value, tokens)


def resolve_templates(value: Any, profile: str, context: dict[str, Any]) -> Any:
    if isinstance(value, str):
        match = TEMPLATE.match(value)
        if match is None:
            return value
        reference = match.group(1)
        if reference == "profile":
            return profile
        return lookup_reference(context, reference)
    if isinstance(value, list):
        return [resolve_templates(item, profile, context) for item in value]
    if isinstance(value, dict):
        return {
            key: resolve_templates(item, profile, context) for key, item in value.items()
        }
    return value


def normalize_headers(response: dict[str, Any]) -> dict[str, Any]:
    headers = response.get("headers")
    if not isinstance(headers, dict):
        return {}
    return {key.lower(): value for key, value in headers.items()}


def validate_assertion(
    response: dict[str, Any],
    assertion: dict[str, Any],
    context: dict[str, Any],
) -> str | None:
    path = assertion["path"]
    operation = assertion["op"]
    try:
        actual = lookup_pointer(response, path)
    except (KeyError, IndexError, ValueError, TypeError):
        return f"{path}: value does not exist"

    if operation == "exists":
        return None
    if operation == "equals":
        expected = assertion.get("value")
        return None if actual == expected else f"{path}: expected {expected!r}, got {actual!r}"
    if operation == "not_equals":
        expected = assertion.get("value")
        return None if actual != expected else f"{path}: unexpectedly equals {expected!r}"
    if operation in {"same_as", "not_same_as"}:
        try:
            expected = lookup_reference(context, assertion["ref"])
        except (KeyError, IndexError, ValueError, TypeError):
            return f"{path}: assertion reference does not exist: {assertion.get('ref')}"
        same = actual == expected
        if operation == "same_as" and not same:
            return f"{path}: expected same value as {assertion['ref']}"
        if operation == "not_same_as" and same:
            return f"{path}: expected different value from {assertion['ref']}"
        return None
    if operation == "matches":
        pattern = assertion.get("value")
        if not isinstance(actual, str) or not isinstance(pattern, str):
            return f"{path}: matches requires string value and pattern"
        return None if re.search(pattern, actual) else f"{path}: {actual!r} does not match {pattern!r}"
    if operation == "is_empty":
        return None if actual in ("", [], {}) else f"{path}: expected empty value"
    if operation == "is_not_empty":
        return None if actual not in ("", [], {}, None) else f"{path}: expected non-empty value"
    if operation == "unique":
        if not isinstance(actual, list):
            return f"{path}: unique requires an array"
        rendered = [json.dumps(item, sort_keys=True, separators=(",", ":")) for item in actual]
        return None if len(rendered) == len(set(rendered)) else f"{path}: values are not unique"
    return f"{path}: unsupported assertion operation {operation}"


def validate_response(
    response: dict[str, Any],
    expectation: dict[str, Any],
    profile: str,
    context: dict[str, Any],
    schemas: dict[str, Any],
    registry: Registry,
) -> list[str]:
    errors: list[str] = []
    expected = resolve_templates(expectation, profile, context)
    if response.get("status") != expected["status"]:
        errors.append(f"status: expected {expected['status']}, got {response.get('status')}")

    normalized_headers = normalize_headers(response)
    for name in expected.get("headers_present", []):
        if name.lower() not in normalized_headers:
            errors.append(f"header missing: {name}")
    for name, value in expected.get("headers_equal", {}).items():
        actual = normalized_headers.get(name.lower())
        if actual != value:
            errors.append(f"header {name}: expected {value!r}, got {actual!r}")

    if expected.get("body_absent") and "body" in response:
        errors.append("body must be absent")

    body_schema = expected.get("body_schema")
    if body_schema is not None:
        if "body" not in response:
            errors.append("body is required for schema validation")
        elif body_schema not in schemas:
            errors.append(f"unknown body schema: {body_schema}")
        else:
            validator = Draft202012Validator(
                schemas[body_schema], registry=registry, format_checker=FormatChecker()
            )
            for error in validator.iter_errors(response["body"]):
                location = "/".join(str(item) for item in error.absolute_path)
                errors.append(f"body schema {location}: {error.message}")

    event_schema = expected.get("event_schema")
    if event_schema is not None:
        events = response.get("events")
        if not isinstance(events, list):
            errors.append("events array is required")
        elif event_schema not in schemas:
            errors.append(f"unknown event schema: {event_schema}")
        else:
            validator = Draft202012Validator(
                schemas[event_schema], registry=registry, format_checker=FormatChecker()
            )
            for index, event in enumerate(events):
                if event.get("id") != event.get("data", {}).get("event_id"):
                    errors.append(f"events/{index}: id differs from data.event_id")
                if event.get("event") != event.get("data", {}).get("event_type"):
                    errors.append(f"events/{index}: event differs from data.event_type")
                for error in validator.iter_errors(event.get("data")):
                    location = "/".join(str(item) for item in error.absolute_path)
                    errors.append(f"events/{index} schema {location}: {error.message}")

    for assertion in expected.get("assertions", []):
        error = validate_assertion(response, assertion, context)
        if error is not None:
            errors.append(error)
    return errors


def load_contract() -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    manifest = load_json(ROOT / "compatibility" / "manifest.json")
    profiles = {
        entry["version"]: load_json(ROOT / entry["path"])
        for entry in manifest["profiles"]
    }
    cases = {
        item["case_id"]: item
        for path in sorted((ROOT / "conformance" / "cases").glob("*.json"))
        for item in [load_json(path)]
    }
    return manifest, profiles, cases


def validate_contract_artifacts(
    manifest: dict[str, Any],
    profiles: dict[str, Any],
    cases: dict[str, Any],
    schemas: dict[str, Any],
    registry: Registry,
) -> None:
    schema_id = "urn:teslatlas:protocol:schema:conformance:1.2.0"
    validator = Draft202012Validator(
        schemas[schema_id], registry=registry, format_checker=FormatChecker()
    )
    for label, value in [
        ("manifest", manifest),
        *[(f"profile {name}", profile) for name, profile in profiles.items()],
        *[(f"case {name}", case) for name, case in cases.items()],
    ]:
        errors = list(validator.iter_errors(value))
        if errors:
            raise ConformanceError(f"{label}: {errors[0].message}")
    for version, profile in profiles.items():
        expected = {
            case_id
            for case_id, case in cases.items()
            if semver(case["introduced_in"]) <= semver(version)
        }
        if set(profile["cases"]) != expected:
            raise ConformanceError(f"profile {version} does not cover its complete case set")


def adapter_command(adapter: str | None) -> list[str]:
    path = (
        ROOT / "conformance" / "adapters" / "reference_adapter.py"
        if adapter is None
        else Path(adapter).expanduser().resolve()
    )
    if not path.is_file():
        raise ConformanceError(f"adapter does not exist: {path}")
    if path.suffix == ".py":
        return [sys.executable, str(path)]
    return [str(path)]


def run() -> int:
    parser = argparse.ArgumentParser(description="Run Teslatlas language-neutral conformance cases")
    parser.add_argument("--profile", action="append", help="protocol profile; repeatable")
    parser.add_argument("--adapter", help="JSONL adapter executable or Python script")
    parser.add_argument("--json", action="store_true", help="emit one machine-readable summary")
    args = parser.parse_args()

    schemas, registry = load_schemas()
    manifest, profiles, cases = load_contract()
    validate_contract_artifacts(manifest, profiles, cases, schemas, registry)
    selected_profiles = args.profile or manifest["supported_profiles"]
    unknown = [item for item in selected_profiles if item not in profiles]
    if unknown:
        raise ConformanceError(f"unknown profile: {', '.join(unknown)}")

    command = adapter_command(args.adapter)
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    if process.stdin is None or process.stdout is None or process.stderr is None:
        raise ConformanceError("failed to open adapter pipes")

    runs = 0
    passed = 0
    failures: list[dict[str, Any]] = []
    try:
        for profile_name in selected_profiles:
            profile = profiles[profile_name]
            for case_id in profile["cases"]:
                runs += 1
                case_value = cases[case_id]
                context: dict[str, Any] = {}
                case_errors: list[str] = []
                for step_value in case_value["steps"]:
                    step_id = step_value["step_id"]
                    try:
                        resolved_request = resolve_templates(
                            step_value["request"], profile_name, context
                        )
                    except (KeyError, IndexError, ValueError, TypeError) as error:
                        case_errors.append(f"{step_id}: request template failed: {error}")
                        break
                    envelope = {
                        "protocol": manifest["runner_protocol"],
                        "profile": profile_name,
                        "case_id": case_id,
                        "step_id": step_id,
                        "capability": case_value["capability"],
                        "request": resolved_request,
                    }
                    process.stdin.write(json.dumps(envelope, separators=(",", ":")) + "\n")
                    process.stdin.flush()
                    line = process.stdout.readline()
                    if not line:
                        case_errors.append(f"{step_id}: adapter closed stdout")
                        break
                    try:
                        observed = json.loads(line)
                        if observed.get("case_id") != case_id or observed.get("step_id") != step_id:
                            raise ConformanceError("adapter response correlation mismatch")
                        response = observed["response"]
                    except (json.JSONDecodeError, KeyError, ConformanceError) as error:
                        case_errors.append(f"{step_id}: invalid adapter response: {error}")
                        break
                    step_errors = validate_response(
                        response,
                        step_value["expect"],
                        profile_name,
                        context,
                        schemas,
                        registry,
                    )
                    context[step_id] = response
                    case_errors.extend(f"{step_id}: {item}" for item in step_errors)
                if case_errors:
                    failures.append(
                        {"profile": profile_name, "case_id": case_id, "errors": case_errors}
                    )
                    if not args.json:
                        print(f"FAIL {profile_name} {case_id}: {case_errors[0]}")
                else:
                    passed += 1
                    if not args.json:
                        print(f"PASS {profile_name} {case_id}")
    finally:
        process.stdin.close()
        adapter_stderr = process.stderr.read()
        return_code = process.wait(timeout=10)

    if return_code != 0:
        failures.append(
            {
                "profile": None,
                "case_id": None,
                "errors": [f"adapter exited {return_code}: {adapter_stderr.strip()}"],
            }
        )
    summary = {
        "adapter": "reference" if args.adapter is None else args.adapter,
        "profiles": selected_profiles,
        "runs": runs,
        "passed": passed,
        "failed": runs - passed + (1 if return_code != 0 else 0),
        "failures": failures,
    }
    if args.json:
        print(json.dumps(summary, sort_keys=True, separators=(",", ":")))
    else:
        print(
            f"profiles={len(selected_profiles)} runs={runs} passed={passed} failed={summary['failed']}"
        )
    return 0 if summary["failed"] == 0 else 1


def main() -> int:
    try:
        return run()
    except (ConformanceError, OSError, subprocess.SubprocessError) as error:
        print(json.dumps({"error": str(error)}, separators=(",", ":")))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

