from __future__ import annotations

import copy
import json
import sys
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]


def reject_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON constant: {value}")


def strict_loads(value: str) -> Any:
    return json.loads(value, parse_constant=reject_constant)


def safe_body_path(value: str) -> Path:
    candidate = (ROOT / value).resolve(strict=True)
    if not candidate.is_file():
        raise ValueError(f"body_file is not a regular file: {value}")
    permitted = [(ROOT / name).resolve() for name in ("examples", "fixtures")]
    if not any(candidate.is_relative_to(directory) for directory in permitted):
        raise ValueError(f"body_file escapes public fixture roots: {value}")
    return candidate


def load_cases() -> dict[str, dict[str, Any]]:
    return {
        case["case_id"]: case
        for path in sorted((ROOT / "conformance" / "cases").glob("*.json"))
        for case in [strict_loads(path.read_text(encoding="utf-8"))]
    }


def replace_profile(value: Any, profile: str) -> Any:
    if value == "${profile}":
        return profile
    if isinstance(value, list):
        return [replace_profile(item, profile) for item in value]
    if isinstance(value, dict):
        return {key: replace_profile(item, profile) for key, item in value.items()}
    return value


CASES = load_cases()

for line in sys.stdin:
    envelope = strict_loads(line)
    case = CASES[envelope["case_id"]]
    step = next(item for item in case["steps"] if item["step_id"] == envelope["step_id"])
    response = copy.deepcopy(step["reference_response"])
    body_file = response.pop("body_file", None)
    if body_file is not None:
        response["body"] = strict_loads(
            safe_body_path(body_file).read_text(encoding="utf-8")
        )
    response = replace_profile(response, envelope["profile"])
    print(
        json.dumps(
            {
                "case_id": envelope["case_id"],
                "step_id": envelope["step_id"],
                "response": response,
            },
            separators=(",", ":"),
        ),
        flush=True,
    )
