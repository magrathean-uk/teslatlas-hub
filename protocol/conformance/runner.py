from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
from functools import lru_cache
from pathlib import Path
from queue import Empty, Queue
from threading import Lock, Thread
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker
from jsonschema.exceptions import SchemaError
from referencing import Registry, Resource


ROOT = Path(__file__).resolve().parents[1]
TEMPLATE = re.compile(r"^\$\{([^}]+)\}$")
DISCOVERY_SCHEMA = "urn:teslatlas:protocol:schema:discovery:1.2.0"
METADATA_SCHEMA = "urn:teslatlas:protocol:schema:metadata:1.2.0"


class ConformanceError(Exception):
    pass


def reject_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON constant: {value}")


def strict_loads(value: str) -> Any:
    return json.loads(value, parse_constant=reject_constant)


def load_json(path: Path) -> Any:
    return strict_loads(path.read_text(encoding="utf-8"))


def load_schemas() -> tuple[dict[str, Any], Registry]:
    schemas: dict[str, Any] = {}
    registry = Registry()
    for path in sorted((ROOT / "schemas").glob("*.schema.json")):
        schema = load_json(path)
        schema_id = schema["$id"]
        schemas[schema_id] = schema
        registry = registry.with_resource(schema_id, Resource.from_contents(schema))
    return schemas, registry


@lru_cache(maxsize=1)
def event_introduced_versions() -> dict[str, str]:
    contract = load_json(ROOT / "events" / "teslatlas-v1.sse.json")
    events = contract.get("events", []) if isinstance(contract, dict) else []
    return {
        item["name"]: item["introduced_in"]
        for item in events
        if isinstance(item, dict)
        and isinstance(item.get("name"), str)
        and isinstance(item.get("introduced_in"), str)
    }


def semver(value: Any) -> tuple[int, int, int]:
    if not isinstance(value, str):
        raise ConformanceError(f"invalid semantic version: {value!r}")
    parts = tuple(int(part) for part in value.split("."))
    if len(parts) != 3:
        raise ConformanceError(f"invalid semantic version: {value}")
    return parts  # type: ignore[return-value]


def validate_discovery_semantics(document: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    try:
        protocol = document["protocol"]
        supported = protocol["supported_versions"]
        if not isinstance(supported, list) or not supported:
            raise ConformanceError("supported_versions must be a non-empty array")
        parsed = [semver(value) for value in supported]
        current = semver(protocol["current_version"])
        minimum = semver(protocol["minimum_client_version"])
    except (KeyError, TypeError, ValueError, ConformanceError) as error:
        return [f"discovery semantic fields are invalid: {error}"]

    if parsed != sorted(parsed):
        errors.append("supported_versions must be in ascending semantic-version order")
    if current != parsed[-1]:
        errors.append("current_version must equal the newest supported version")
    if minimum != parsed[0]:
        errors.append("minimum_client_version must equal the oldest supported version")
    if any(version[0] != current[0] for version in parsed):
        errors.append("all supported versions must share the current major version")

    capabilities = document.get("capabilities", [])
    if not isinstance(capabilities, list):
        return [*errors, "capabilities must be an array"]
    capability_ids = [item.get("id") for item in capabilities if isinstance(item, dict)]
    invalid_ids = [item for item in capability_ids if not isinstance(item, str)]
    if invalid_ids:
        errors.append("capability IDs must be strings")
    valid_ids = [item for item in capability_ids if isinstance(item, str)]
    if len(valid_ids) != len(set(valid_ids)):
        errors.append("capability IDs must be unique")
    for capability in capabilities:
        if not isinstance(capability, dict):
            continue
        commands = capability.get("commands", [])
        if not isinstance(commands, list):
            errors.append(f"commands must be an array in {capability.get('id')}")
            continue
        names = [item.get("name") for item in commands if isinstance(item, dict)]
        invalid_names = [item for item in names if not isinstance(item, str)]
        if invalid_names:
            errors.append(f"command names must be strings in {capability.get('id')}")
        valid_names = [item for item in names if isinstance(item, str)]
        if len(valid_names) != len(set(valid_names)):
            errors.append(f"command names must be unique in {capability.get('id')}")
        for command in commands:
            if not isinstance(command, dict):
                continue
            for field in ("parameters_schema", "expected_state_schema"):
                schema = command.get(field)
                if isinstance(schema, dict):
                    try:
                        Draft202012Validator.check_schema(schema)
                    except SchemaError as error:
                        errors.append(
                            f"{capability.get('id')} {command.get('name')} {field} is invalid: {error.message}"
                        )
        deprecation = capability.get("deprecation")
        if isinstance(deprecation, dict):
            deprecated_at = deprecation.get("deprecated_at")
            sunset_at = deprecation.get("sunset_at")
            if (
                isinstance(deprecated_at, str)
                and isinstance(sunset_at, str)
                and sunset_at < deprecated_at
            ):
                errors.append(
                    f"sunset_at must not precede deprecated_at for {capability.get('id')}"
                )
    return errors


def validate_metadata_history(history: Any) -> list[str]:
    if not isinstance(history, list) or not history:
        return ["metadata audit history must be a non-empty array"]
    errors: list[str] = []
    previous_hash: Any = None
    for index, entry in enumerate(history):
        if not isinstance(entry, dict):
            errors.append(f"metadata audit history/{index} must be an object")
            continue
        expected_revision = index + 1
        if entry.get("revision") != expected_revision:
            errors.append(
                f"metadata audit history/{index} revision must be {expected_revision}"
            )
        expected_action = "created" if index == 0 else "updated"
        if entry.get("action") != expected_action:
            errors.append(
                f"metadata audit history/{index} action must be {expected_action}"
            )
        if entry.get("previous_hash") != previous_hash:
            errors.append(
                f"metadata audit history/{index} previous_hash must match the prior new_hash"
            )
        previous_hash = entry.get("new_hash")
    return errors


def validate_metadata_semantics(document: dict[str, Any]) -> list[str]:
    audit = document.get("audit")
    if isinstance(audit, list):
        errors = validate_metadata_history(audit)
        if audit and isinstance(audit[-1], dict):
            terminal = audit[-1]
            comparisons = (
                ("revision", "revision"),
                ("updated_at", "at"),
                ("updated_by", "actor_id"),
            )
            for document_field, audit_field in comparisons:
                if document.get(document_field) != terminal.get(audit_field):
                    errors.append(
                        f"metadata {document_field} must match final audit {audit_field}"
                    )
        if audit and isinstance(audit[0], dict):
            first = audit[0]
            for document_field, audit_field in (
                ("created_at", "at"),
                ("created_by", "actor_id"),
            ):
                if document.get(document_field) != first.get(audit_field):
                    errors.append(
                        f"metadata {document_field} must match initial audit {audit_field}"
                    )
        return errors
    if isinstance(audit, dict):
        history = audit.get("history")
        errors = validate_metadata_history(history)
        deletion = audit.get("deletion")
        if not isinstance(deletion, dict):
            return [*errors, "metadata audit deletion must be an object"]
        if isinstance(history, list) and history:
            terminal_revision = (
                history[-1].get("revision")
                if isinstance(history[-1], dict)
                else None
            )
            if (
                not isinstance(terminal_revision, int)
                or deletion.get("revision") != terminal_revision + 1
            ):
                errors.append(
                    "metadata deletion revision must follow the live audit history"
                )
        return errors
    return []


EVENT_RESOURCE_FIELDS = {
    "observation.admitted": "observation_id",
    "vehicle.current.changed": "vehicle_id",
    "drive.started": "drive_id",
    "drive.updated": "drive_id",
    "drive.ended": "drive_id",
    "charge.started": "charge_id",
    "charge.updated": "charge_id",
    "charge.ended": "charge_id",
    "state.changed": "state_id",
    "software_update.changed": "update_id",
    "data_quality.changed": "subject_id",
    "command.changed": "command_id",
    "metadata.changed": "metadata_id",
}


def validate_event_semantics(event: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    data = event.get("data")
    if not isinstance(data, dict):
        return ["event data must be an object"]

    data_revision = data.get("revision")
    audit = data.get("audit")
    if data_revision is None and isinstance(audit, dict):
        deletion = audit.get("deletion")
        if isinstance(deletion, dict):
            data_revision = deletion.get("revision")
    if data_revision is not None and event.get("revision") != data_revision:
        errors.append("event revision must equal data revision")
    if "vehicle_id" in data and event.get("vehicle_id") != data["vehicle_id"]:
        errors.append("event vehicle_id must equal data vehicle_id")
    identifier_field = EVENT_RESOURCE_FIELDS.get(event.get("event_type"))
    if identifier_field is not None and identifier_field in data:
        if event.get("resource_id") != data[identifier_field]:
            errors.append(f"event resource_id must equal data {identifier_field}")
    return errors


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
    request: dict[str, Any],
    expectation: dict[str, Any],
    profile: str,
    context: dict[str, Any],
    schemas: dict[str, Any],
    registry: Registry,
) -> list[str]:
    errors: list[str] = []
    try:
        expected = resolve_templates(expectation, profile, context)
    except (KeyError, IndexError, ValueError, TypeError) as error:
        return [f"expectation template failed: {error}"]
    if response.get("status") != expected["status"]:
        errors.append(f"status: expected {expected['status']}, got {response.get('status')}")

    normalized_headers = normalize_headers(response)

    def require_header(name: str, value: str | None = None) -> None:
        actual = normalized_headers.get(name.lower())
        if actual is None:
            errors.append(f"header missing: {name}")
        elif value is not None and actual != value:
            errors.append(f"header {name}: expected {value!r}, got {actual!r}")

    is_json_get = (
        request.get("method") == "GET"
        and response.get("status") == 200
        and expected.get("body_schema") is not None
    )
    if is_json_get:
        require_header("Content-Type", "application/json")
        require_header("ETag")
        require_header("Cache-Control")
        require_header("Vary")
        if str(request.get("path", "")).startswith("/v1/"):
            requested_version = normalize_headers(request).get(
                "teslatlas-protocol-version"
            )
            require_header("Teslatlas-Protocol-Version", requested_version)
    if request.get("method") == "GET" and response.get("status") == 304:
        require_header("ETag")
        require_header("Cache-Control")
        require_header("Vary")
        if str(request.get("path", "")).startswith("/v1/"):
            requested_version = normalize_headers(request).get(
                "teslatlas-protocol-version"
            )
            require_header("Teslatlas-Protocol-Version", requested_version)
    for name in expected.get("headers_present", []):
        if name.lower() not in normalized_headers:
            errors.append(f"header missing: {name}")
    for name, value in expected.get("headers_equal", {}).items():
        actual = normalized_headers.get(name.lower())
        if actual != value:
            errors.append(f"header {name}: expected {value!r}, got {actual!r}")

    path = str(request.get("path", ""))
    method = request.get("method")
    status = response.get("status")
    is_metadata_entity = re.fullmatch(r"/v1/metadata/[^/]+", path) is not None
    is_metadata_create = (
        method == "POST"
        and re.fullmatch(r"/v1/vehicles/[^/]+/metadata", path) is not None
    )
    if (is_metadata_entity or is_metadata_create) and (
        isinstance(status, int) and (200 <= status < 300 or status == 304)
    ):
        etag = normalized_headers.get("etag")
        if etag is None:
            errors.append("header missing: ETag")
        elif re.fullmatch(r'"[^"\\\x00-\x20\x7f]+"', etag) is None:
            errors.append("header ETag must be a strong ETag")

    if expected.get("body_absent") and "body" in response:
        errors.append("body must be absent")

    body_schema = expected.get("body_schema")
    if body_schema is not None:
        schema_base = body_schema.partition("#")[0]
        if "body" not in response:
            errors.append("body is required for schema validation")
        elif schema_base not in schemas:
            errors.append(f"unknown body schema: {body_schema}")
        else:
            validator = Draft202012Validator(
                {"$ref": body_schema},
                registry=registry,
                format_checker=FormatChecker(),
            )
            for error in validator.iter_errors(response["body"]):
                location = "/".join(str(item) for item in error.absolute_path)
                errors.append(f"body schema {location}: {error.message}")
            if schema_base == DISCOVERY_SCHEMA and isinstance(response["body"], dict):
                errors.extend(
                    f"body discovery: {item}"
                    for item in validate_discovery_semantics(response["body"])
                )
            if schema_base == METADATA_SCHEMA and isinstance(response["body"], dict):
                errors.extend(
                    f"body metadata: {item}"
                    for item in validate_metadata_semantics(response["body"])
                )

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
                event_type = event.get("event")
                if isinstance(event_type, str):
                    introduced_in = event_introduced_versions().get(event_type)
                    if introduced_in is None:
                        errors.append(
                            f"events/{index}: event type is missing from the event catalogue"
                        )
                    elif semver(introduced_in) > semver(profile):
                        errors.append(
                            f"events/{index}: {event_type} is not available in profile {profile}"
                        )
                if event.get("id") != event.get("data", {}).get("event_id"):
                    errors.append(f"events/{index}: id differs from data.event_id")
                if event.get("event") != event.get("data", {}).get("event_type"):
                    errors.append(f"events/{index}: event differs from data.event_type")
                for error in validator.iter_errors(event.get("data")):
                    location = "/".join(str(item) for item in error.absolute_path)
                    errors.append(f"events/{index} schema {location}: {error.message}")
                if isinstance(event.get("data"), dict):
                    errors.extend(
                        f"events/{index}: {item}"
                        for item in validate_event_semantics(event["data"])
                    )
                    if event.get("event") == "metadata.changed":
                        payload = event["data"].get("data")
                        if not isinstance(payload, dict):
                            continue
                        errors.extend(
                            f"events/{index} metadata: {item}"
                            for item in validate_metadata_semantics(payload)
                        )

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
    supported = manifest["supported_profiles"]
    if list(profiles) != supported:
        raise ConformanceError("manifest profiles must exactly match supported_profiles order")
    if supported != sorted(supported, key=semver):
        raise ConformanceError("supported_profiles must be in ascending semantic-version order")
    if manifest["current_version"] != supported[-1]:
        raise ConformanceError("current_version must be the newest supported profile")

    previous_profile: dict[str, Any] | None = None
    for index, version in enumerate(supported):
        profile = profiles[version]
        if profile["protocol_version"] != version:
            raise ConformanceError(f"profile key/version mismatch: {version}")
        expected_parent = None if index == 0 else supported[index - 1]
        if profile["extends"] != expected_parent:
            raise ConformanceError(
                f"profile {version} must extend {expected_parent or 'no profile'}"
            )
        expected = {
            case_id
            for case_id, case in cases.items()
            if semver(case["introduced_in"]) <= semver(version)
        }
        if set(profile["cases"]) != expected:
            raise ConformanceError(f"profile {version} does not cover its complete case set")
        unavailable = {
            case_id: cases[case_id]["capability"]
            for case_id in profile["cases"]
            if cases[case_id]["capability"] not in profile["capabilities"]
        }
        if unavailable:
            raise ConformanceError(
                f"profile {version} has cases for unavailable capabilities: {unavailable}"
            )
        if previous_profile is not None:
            if not set(previous_profile["capabilities"]).issubset(profile["capabilities"]):
                raise ConformanceError(f"profile {version} removes inherited capabilities")
            if not set(previous_profile["cases"]).issubset(profile["cases"]):
                raise ConformanceError(f"profile {version} removes inherited cases")
        previous_profile = profile


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


class AdapterSession:
    STDERR_LIMIT = 65536

    def __init__(self, command: list[str], timeout_seconds: float) -> None:
        self.timeout_seconds = timeout_seconds
        self.process = subprocess.Popen(
            command,
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            start_new_session=os.name == "posix",
        )
        if (
            self.process.stdin is None
            or self.process.stdout is None
            or self.process.stderr is None
        ):
            self._stop()
            raise ConformanceError("failed to open adapter pipes")
        self.stdout_lines: Queue[str | None] = Queue()
        self.stderr_chunks: list[str] = []
        self.stderr_size = 0
        self.stderr_truncated = False
        self.stderr_lock = Lock()
        self.stdout_thread = Thread(target=self._drain_stdout, daemon=True)
        self.stderr_thread = Thread(target=self._drain_stderr, daemon=True)
        self.stdout_thread.start()
        self.stderr_thread.start()

    def _signal_process_tree(self, value: signal.Signals) -> None:
        try:
            if os.name == "posix":
                os.killpg(self.process.pid, value)
            elif self.process.poll() is None:
                if value == signal.SIGTERM:
                    self.process.terminate()
                else:
                    self.process.kill()
        except ProcessLookupError:
            pass

    def _drain_stdout(self) -> None:
        assert self.process.stdout is not None
        try:
            for line in self.process.stdout:
                self.stdout_lines.put(line)
        finally:
            self.stdout_lines.put(None)

    def _drain_stderr(self) -> None:
        assert self.process.stderr is not None
        for chunk in iter(lambda: self.process.stderr.read(8192), ""):
            with self.stderr_lock:
                remaining = self.STDERR_LIMIT - self.stderr_size
                if remaining > 0:
                    kept = chunk[:remaining]
                    self.stderr_chunks.append(kept)
                    self.stderr_size += len(kept)
                if len(chunk) > remaining:
                    self.stderr_truncated = True

    def stderr_text(self) -> str:
        with self.stderr_lock:
            value = "".join(self.stderr_chunks).strip()
            if self.stderr_truncated:
                value += " [stderr truncated]"
            return value

    def request(self, envelope: dict[str, Any]) -> str:
        if self.process.poll() is not None:
            raise ConformanceError(
                f"adapter exited {self.process.returncode}: {self.stderr_text()}"
            )
        assert self.process.stdin is not None
        try:
            self.process.stdin.write(
                json.dumps(envelope, separators=(",", ":"), allow_nan=False) + "\n"
            )
            self.process.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise ConformanceError(f"adapter stdin failed: {error}") from error
        try:
            line = self.stdout_lines.get(timeout=self.timeout_seconds)
        except Empty as error:
            self._stop()
            raise ConformanceError(
                f"adapter response timed out after {self.timeout_seconds:g} seconds"
            ) from error
        if line is None:
            raise ConformanceError(
                f"adapter closed stdout: {self.stderr_text()}"
            )
        return line

    def _stop(self) -> None:
        process = self.process
        if process.stdin is not None and not process.stdin.closed:
            try:
                process.stdin.close()
            except OSError:
                pass
        self._signal_process_tree(signal.SIGTERM)
        try:
            process.wait(timeout=0.5)
        except subprocess.TimeoutExpired:
            self._signal_process_tree(signal.SIGKILL)
            process.wait(timeout=1)
        if os.name == "posix":
            self._signal_process_tree(signal.SIGKILL)

    def _close_pipes(self) -> None:
        for stream in (self.process.stdout, self.process.stderr):
            if stream is not None and not stream.closed:
                try:
                    stream.close()
                except OSError:
                    pass

    def _extra_stdout(self) -> list[str]:
        extra: list[str] = []
        while True:
            try:
                line = self.stdout_lines.get_nowait()
            except Empty:
                break
            if line is not None and line.strip():
                extra.append(line.strip())
        return extra

    def finish(self) -> str | None:
        if self.process.stdin is not None and not self.process.stdin.closed:
            try:
                self.process.stdin.close()
            except OSError:
                pass
        timed_out = False
        try:
            return_code = self.process.wait(timeout=self.timeout_seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            self._stop()
            return_code = self.process.returncode
        self.stdout_thread.join(timeout=0.1)
        self.stderr_thread.join(timeout=0.1)
        descendants_retained_pipes = (
            self.stdout_thread.is_alive() or self.stderr_thread.is_alive()
        )
        if descendants_retained_pipes:
            self._signal_process_tree(signal.SIGTERM)
            self._signal_process_tree(signal.SIGKILL)
            self.stdout_thread.join(timeout=1)
            self.stderr_thread.join(timeout=1)
        extra_stdout = self._extra_stdout()
        self._close_pipes()
        if timed_out:
            return f"adapter did not exit after stdin EOF: {self.stderr_text()}"
        if return_code != 0:
            return f"adapter exited {return_code}: {self.stderr_text()}"
        if descendants_retained_pipes:
            return "adapter descendants retained stdout or stderr after exit"
        if extra_stdout:
            return "adapter emitted extra stdout after final response"
        return None


def adapter_response_validator(
    schemas: dict[str, Any], registry: Registry
) -> Draft202012Validator:
    schema_id = "urn:teslatlas:protocol:schema:conformance:1.2.0"
    return Draft202012Validator(
        {"$ref": f"{schema_id}#/$defs/adapter_response_envelope"},
        registry=registry,
        format_checker=FormatChecker(),
    )


def run() -> int:
    parser = argparse.ArgumentParser(description="Run Teslatlas language-neutral conformance cases")
    parser.add_argument("--profile", action="append", help="protocol profile; repeatable")
    parser.add_argument("--adapter", help="JSONL adapter executable or Python script")
    parser.add_argument("--config", help="private actual-Hub connection descriptor")
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=10.0,
        help="maximum seconds for each adapter response and shutdown",
    )
    parser.add_argument("--json", action="store_true", help="emit one machine-readable summary")
    args = parser.parse_args()
    if args.timeout_seconds <= 0:
        raise ConformanceError("timeout-seconds must be positive")

    if args.profile and "hub-http-v1@1.0.0" in args.profile:
        if args.profile != ["hub-http-v1@1.0.0"]:
            raise ConformanceError("actual-Hub acceptance must run separately from rich profiles")
        actual = ROOT / "conformance/adapters/actual-hub"
        if args.adapter is None or Path(args.adapter).resolve() != actual.resolve():
            raise ConformanceError("current-Hub acceptance requires the actual-hub adapter")
        command = [str(actual)]
        if args.config:
            command += ["--config", args.config]
        if args.json:
            command += ["--json"]
        return subprocess.run(command, check=False).returncode

    schemas, registry = load_schemas()
    manifest, profiles, cases = load_contract()
    validate_contract_artifacts(manifest, profiles, cases, schemas, registry)
    selected_profiles = args.profile or manifest["supported_profiles"]
    unknown = [item for item in selected_profiles if item not in profiles]
    if unknown:
        raise ConformanceError(f"unknown profile: {', '.join(unknown)}")

    command = adapter_command(args.adapter)
    response_validator = adapter_response_validator(schemas, registry)

    runs = 0
    passed = 0
    failures: list[dict[str, Any]] = []
    for profile_name in selected_profiles:
        profile = profiles[profile_name]
        for case_id in profile["cases"]:
            runs += 1
            case_value = cases[case_id]
            context: dict[str, Any] = {}
            case_errors: list[str] = []
            session = AdapterSession(command, args.timeout_seconds)
            try:
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
                    try:
                        line = session.request(envelope)
                        observed = strict_loads(line)
                        schema_errors = list(response_validator.iter_errors(observed))
                        if schema_errors:
                            raise ConformanceError(
                                f"adapter response schema: {schema_errors[0].message}"
                            )
                        if observed.get("case_id") != case_id or observed.get("step_id") != step_id:
                            raise ConformanceError("adapter response correlation mismatch")
                        response = observed["response"]
                        if type(response["status"]) is not int:
                            raise ConformanceError("adapter response status must be an integer")
                    except (KeyError, TypeError, ValueError, ConformanceError) as error:
                        case_errors.append(f"{step_id}: invalid adapter response: {error}")
                        break
                    step_errors = validate_response(
                        response,
                        resolved_request,
                        step_value["expect"],
                        profile_name,
                        context,
                        schemas,
                        registry,
                    )
                    context[step_id] = response
                    case_errors.extend(f"{step_id}: {item}" for item in step_errors)
            finally:
                finish_error = session.finish()
                if finish_error is not None:
                    case_errors.append(finish_error)
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
    summary = {
        "provenance": "harness-consistency" if args.adapter is None else "adapter-contract",
        "adapter": "reference" if args.adapter is None else args.adapter,
        "profiles": selected_profiles,
        "runs": runs,
        "passed": passed,
        "failed": runs - passed,
        "failures": failures,
    }
    if args.json:
        print(json.dumps(summary, sort_keys=True, separators=(",", ":"), allow_nan=False))
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
