"""Validate the custom integration package metadata."""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).parents[1]
INTEGRATION = ROOT / "custom_components" / "teslatlas_hub"


def _load_json(path: Path) -> dict[str, object]:
    return json.loads(path.read_text(encoding="utf-8"))


def test_manifest_declares_local_push_hub() -> None:
    """Catch packaging that no longer advertises the approved HA contract."""
    manifest = _load_json(INTEGRATION / "manifest.json")

    assert manifest["domain"] == "teslatlas_hub"
    assert manifest["name"] == "Teslatlas Hub"
    assert manifest["version"] == "0.1.0"
    assert manifest["integration_type"] == "hub"
    assert manifest["iot_class"] == "local_push"
    assert manifest["config_flow"] is True
    assert manifest["zeroconf"] == ["_teslatlas-hub._tcp.local."]
    assert manifest["requirements"] == []


def test_hacs_metadata_targets_one_integration() -> None:
    """Catch HACS metadata that points at another domain or adds sidecars."""
    hacs = _load_json(ROOT / "hacs.json")

    assert hacs == {
        "name": "Teslatlas Hub",
        "homeassistant": "2026.8.0",
    }


def test_package_has_no_command_or_service_surface() -> None:
    """Catch accidental command exposure in the read-only integration."""
    forbidden = {
        "button.py",
        "services.yaml",
        "services.json",
        "switch.py",
    }

    assert not forbidden.intersection(path.name for path in INTEGRATION.glob("*"))
