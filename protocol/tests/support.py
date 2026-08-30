from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from referencing import Registry, Resource


ROOT = Path(__file__).resolve().parents[1]


def reject_constant(value: str) -> None:
    raise ValueError(f"non-finite JSON constant: {value}")


def strict_loads(value: str) -> Any:
    return json.loads(value, parse_constant=reject_constant)


def load_json(relative_path: str) -> Any:
    path = ROOT / relative_path
    if not path.is_file():
        raise AssertionError(f"required contract artifact is missing: {relative_path}")
    return strict_loads(path.read_text(encoding="utf-8"))


def load_schema_registry() -> tuple[dict[str, Any], Registry]:
    schemas: dict[str, Any] = {}
    registry = Registry()
    schema_dir = ROOT / "schemas"
    if not schema_dir.is_dir():
        raise AssertionError("required contract artifact is missing: schemas/")

    for path in sorted(schema_dir.glob("*.schema.json")):
        schema = strict_loads(path.read_text(encoding="utf-8"))
        schema_id = schema.get("$id")
        if not isinstance(schema_id, str):
            raise AssertionError(f"schema has no $id: {path.relative_to(ROOT)}")
        schemas[schema_id] = schema
        registry = registry.with_resource(schema_id, Resource.from_contents(schema))
    return schemas, registry
