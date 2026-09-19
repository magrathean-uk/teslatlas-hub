#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Fail-closed validation of the finite B-to-C evidence annotation delta.

The helper deliberately has no acceptance-capable command-line mode.  Receipt
semantics belong to the shared integrator's concrete parsers; callers must
supply a validator returning the closed result documented by
``final-receipt-validation.schema.json``.  This module independently reopens
and rehashes every bound byte before accepting that result.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
from typing import Any, Callable, Mapping, Sequence


HERE = Path(__file__).resolve().parent
EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
ALLOWED_PATHS = {
    "docs/compatibility/execution-state.json",
    "docs/compatibility/ecosystem-release.json",
    "docs/compatibility/acceptance.md",
    "docs/compatibility/receipt-index.json",
}
EXECUTION_POINTERS = {
    "/status", "/tasks/10", "/tasks/11", "/tasks/12", "/tasks/10a",
    "/evidence/final_cohort",
}
RELEASE_POINTERS = {"/test_receipt_paths"}
RECEIPT_INDEX_POINTERS = {"/receipts"}
KIND_SCOPES = {
    "matrix": "local-tested-cohort-not-published",
    "package": "local-built-not-published",
    "bootstrap": "local-source-candidate",
    "upgrade": "isolated-upgrade",
    "private-lane": "authorized-private-lane",
    "historical": "historical-local-evidence",
    "review": "independent-review",
}
SUPPORTED_ARTIFACT_SCHEMAS = {
    "matrix": "hub-compatibility-receipt-v2",
    "package": "hub-package-receipt-v1",
    "bootstrap": "hub-bootstrap-receipt-v2",
    "upgrade": "hub-upgrade-receipt-v1",
    "private-lane": "hub-private-lane-receipt-v1",
    "historical": "historical-evidence-v1",
    "review": "markdown-independent-review-v1",
}
MANDATORY_KINDS = {"matrix", "package", "bootstrap", "upgrade", "private-lane", "review"}
REQUIRED_EVIDENCE_KINDS = MANDATORY_KINDS | {"historical"}


class FinalizationError(RuntimeError):
    """The candidate finalization cannot support its requested disposition."""


def _candidate_source_identity_matches(
    release: Mapping[str, Any],
    tested_hub_identity: Mapping[str, Any],
    *,
    publication_performed: bool,
) -> bool:
    source_identity = release.get("source_identity")
    if (
        not isinstance(source_identity, dict)
        or set(source_identity) != {"kind", "commit", "status"}
        or source_identity.get("kind") != "git-commit"
    ):
        return False
    source_status = source_identity.get("status")
    source_commit = source_identity.get("commit")
    if source_status == "unbound-candidate":
        return (
            source_commit is None
            and release.get("status") == "candidate"
            and publication_performed is False
        )
    return (
        source_status == "bound"
        and isinstance(source_commit, str)
        and re.fullmatch(r"[0-9a-f]{40}", source_commit) is not None
        and source_commit == tested_hub_identity.get("head")
    )


_SOURCE = None


def _source():
    global _SOURCE
    if _SOURCE is None:
        spec = importlib.util.spec_from_file_location("teslatlas_source_evidence", HERE / "source_evidence.py")
        module = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(module)
        _SOURCE = module
    return _SOURCE


def _schema(value: Any, filename: str, label: str) -> None:
    try:
        from jsonschema import Draft202012Validator
    except ImportError as error:
        raise FinalizationError("jsonschema dependency is unavailable") from error
    try:
        schema = _source()._read_json(HERE / filename, filename)
    except Exception as error:
        raise FinalizationError(str(error)) from error
    errors = sorted(Draft202012Validator(schema).iter_errors(value), key=lambda item: list(item.path))
    if errors:
        path = "/" + "/".join(str(item) for item in errors[0].path)
        raise FinalizationError(f"{label} violates schema at {path}")


def _strict(payload: bytes, label: str) -> Any:
    try:
        return _source()._strict_json(payload, label)
    except Exception as error:
        raise FinalizationError(str(error)) from error


def _bound(binding: Mapping[str, Any], label: str) -> tuple[Path, bytes]:
    try:
        return _source()._verify_binding(binding, label)
    except Exception as error:
        raise FinalizationError(str(error)) from error


def _snapshot(binding: Mapping[str, Any]) -> dict[str, Any]:
    try:
        return _source()._validate_snapshot(binding)
    except Exception as error:
        raise FinalizationError(str(error)) from error


def _snapshot_dependencies_external(
    binding: Mapping[str, Any], snapshot: Mapping[str, Any],
    protected_roots: Sequence[Path], label: str,
) -> None:
    try:
        paths = [_source()._canonical(binding["path"], f"{label} snapshot")]
        paths.append(_source()._canonical(snapshot["patch"]["path"], f"{label} snapshot patch"))
        paths.extend(
            _source()._canonical(member["blob"], f"{label} snapshot blob")
            for member in snapshot["members"] if member["blob"] is not None
        )
    except Exception as error:
        raise FinalizationError(str(error)) from error
    if any(_source()._within(path, root) for path in paths for root in protected_roots):
        raise FinalizationError(f"{label} snapshot dependency must be outside protected source/export/output roots")


def _receipt_sort(item: Mapping[str, Any]) -> tuple[str, str]:
    return str(item["kind"]), str(item["path"])


def render_acceptance(
    *, cohort_id: str, cohort_inputs: Mapping[str, str],
    final_receipts: Sequence[Mapping[str, str]],
    review_receipts: Sequence[Mapping[str, str]], status: str,
    publication_performed: bool,
) -> bytes:
    """Render the exact final acceptance annotation from external bindings."""
    if not isinstance(cohort_id, str) or not cohort_id or status not in {"passed", "failed", "pending"}:
        raise FinalizationError("acceptance render inputs are invalid")
    if publication_performed is not False:
        raise FinalizationError("acceptance renderer cannot claim publication")
    _schema({"schema_version": 1, "cohort_id": cohort_id, "receipts": list(final_receipts) + list(review_receipts)}, "receipt-index.schema.json", "acceptance receipts")
    if not isinstance(cohort_inputs, dict) or set(cohort_inputs) != {"path", "sha256"}:
        raise FinalizationError("acceptance cohort binding is invalid")
    lines = [
        "# Hub ecosystem compatibility acceptance", "",
        f"Cohort: `{cohort_id}`", f"Status: `{status}`",
        "Scope: `local-tested-cohort-not-published`",
        f"Cohort inputs: `{cohort_inputs['path']}` (`{cohort_inputs['sha256']}`)", "",
        "## Final evidence", "",
    ]
    for receipt in sorted(final_receipts, key=_receipt_sort):
        lines.append(f"- `{receipt['kind']}` `{receipt['scope']}`: `{receipt['path']}` (`{receipt['sha256']}`)")
    lines.extend(["", "## Independent reviews", ""])
    for receipt in sorted(review_receipts, key=_receipt_sort):
        lines.append(f"- `{receipt['path']}` (`{receipt['sha256']}`)")
    lines.extend([
        "", "Task 11 remains explicitly pending when its bound private-lane evidence is pending.",
        "The tested B cohort is a local candidate. The C checkout contains evidence annotations and is not a tested export.",
        "Publication performed: `false`", "",
    ])
    return "\n".join(lines).encode("utf-8")


def _validation_results(
    receipts: Sequence[Mapping[str, Any]], cohort_id: str,
    cohort_inputs: Mapping[str, str],
    validator: Callable[[Mapping[str, str], bytes, Mapping[str, Any]], Mapping[str, Any]] | None,
    required_gates: Mapping[str, Sequence[str]],
    protected_roots: Sequence[Path],
) -> list[dict[str, Any]]:
    if validator is None or not callable(validator):
        raise FinalizationError("a concrete receipt validator is required")
    if not isinstance(required_gates, dict) or set(required_gates) != MANDATORY_KINDS:
        raise FinalizationError("required gate contract is incomplete")
    if any(
        not isinstance(gates, (list, tuple)) or not gates
        or any(not isinstance(gate, str) or not gate for gate in gates)
        or len(set(gates)) != len(gates)
        for gates in required_gates.values()
    ):
        raise FinalizationError("required gate contract is malformed")
    seen_paths: set[str] = set()
    results: list[dict[str, Any]] = []
    for receipt in receipts:
        kind = receipt["kind"]
        if receipt["scope"] != KIND_SCOPES[kind]:
            raise FinalizationError("receipt kind and scope do not match")
        path, raw = _bound({"path": receipt["path"], "sha256": receipt["sha256"]}, f"{kind} receipt")
        if any(_source()._within(path, root) for root in protected_roots):
            raise FinalizationError(f"{kind} receipt must be outside protected source/export/output roots")
        if str(path) in seen_paths:
            raise FinalizationError("receipt bindings are duplicated")
        seen_paths.add(str(path))
        try:
            result = validator(
                dict(receipt), raw,
                {"cohort_id": cohort_id, "cohort_inputs": dict(cohort_inputs)},
            )
        except FinalizationError:
            raise
        except Exception as error:
            raise FinalizationError(f"receipt validator failed for {kind}") from error
        if not isinstance(result, dict):
            raise FinalizationError("receipt validation result must be a closed object")
        _schema(result, "final-receipt-validation.schema.json", "receipt validation result")
        for field in ("kind", "path", "sha256", "scope"):
            if result[field] != receipt[field]:
                raise FinalizationError("receipt validation result is stale or mismatched")
        if result["cohort_id"] != cohort_id:
            raise FinalizationError("receipt validation result has a foreign cohort")
        if kind == "historical":
            if result["tested_cohort_inputs"] is not None or not isinstance(result["historical_tested_inputs"], dict):
                raise FinalizationError("historical evidence lacks a separate tested input binding")
            historical_path, _historical_raw = _bound(
                result["historical_tested_inputs"], "historical tested input"
            )
            if result["historical_tested_inputs"] == cohort_inputs:
                raise FinalizationError("historical evidence cannot relabel current cohort inputs")
            if any(_source()._within(historical_path, root) for root in protected_roots):
                raise FinalizationError("historical tested input must be outside protected roots")
        else:
            if result["tested_cohort_inputs"] != cohort_inputs:
                raise FinalizationError("receipt tested cohort input binding does not match selected inputs")
            if result["historical_tested_inputs"] is not None:
                raise FinalizationError("current receipt contains a historical tested input binding")
        if result["artifact_schema"] != SUPPORTED_ARTIFACT_SCHEMAS[kind]:
            if kind == "matrix":
                raise FinalizationError("matrix receipt must use the installed aggregate v2 schema")
            raise FinalizationError(f"{kind} receipt schema is unsupported")
        required = set(required_gates.get(kind, ()))
        if kind in MANDATORY_KINDS and (not required or not required.issubset(set(result["gate_ids"]))):
            raise FinalizationError(f"{kind} receipt lacks mandatory gate coverage")
        if kind == "review":
            if result["status"] != "passed" or result["accepted_review_verdict"] is not True:
                raise FinalizationError("independent review is not accepted")
        elif result["accepted_review_verdict"] is not None:
            raise FinalizationError("non-review receipt contains a review verdict")
        elif kind == "private-lane":
            if result["status"] not in {"passed", "pending"}:
                raise FinalizationError("private-lane disposition is invalid")
        elif result["status"] != "passed":
            raise FinalizationError(f"{kind} receipt did not pass")
        results.append(dict(result))
    present = {item["kind"] for item in results}
    if not REQUIRED_EVIDENCE_KINDS.issubset(present):
        raise FinalizationError("final evidence omits a mandatory receipt kind")
    counts = {kind: sum(item["kind"] == kind for item in results) for kind in present}
    if any(counts.get(kind) != 1 for kind in ("matrix", "bootstrap", "private-lane")):
        raise FinalizationError("final evidence has an invalid aggregate receipt count")
    if counts.get("historical") != 1:
        raise FinalizationError("final evidence must contain exactly one historical A binding")
    return results


def _member_state(member: Mapping[str, Any] | None) -> tuple[Any, ...]:
    if member is None or member.get("type") == "deleted":
        return ("deleted", "0000", 0, None, None)
    return tuple(member[key] for key in ("type", "mode", "bytes", "sha256", "symlink_target"))


def _blob(member: Mapping[str, Any] | None, label: str) -> bytes:
    if member is None or member.get("blob") is None:
        return b""
    _path, payload = _bound({"path": member["blob"], "sha256": member["sha256"]}, label)
    return payload


def _escape_pointer(value: str) -> str:
    return value.replace("~", "~0").replace("/", "~1")


def _json_differences(before: Any, after: Any, prefix: str = "") -> set[str]:
    if type(before) is not type(after):
        return {prefix or "/"}
    if isinstance(before, dict):
        differences: set[str] = set()
        for key in sorted(set(before) | set(after)):
            pointer = prefix + "/" + _escape_pointer(key)
            if key not in before or key not in after:
                differences.add(pointer)
            else:
                differences.update(_json_differences(before[key], after[key], pointer))
        return differences
    if isinstance(before, list):
        if len(before) != len(after) or any(
            _json_differences(left, right, "") for left, right in zip(before, after)
        ):
            return {prefix or "/"}
        return set()
    return set() if before == after else {prefix or "/"}


def _binding_only(receipt: Mapping[str, Any]) -> dict[str, str]:
    return {"path": receipt["path"], "sha256": receipt["sha256"]}


def validate_finalization(
    path: Path | str, *,
    receipt_validator: Callable[
        [Mapping[str, str], bytes, Mapping[str, Any]], Mapping[str, Any]
    ] | None,
    required_gates: Mapping[str, Sequence[str]],
) -> dict[str, Any]:
    """Validate exact raw B/C state and all external receipt semantics.

    The validator callback receives the rehashed binding, its bytes, and a
    closed context with the exact selected cohort id and input binding.
    It must return exactly the fields in ``final-receipt-validation.schema.json``.
    """
    final_path, raw = _source()._binding(path, "finalization")
    value = _strict(raw, "finalization")
    _schema(value, "finalization.schema.json", "finalization")
    if value["publication_performed"] is not False:
        raise FinalizationError("publication must remain false")
    _cohort_path, cohort_raw = _bound(value["cohort_inputs"], "cohort inputs")
    try:
        cohort = _source().validate_cohort_inputs(
            value["cohort_inputs"]["path"], validation_mode="retained"
        )
    except Exception as error:
        raise FinalizationError(f"cohort inputs are invalid: {error}") from error
    if _strict(cohort_raw, "cohort inputs") != cohort:
        raise FinalizationError("cohort inputs changed during validation")
    if cohort["cohort_id"] != value["cohort_id"]:
        raise FinalizationError("cohort input identity does not match finalization")

    cohort_observations = {
        item["source_identity"]["role"]: item
        for item in cohort["repository_observations"]
    }
    source_roots = [Path(item["source_identity"]["repo"]) for item in cohort_observations.values()]
    export_roots: list[Path] = []
    output_roots: list[Path] = []
    for build in cohort["builds"]:
        _manifest_path, manifest_raw = _bound(build["input_manifest"], "build input manifest")
        manifest = _strict(manifest_raw, "build input manifest")
        export_roots.extend(Path(item["path"]) for item in manifest["export_roots"])
        output_roots.extend(Path(item["path"]) for item in build["outputs"])
    protected_roots = [*source_roots, *export_roots, *output_roots]
    if any(_source()._within(final_path, root) for root in protected_roots):
        raise FinalizationError("finalization must be outside protected source/export/output roots")
    if any(_source()._within(_cohort_path, root) for root in [*export_roots, *output_roots]):
        raise FinalizationError("cohort inputs must be outside protected export/output roots")

    all_receipts = list(value["final_receipts"]) + list(value["review_receipts"])
    if any(item["kind"] == "review" for item in value["final_receipts"]) or any(item["kind"] != "review" for item in value["review_receipts"]):
        raise FinalizationError("review receipt boundary is invalid")
    results = _validation_results(
        all_receipts, value["cohort_id"], value["cohort_inputs"],
        receipt_validator, required_gates, protected_roots,
    )
    if value["status"] == "passed" and any(item["status"] != "passed" and item["kind"] != "private-lane" for item in results):
        raise FinalizationError("passed finalization has an incomplete required gate")

    before_records = {item["repo"]: item for item in value["repository_before"]}
    after_records = {item["repo"]: item for item in value["repository_after"]}
    if len(before_records) != len(value["repository_before"]) or len(after_records) != len(value["repository_after"]) or set(before_records) != set(after_records):
        raise FinalizationError("repository observation sets are invalid")
    before_snapshots = {name: _snapshot({"path": item["path"], "sha256": item["sha256"]}) for name, item in before_records.items()}
    after_snapshots = {name: _snapshot({"path": item["path"], "sha256": item["sha256"]}) for name, item in after_records.items()}
    for name in before_snapshots:
        _snapshot_dependencies_external(
            before_records[name], before_snapshots[name], protected_roots, "B"
        )
        _snapshot_dependencies_external(
            after_records[name], after_snapshots[name], protected_roots, "C"
        )
    cohort_identities = {name: item["source_identity"] for name, item in cohort_observations.items()}
    if set(before_records) != set(cohort_identities):
        raise FinalizationError("B repository observations do not cover the cohort")
    for name in before_snapshots:
        expected_snapshot = cohort_observations[name]["snapshot"]
        actual_binding = {key: before_records[name][key] for key in ("path", "sha256")}
        if actual_binding != expected_snapshot:
            raise FinalizationError("B repository snapshot binding is not the exact cohort snapshot")
        before_identity = before_snapshots[name]["source_identity"]
        after_identity = after_snapshots[name]["source_identity"]
        if before_identity != cohort_identities[name]:
            raise FinalizationError("B repository snapshot does not match cohort inputs")
        if before_identity["repo"] != after_identity["repo"] or before_identity["head"] != after_identity["head"] or before_identity["role"] != after_identity["role"] or before_identity["role"] != name:
            raise FinalizationError("repository before/after identity lineage does not match")

    try:
        ignored_inputs_after = _source()._validate_ignored_input_observation(
            value["ignored_inputs_after"], value["cohort_inputs"], cohort,
            value["repository_after"], protected_roots,
        )
    except Exception as error:
        raise FinalizationError(f"C ignored-input validation failed: {error}") from error

    actual_changes: dict[tuple[str, str], tuple[Mapping[str, Any] | None, Mapping[str, Any] | None]] = {}
    for name in before_snapshots:
        before_members = {item["path"]: item for item in before_snapshots[name]["members"]}
        after_members = {item["path"]: item for item in after_snapshots[name]["members"]}
        for relative in sorted(set(before_members) | set(after_members)):
            if _member_state(before_members.get(relative)) != _member_state(after_members.get(relative)):
                actual_changes[(name, relative)] = (before_members.get(relative), after_members.get(relative))
    if set(path for _repo, path in actual_changes) - ALLOWED_PATHS or any(repo != "hub_source" for repo, _path in actual_changes):
        raise FinalizationError("B-to-C changes exceed the final annotation allowlist")
    if {path for _repo, path in actual_changes} != ALLOWED_PATHS:
        raise FinalizationError("B-to-C changes omit a required final annotation file")
    declared = {(item["repo"], item["path"]): item for item in value["changes"]}
    if len(declared) != len(value["changes"]) or set(declared) != set(actual_changes):
        raise FinalizationError("declared changes do not match raw B-to-C observations")

    parsed: dict[str, tuple[Any, Any]] = {}
    for key, (before_member, after_member) in actual_changes.items():
        change = declared[key]
        if change["before_sha256"] != ((before_member or {}).get("sha256") or EMPTY_SHA256) or change["after_sha256"] != ((after_member or {}).get("sha256") or EMPTY_SHA256):
            raise FinalizationError("change digest does not match raw snapshot")
        if before_member is not None and after_member is not None and (before_member["type"] != after_member["type"] or before_member["mode"] != after_member["mode"]):
            raise FinalizationError("allowed final file type or mode changed")
        relative = key[1]
        if relative.endswith(".json"):
            before_json = _strict(_blob(before_member, f"B {relative}"), f"B {relative}")
            after_json = _strict(_blob(after_member, f"C {relative}"), f"C {relative}")
            pointers = _json_differences(before_json, after_json)
            allowed = (
                EXECUTION_POINTERS if relative.endswith("execution-state.json")
                else RELEASE_POINTERS if relative.endswith("ecosystem-release.json")
                else RECEIPT_INDEX_POINTERS if relative.endswith("receipt-index.json")
                else set()
            )
            if pointers - allowed:
                if relative.endswith("execution-state.json") and any(pointer.startswith("/platforms") for pointer in pointers):
                    raise FinalizationError("execution-state platform changed after B")
                raise FinalizationError("unknown JSON leaf changed after B")
            if sorted(pointers) != change["json_pointers"]:
                raise FinalizationError("declared JSON pointers do not match raw delta")
            parsed[relative] = (before_json, after_json)
        elif change["json_pointers"]:
            raise FinalizationError("non-JSON final annotation has JSON pointers")

    expected_paths = sorted(item["path"] for item in value["final_receipts"])
    if len(expected_paths) != len(set(expected_paths)):
        raise FinalizationError("final receipt paths are duplicated")
    execution_before, execution_after = parsed["docs/compatibility/execution-state.json"]
    release_before, release_after = parsed["docs/compatibility/ecosystem-release.json"]
    if execution_before.get("evidence", {}).get("final_cohort") is not None:
        raise FinalizationError("B did not contain a null predeclared final cohort")
    private_pending = any(item["kind"] == "private-lane" and item["status"] == "pending" for item in results)
    expected_tasks = {"10": "complete", "11": "pending" if private_pending else "complete", "12": "complete", "10a": "complete"}
    if execution_after.get("status") != "complete" or any(execution_after.get("tasks", {}).get(key) != state for key, state in expected_tasks.items()):
        raise FinalizationError("execution-state final disposition is not independently derived")
    final_cohort = execution_after.get("evidence", {}).get("final_cohort")
    expected_final_cohort = {
        "cohort_id": value["cohort_id"], "cohort_inputs": value["cohort_inputs"],
        "matrix_receipt": _binding_only(next(item for item in value["final_receipts"] if item["kind"] == "matrix")),
        "upgrade_receipts": [_binding_only(item) for item in sorted(value["final_receipts"], key=lambda item: item["path"]) if item["kind"] == "upgrade"],
        "review_receipts": [_binding_only(item) for item in sorted(value["review_receipts"], key=lambda item: item["path"])],
        "acceptance_scope": "local-tested-cohort-not-published",
    }
    if final_cohort != expected_final_cohort:
        raise FinalizationError("execution-state final cohort binding is invalid")
    tested_hub_identity = cohort_identities.get("hub_source")
    if not isinstance(tested_hub_identity, dict):
        raise FinalizationError("tested cohort has no Hub source identity")
    if release_after.get("status") != "candidate" or not _candidate_source_identity_matches(
        release_after,
        tested_hub_identity,
        publication_performed=value["publication_performed"],
    ):
        raise FinalizationError(
            "candidate source identity is invalid or does not match the tested Hub cohort"
        )
    before_paths = release_before.get("test_receipt_paths")
    if not isinstance(before_paths, list) or before_paths != sorted(set(before_paths)) or not set(before_paths).issubset(expected_paths) or release_after.get("test_receipt_paths") != expected_paths:
        raise FinalizationError("ecosystem receipt paths are not an exact sorted append")

    after_members = {item["path"]: item for item in after_snapshots["hub_source"]["members"]}
    acceptance = _blob(after_members.get("docs/compatibility/acceptance.md"), "acceptance annotation")
    expected_acceptance = render_acceptance(
        cohort_id=value["cohort_id"], cohort_inputs=value["cohort_inputs"],
        final_receipts=value["final_receipts"], review_receipts=value["review_receipts"],
        status=value["status"], publication_performed=value["publication_performed"],
    )
    if acceptance != expected_acceptance:
        raise FinalizationError("acceptance rendering does not match frozen renderer")
    receipt_index = _strict(_blob(after_members.get("docs/compatibility/receipt-index.json"), "receipt index"), "receipt index")
    _schema(receipt_index, "receipt-index.schema.json", "receipt index")
    expected_index = {
        "schema_version": 1, "cohort_id": value["cohort_id"],
        "receipts": sorted(all_receipts, key=_receipt_sort),
    }
    if receipt_index != expected_index:
        raise FinalizationError("receipt index does not exactly bind final and review evidence")

    all_bound_paths = {item["path"] for item in all_receipts} | {
        value["cohort_inputs"]["path"], value["ignored_inputs_after"]["path"],
    }
    report_paths: set[str] = set()
    for binding in value["report_files"]:
        if binding["path"] == str(final_path):
            raise FinalizationError("self-referential finalization binding is forbidden")
        report_path, _ = _bound(binding, "final report")
        if any(_source()._within(report_path, root) for root in protected_roots):
            raise FinalizationError("final report must be outside protected source/export/output roots")
        if str(report_path) in report_paths:
            raise FinalizationError("final report bindings are duplicated")
        report_paths.add(str(report_path))
        all_bound_paths.add(str(report_path))
    if str(final_path) in all_bound_paths:
        raise FinalizationError("self-referential finalization binding is forbidden")
    if value["status"] != "passed":
        raise FinalizationError("finalization is not passed")
    return {
        "status": "passed", "cohort_id": value["cohort_id"],
        "validated_receipts": results, "ignored_inputs_after": value["ignored_inputs_after"],
        "changes": value["changes"],
    }


__all__ = ["FinalizationError", "render_acceptance", "validate_finalization"]
