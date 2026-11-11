"""Shared entity support for Teslatlas Hub."""

from __future__ import annotations

from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .coordinator import TeslatlasDataCoordinator


class TeslatlasCoordinatorEntity(CoordinatorEntity[TeslatlasDataCoordinator]):
    """Base for translated, push-updated Teslatlas entities."""

    _attr_has_entity_name = True
