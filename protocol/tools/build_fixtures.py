#!/usr/bin/env python3
from __future__ import annotations

import argparse
import copy
import hashlib
import json
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "fixtures" / "v1"
MANIFEST_PATH = ROOT / "fixtures" / "manifest.json"
VEHICLE_ID = "vehicle_demo_alpha"
PROJECTION_VERSION = "3.1.0"


def canonical_bytes(value: Any) -> bytes:
    return (
        json.dumps(
            value, indent=2, sort_keys=True, ensure_ascii=False, allow_nan=False
        )
        + "\n"
    ).encode("utf-8")


def source_sequence_key(value: int | str | None) -> tuple[int, int | bytes]:
    if type(value) is int:
        return (0, value)
    if isinstance(value, str):
        return (1, value.encode("utf-8"))
    if value is None:
        return (2, b"")
    raise TypeError(f"unsupported source sequence: {value!r}")


def canonical_observation_key(value: dict[str, Any]) -> tuple[Any, ...]:
    return (
        value["provider_timestamp"],
        value["source"].lower().encode("ascii"),
        source_sequence_key(value["source_sequence"]),
        value["observation_id"].lower().encode("ascii"),
    )


def observation(
    observation_id: str,
    provider_timestamp: str,
    received_timestamp: str,
    source_sequence: int | str | None,
    field: str,
    value: float,
    unit: str,
    quality_flags: list[str] | None = None,
    *,
    source: str = "fleet_telemetry",
) -> dict[str, Any]:
    redacted_source_value = {
        "field": field,
        "provider_timestamp": provider_timestamp,
        "source_sequence": source_sequence,
        "source": source,
        "value": value,
    }
    return {
        "observation_id": observation_id,
        "vehicle_id": VEHICLE_ID,
        "source": source,
        "provider_timestamp": provider_timestamp,
        "received_timestamp": received_timestamp,
        "source_sequence": source_sequence,
        "field": field,
        "typed_value": {"kind": "number", "value": value, "unit": unit},
        "validity": "valid",
        "quality_flags": quality_flags or [],
        "raw_payload_hash": hashlib.sha256(
            json.dumps(redacted_source_value, sort_keys=True, separators=(",", ":")).encode(
                "utf-8"
            )
        ).hexdigest(),
        "collector_version": "5.4.0",
        "projection_version": PROJECTION_VERSION,
    }


def assessment(
    subject_type: str,
    subject_id: str,
    quality: str,
    assessed_at: str,
    *,
    gap_count: int = 0,
    largest_gap_seconds: int = 0,
    issues: list[dict[str, Any]] | None = None,
    sources: list[str] | None = None,
) -> dict[str, Any]:
    return {
        "subject_type": subject_type,
        "subject_id": subject_id,
        "quality": quality,
        "sources": sources or ["fleet_telemetry"],
        "gap_count": gap_count,
        "largest_gap_seconds": largest_gap_seconds,
        "derived_fields": [],
        "projection_version": PROJECTION_VERSION,
        "assessed_at": assessed_at,
        "issues": issues or [],
    }


def build_fixture(
    scenario_id: str,
    description: str,
    input_observations: list[dict[str, Any]],
    accepted_ids: list[str],
    duplicate_ids: list[str],
    quality_name: str,
    *,
    gap_count: int = 0,
    largest_gap_seconds: int = 0,
    issues: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    by_id = {item["observation_id"]: item for item in input_observations}
    ordered_ids = sorted(accepted_ids, key=lambda item: canonical_observation_key(by_id[item]))
    accepted = [by_id[item] for item in ordered_ids]
    sources = sorted({item["source"] for item in accepted}, key=str.lower)
    computed_at = max(item["received_timestamp"] for item in accepted)
    observed_at = max(item["provider_timestamp"] for item in accepted)
    latest_by_field: dict[str, dict[str, Any]] = {}
    for item in accepted:
        latest_by_field[item["field"]] = item

    battery = latest_by_field.get("battery.level_percent")
    vehicle_range = latest_by_field.get("battery.range_km")
    projection_id = f"projection_{scenario_id}_current_0001"
    projection_quality = assessment(
        "projection",
        projection_id,
        quality_name,
        computed_at,
        gap_count=gap_count,
        largest_gap_seconds=largest_gap_seconds,
        issues=issues,
        sources=sources,
    )
    payload_quality = assessment(
        "vehicle",
        VEHICLE_ID,
        quality_name,
        computed_at,
        gap_count=gap_count,
        largest_gap_seconds=largest_gap_seconds,
        issues=issues,
        sources=sources,
    )
    projection = {
        "projection_id": projection_id,
        "projection_type": "current_state",
        "vehicle_id": VEHICLE_ID,
        "input_observation_ids": ordered_ids,
        "computed_at": computed_at,
        "projection_version": PROJECTION_VERSION,
        "quality": projection_quality,
        "payload": {
            "resource_type": "current_state",
            "vehicle_id": VEHICLE_ID,
            "observed_at": observed_at,
            "revision": 1,
            "state": "online",
            "battery_level_percent": (
                battery["typed_value"]["value"] if battery is not None else None
            ),
            "range_km": (
                vehicle_range["typed_value"]["value"] if vehicle_range is not None else None
            ),
            "odometer_km": None,
            "inside_temperature_c": None,
            "outside_temperature_c": None,
            "locked": None,
            "climate_on": None,
            "charging_state": "unknown",
            "location": None,
            "quality": payload_quality,
        },
    }
    return {
        "fixture_version": "1.0.0",
        "scenario_id": scenario_id,
        "description": description,
        "input_observations": input_observations,
        "expected": {
            "accepted_observation_ids": accepted_ids,
            "duplicate_observation_ids": duplicate_ids,
            "ordered_observation_ids": ordered_ids,
            "projection": projection,
            "quality": copy.deepcopy(projection_quality),
        },
        "redaction": {
            "contains_real_identifiers": False,
            "contains_secrets": False,
            "contains_precise_location": False,
            "maximum_coordinate_decimals": 2,
        },
    }


def scenarios() -> list[dict[str, Any]]:
    normal_battery = observation(
        "obs_normal_battery_0001",
        "2026-08-30T12:00:00.000Z",
        "2026-08-30T12:00:00.250Z",
        "demo-normal-0001",
        "battery.level_percent",
        78,
        "percent",
    )
    normal_range = observation(
        "obs_normal_range_0002",
        "2026-08-30T12:00:01.000Z",
        "2026-08-30T12:00:01.250Z",
        "demo-normal-0002",
        "battery.range_km",
        338.4,
        "km",
    )
    normal = build_fixture(
        "normal",
        "Two ordered redacted observations produce a complete current-state projection.",
        [normal_battery, normal_range],
        [normal_battery["observation_id"], normal_range["observation_id"]],
        [],
        "complete",
    )

    duplicate_observation = observation(
        "obs_duplicate_battery_0001",
        "2026-08-30T13:00:00.000Z",
        "2026-08-30T13:00:00.250Z",
        "demo-duplicate-0001",
        "battery.level_percent",
        77,
        "percent",
    )
    duplicate = build_fixture(
        "duplicate",
        "A byte-identical observation ID is admitted once and reported as an idempotent duplicate.",
        [duplicate_observation, copy.deepcopy(duplicate_observation)],
        [duplicate_observation["observation_id"]],
        [duplicate_observation["observation_id"]],
        "complete",
    )

    delayed_observation = observation(
        "obs_delayed_range_0001",
        "2026-08-30T14:00:00.000Z",
        "2026-08-30T14:10:00.000Z",
        "demo-delayed-0001",
        "battery.range_km",
        331.2,
        "km",
        ["delayed"],
    )
    delayed_issue = {
        "code": "late_observation",
        "severity": "warning",
        "message": "The redacted observation arrived ten minutes after provider time.",
        "from": "2026-08-30T14:00:00.000Z",
        "to": "2026-08-30T14:10:00.000Z",
        "affected_fields": ["battery.range_km"],
    }
    delayed = build_fixture(
        "delayed",
        "A delayed observation remains visible and degrades the projection without inventing data.",
        [delayed_observation],
        [delayed_observation["observation_id"]],
        [],
        "partial",
        issues=[delayed_issue],
    )

    missing_before = observation(
        "obs_missing_battery_0001",
        "2026-08-30T15:00:00.000Z",
        "2026-08-30T15:00:00.250Z",
        "demo-missing-0001",
        "battery.level_percent",
        78,
        "percent",
        ["gap_after"],
    )
    missing_after = observation(
        "obs_missing_battery_0002",
        "2026-08-30T15:10:00.000Z",
        "2026-08-30T15:10:00.250Z",
        "demo-missing-0002",
        "battery.level_percent",
        76,
        "percent",
        ["gap_before"],
    )
    missing_issue = {
        "code": "missing_interval",
        "severity": "warning",
        "message": "A redacted ten-minute interval is unavailable and is not interpolated.",
        "from": "2026-08-30T15:00:00.000Z",
        "to": "2026-08-30T15:10:00.000Z",
        "affected_fields": ["battery.level_percent"],
    }
    missing = build_fixture(
        "missing",
        "A missing interval is explicit in quality metadata and no value is interpolated.",
        [missing_before, missing_after],
        [missing_before["observation_id"], missing_after["observation_id"]],
        [],
        "partial",
        gap_count=1,
        largest_gap_seconds=600,
        issues=[missing_issue],
    )

    reordered_source = observation(
        "obs_reordered_source_0001",
        "2026-08-30T16:00:01.000Z",
        "2026-08-30T16:00:06.250Z",
        99,
        "battery.level_percent",
        76,
        "percent",
        ["reordered"],
        source="fleet_api",
    )
    reordered_int_two = observation(
        "obs_reordered_int_0002",
        "2026-08-30T16:00:01.000Z",
        "2026-08-30T16:00:05.250Z",
        2,
        "battery.range_km",
        330.2,
        "km",
        ["reordered"],
    )
    reordered_int_ten = observation(
        "obs_reordered_int_0010",
        "2026-08-30T16:00:01.000Z",
        "2026-08-30T16:00:04.250Z",
        10,
        "vehicle.odometer_km",
        12001,
        "km",
        ["reordered"],
    )
    reordered_string = observation(
        "obs_reordered_string_0010",
        "2026-08-30T16:00:01.000Z",
        "2026-08-30T16:00:03.250Z",
        "10",
        "climate.inside_temperature_c",
        21,
        "celsius",
        ["reordered"],
    )
    reordered_null_b = observation(
        "obs_reordered_null_0002",
        "2026-08-30T16:00:01.000Z",
        "2026-08-30T16:00:02.250Z",
        None,
        "charging.power_kw",
        0,
        "kw",
        ["reordered"],
    )
    reordered_null_a = observation(
        "obs_reordered_null_0001",
        "2026-08-30T16:00:01.000Z",
        "2026-08-30T16:00:01.250Z",
        None,
        "charging.energy_added_kwh",
        0,
        "kwh",
        ["reordered"],
    )
    reordered_issue = {
        "code": "reordered_observation",
        "severity": "info",
        "message": "Input arrival order differs from canonical provider order.",
        "affected_fields": ["battery.level_percent"],
    }
    reordered = build_fixture(
        "reordered",
        "A tied, out-of-order input set exercises source, typed sequence, and ID ordering.",
        [
            reordered_null_b,
            reordered_string,
            reordered_int_ten,
            reordered_source,
            reordered_null_a,
            reordered_int_two,
        ],
        [
            reordered_null_b["observation_id"],
            reordered_string["observation_id"],
            reordered_int_ten["observation_id"],
            reordered_source["observation_id"],
            reordered_null_a["observation_id"],
            reordered_int_two["observation_id"],
        ],
        [],
        "partial",
        issues=[reordered_issue],
    )

    return [normal, duplicate, delayed, missing, reordered]


def build_outputs() -> dict[Path, bytes]:
    outputs: dict[Path, bytes] = {}
    entries = []
    for fixture in scenarios():
        relative = Path("fixtures") / "v1" / f"{fixture['scenario_id']}.json"
        payload = canonical_bytes(fixture)
        outputs[ROOT / relative] = payload
        entries.append(
            {
                "scenario_id": fixture["scenario_id"],
                "path": relative.as_posix(),
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
        )
    manifest = {
        "fixture_set_version": "1.0.0",
        "schema": "urn:teslatlas:protocol:schema:fixture:1.2.0",
        "fixtures": entries,
    }
    outputs[MANIFEST_PATH] = canonical_bytes(manifest)
    return outputs


def check_outputs(outputs: dict[Path, bytes]) -> list[str]:
    errors = []
    for path, expected in outputs.items():
        if not path.is_file():
            errors.append(f"missing: {path.relative_to(ROOT)}")
        elif path.read_bytes() != expected:
            errors.append(f"out of date: {path.relative_to(ROOT)}")
    expected_fixture_paths = {path for path in outputs if path.parent == FIXTURE_DIR}
    actual_fixture_paths = set(FIXTURE_DIR.glob("*.json")) if FIXTURE_DIR.is_dir() else set()
    for unexpected in sorted(actual_fixture_paths - expected_fixture_paths):
        errors.append(f"unexpected: {unexpected.relative_to(ROOT)}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description="Build deterministic redacted fixtures")
    parser.add_argument("--check", action="store_true", help="fail if committed fixtures differ")
    args = parser.parse_args()
    outputs = build_outputs()
    if args.check:
        errors = check_outputs(outputs)
        if errors:
            print("\n".join(errors))
            return 1
        print(f"up to date: {len(outputs) - 1} fixtures and manifest")
        return 0
    for path, payload in outputs.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payload)
    print(f"wrote {len(outputs) - 1} fixtures and manifest")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
