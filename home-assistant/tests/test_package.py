"""Validate the custom integration package metadata."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).parents[1]
INTEGRATION = ROOT / "custom_components" / "teslatlas_hub"


def _load_json(path: Path) -> dict[str, object]:
    return json.loads(path.read_text(encoding="utf-8"))


def test_manifest_declares_local_poll_hub() -> None:
    """Catch packaging that no longer advertises the approved HA contract."""
    manifest = _load_json(INTEGRATION / "manifest.json")

    assert manifest["domain"] == "teslatlas_hub"
    assert manifest["name"] == "Teslatlas Hub"
    assert manifest["version"] == "2026.36.2"
    assert manifest["integration_type"] == "hub"
    assert manifest["iot_class"] == "local_poll"
    assert manifest["config_flow"] is True
    assert "zeroconf" not in manifest
    assert manifest["requirements"] == []


def test_hacs_metadata_targets_one_integration() -> None:
    """Catch HACS metadata that points at another domain or adds sidecars."""
    hacs = _load_json(ROOT / "hacs.json")

    assert hacs == {
        "name": "Teslatlas Hub",
        "homeassistant": "2026.8.3",
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


def test_embedded_current_profile_matches_approved_bundle() -> None:
    """Catch stale or partially copied public contract resources."""
    profile = INTEGRATION / "profile" / "hub-http-v1" / "1.0.0"
    sums = profile / "SHA256SUMS"
    assert hashlib.sha256(sums.read_bytes()).hexdigest() == (
        "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
    )
    for line in sums.read_text(encoding="utf-8").splitlines():
        expected, relative = line.split("  ", maxsplit=1)
        assert hashlib.sha256((profile / relative).read_bytes()).hexdigest() == expected
