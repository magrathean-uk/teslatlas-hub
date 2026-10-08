from __future__ import annotations

import copy
import json
import sys
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]


def replace_profile(value: Any, profile: str) -> Any:
    if value == "${profile}":
        return profile
    if isinstance(value, list):
        return [replace_profile(item, profile) for item in value]
    if isinstance(value, dict):
        return {key: replace_profile(item, profile) for key, item in value.items()}
    return value


cases = {
    case["case_id"]: case
    for filepath in sorted((ROOT / "conformance" / "cases").glob("*.json"))
    for case in [json.loads(filepath.read_text(encoding="utf-8"))]
}
active_case: tuple[str, str] | None = None

for line in sys.stdin:
    envelope = json.loads(line)
    key = (envelope["profile"], envelope["case_id"])
    if active_case is None:
        active_case = key
    if key != active_case:
        response = {"status": 599, "headers": {}}
    else:
        case = cases[envelope["case_id"]]
        step = next(item for item in case["steps"] if item["step_id"] == envelope["step_id"])
        response = copy.deepcopy(step["reference_response"])
        body_file = response.pop("body_file", None)
        if body_file is not None:
            response["body"] = json.loads((ROOT / body_file).read_text(encoding="utf-8"))
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
