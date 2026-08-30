"""Tests for Teslatlas Hub devices and read-only sensors."""

from __future__ import annotations

import asyncio
from dataclasses import replace
from unittest.mock import patch

from homeassistant.const import (
    ATTR_DEVICE_CLASS,
    ATTR_UNIT_OF_MEASUREMENT,
    CONF_HOST,
    STATE_UNAVAILABLE,
    STATE_UNKNOWN,
)
from homeassistant.core import HomeAssistant
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers.update_coordinator import UpdateFailed
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.models import HubEvent
from custom_components.teslatlas_hub.sensor import (
    HUB_SENSOR_DESCRIPTIONS,
    VEHICLE_SENSOR_DESCRIPTIONS,
)
from tests.helpers import FixtureHubClient, vehicle_update


def _entry(hass: HomeAssistant) -> MockConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id="hub-fixture",
        data={
            CONF_HOST: "hub-fixture.local",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "fixture-device-bearer",
        },
    )
    entry.add_to_hass(hass)
    return entry


async def _setup(
    hass: HomeAssistant,
    client: FixtureHubClient,
) -> MockConfigEntry:
    entry = _entry(hass)
    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=client,
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True
    return entry


def _entity_id(hass: HomeAssistant, unique_id: str) -> str:
    entity_id = er.async_get(hass).async_get_entity_id("sensor", DOMAIN, unique_id)
    assert entity_id is not None
    return entity_id


async def test_setup_creates_hub_and_vehicle_devices_with_stable_entities(
    hass: HomeAssistant,
    caplog,
) -> None:
    """Catch unstable IDs, merged vehicles, or missing device ownership."""
    client = FixtureHubClient()
    entry = await _setup(hass, client)

    hub_device = dr.async_get(hass).async_get_device(
        identifiers={(DOMAIN, "hub-fixture")}
    )
    alpha_device = dr.async_get(hass).async_get_device(
        identifiers={(DOMAIN, "hub-fixture:vehicle-alpha")}
    )
    beta_device = dr.async_get(hass).async_get_device(
        identifiers={(DOMAIN, "hub-fixture:vehicle-beta")}
    )
    assert hub_device is not None
    assert hub_device.name == "Fixture Hub"
    assert alpha_device is not None
    assert alpha_device.name == "Fixture Alpha"
    assert beta_device is not None
    assert beta_device.name == "Fixture Beta"

    charge_id = _entity_id(
        hass,
        "hub-fixture_vehicle-alpha_state_of_charge",
    )
    charge_state = hass.states.get(charge_id)
    assert charge_state is not None
    assert charge_state.state == "72.5"
    assert charge_state.attributes[ATTR_DEVICE_CLASS] == "battery"
    assert charge_state.attributes[ATTR_UNIT_OF_MEASUREMENT] == "%"

    inside_id = _entity_id(
        hass,
        "hub-fixture_vehicle-alpha_inside_temperature",
    )
    inside_state = hass.states.get(inside_id)
    assert inside_state is not None
    assert inside_state.state == STATE_UNKNOWN

    collector_id = _entity_id(
        hass,
        "hub-fixture_hub_collector_health",
    )
    collector_state = hass.states.get(collector_id)
    assert collector_state is not None
    assert collector_state.state == "healthy"
    assert hass.services.async_services().get(DOMAIN) is None
    assert "impossible considering device class" not in caplog.text

    assert await hass.config_entries.async_unload(entry.entry_id) is True


async def test_disconnect_marks_all_entities_unavailable(
    hass: HomeAssistant,
) -> None:
    """Catch stale entity availability after the public stream is lost."""
    client = FixtureHubClient()
    entry = await _setup(hass, client)
    coordinator = entry.runtime_data
    charge_id = _entity_id(
        hass,
        "hub-fixture_vehicle-alpha_state_of_charge",
    )

    coordinator.async_set_update_error(UpdateFailed("offline"))
    await hass.async_block_till_done()

    charge_state = hass.states.get(charge_id)
    assert charge_state is not None
    assert charge_state.state == STATE_UNAVAILABLE

    assert await hass.config_entries.async_unload(entry.entry_id) is True


async def test_push_adds_new_vehicle_entities_once(
    hass: HomeAssistant,
) -> None:
    """Catch dropped or duplicated entities when a vehicle appears by push."""
    client = FixtureHubClient()
    base = vehicle_update()
    gamma = HubEvent(
        event_id="fixture-event-gamma",
        vehicle=replace(
            base.vehicle,
            vehicle_id="vehicle-gamma",
            name="Fixture Gamma",
        ),
        received_at=base.received_at,
    )
    client.event_connections = [[gamma, gamma]]
    entry = await _setup(hass, client)
    await asyncio.wait_for(client.stream_blocked.wait(), timeout=1)

    gamma_device = dr.async_get(hass).async_get_device(
        identifiers={(DOMAIN, "hub-fixture:vehicle-gamma")}
    )
    assert gamma_device is not None
    assert gamma_device.name == "Fixture Gamma"
    gamma_entries = [
        entity
        for entity in er.async_get(hass).entities.values()
        if entity.unique_id.startswith("hub-fixture_vehicle-gamma_")
    ]
    assert len(gamma_entries) == len(VEHICLE_SENSOR_DESCRIPTIONS)
    assert len(HUB_SENSOR_DESCRIPTIONS) == 3

    assert await hass.config_entries.async_unload(entry.entry_id) is True
