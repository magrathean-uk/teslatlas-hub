"""Shared entity support for Tesatlas Hub."""

from __future__ import annotations

from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .coordinator import TesatlasDataCoordinator


class TesatlasCoordinatorEntity(CoordinatorEntity[TesatlasDataCoordinator]):
    """Base for translated, push-updated Tesatlas entities."""

    _attr_has_entity_name = True
