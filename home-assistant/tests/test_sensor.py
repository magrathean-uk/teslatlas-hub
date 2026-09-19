"""Tests for Teslatlas Hub devices and read-only sensors."""

from __future__ import annotations

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
from homeassistant.helpers.entity_registry import RegistryEntryDisabler
from homeassistant.helpers.update_coordinator import UpdateFailed
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.models import HubSnapshot
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

    alpha_device = dr.async_get(hass).async_get_device(
        identifiers={(DOMAIN, "hub-fixture:vehicle-alpha")}
    )
    beta_device = dr.async_get(hass).async_get_device(
        identifiers={(DOMAIN, "hub-fixture:vehicle-beta")}
    )
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

    assert len(HUB_SENSOR_DESCRIPTIONS) == 0
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


async def test_poll_adds_new_vehicle_entities_once(
    hass: HomeAssistant,
) -> None:
    """Catch dropped or duplicated entities when a vehicle appears on poll."""
    client = FixtureHubClient()
    base = vehicle_update()
    gamma = replace(
        base.vehicle,
        vehicle_id="vehicle-gamma",
        name="Fixture Gamma",
    )
    entry = await _setup(hass, client)
    snapshot = entry.runtime_data.data
    entry.runtime_data.async_set_updated_data(
        HubSnapshot.create(
            info=snapshot.info,
            status=snapshot.status,
            vehicles=[*snapshot.vehicles.values(), gamma],
            received_at=base.received_at,
        )
    )
    await hass.async_block_till_done()

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
    assert len(HUB_SENSOR_DESCRIPTIONS) == 0

    assert await hass.config_entries.async_unload(entry.entry_id) is True


async def test_removed_vehicle_keeps_registry_ids_and_recovers(
    hass: HomeAssistant,
) -> None:
    """Keep matching IDs while a missing vehicle is unavailable."""
    client = FixtureHubClient()
    entry = await _setup(hass, client)
    beta_charge_id = _entity_id(
        hass,
        "hub-fixture_vehicle-beta_state_of_charge",
    )
    registry = er.async_get(hass)
    original_ids = {
        item.unique_id
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    }

    snapshot = entry.runtime_data.data
    without_beta = HubSnapshot.create(
        info=snapshot.info,
        status=snapshot.status,
        vehicles=[snapshot.vehicles["vehicle-alpha"]],
        received_at=snapshot.received_at,
    )
    entry.runtime_data.async_set_updated_data(without_beta)
    await hass.async_block_till_done()

    beta_state = hass.states.get(beta_charge_id)
    assert beta_state is not None
    assert beta_state.state == STATE_UNAVAILABLE
    assert {
        item.unique_id
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    } == original_ids

    entry.runtime_data.async_set_updated_data(snapshot)
    await hass.async_block_till_done()
    beta_state = hass.states.get(beta_charge_id)
    assert beta_state is not None
    assert beta_state.state == "44.0"
    assert {
        item.unique_id
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    } == original_ids

    assert await hass.config_entries.async_unload(entry.entry_id) is True


async def test_retired_registry_entries_are_scoped_to_their_config_entry(
    hass: HomeAssistant,
) -> None:
    """Remove only historical sensor meanings owned by this entry."""
    entry = _entry(hass)
    other_entry = MockConfigEntry(
        domain=DOMAIN,
        title="Other Hub",
        unique_id="other-hub",
        data={
            CONF_HOST: "other-hub.local",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "other-hub",
            CONF_ACCESS_TOKEN: "other-bearer",
        },
    )
    other_entry.add_to_hass(hass)
    registry = er.async_get(hass)
    for unique_id in (
        "hub-fixture_hub_collector_health",
        "hub-fixture_hub_fleet_cost",
        "hub-fixture_hub_backup_age",
        "hub-fixture_vehicle-alpha_data_quality",
        "hub-fixture_vehicle-alpha_state_of_charge",
    ):
        registry.async_get_or_create(
            "sensor",
            DOMAIN,
            unique_id,
            config_entry=entry,
        )
    other_retired = registry.async_get_or_create(
        "sensor",
        DOMAIN,
        "other-hub_hub_collector_health",
        config_entry=other_entry,
    )

    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=FixtureHubClient(),
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True

    current_ids = {
        item.unique_id
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    }
    assert current_ids == {
        "hub-fixture_vehicle-alpha_state_of_charge",
        *{
            f"hub-fixture_{vehicle_id}_{description.key}"
            for vehicle_id in ("vehicle-alpha", "vehicle-beta")
            for description in VEHICLE_SENSOR_DESCRIPTIONS
        },
    }
    assert registry.async_get(other_retired.entity_id) is not None

    assert await hass.config_entries.async_unload(entry.entry_id) is True


async def test_existing_registry_customizations_survive_component_reload(
    hass: HomeAssistant,
) -> None:
    """Keep a user's entity ID, name, and disabled state across replacement."""
    entry = _entry(hass)
    registry = er.async_get(hass)
    existing = registry.async_get_or_create(
        "sensor",
        DOMAIN,
        "hub-fixture_vehicle-alpha_state_of_charge",
        config_entry=entry,
    )
    customized = registry.async_update_entity(
        existing.entity_id,
        new_entity_id="sensor.garage_alpha_battery",
        name="Garage Alpha battery",
        disabled_by=RegistryEntryDisabler.USER,
    )

    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=FixtureHubClient(),
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True

    retained = registry.async_get(customized.entity_id)
    assert retained is not None
    assert retained.unique_id == existing.unique_id
    assert retained.name == "Garage Alpha battery"
    assert retained.disabled_by is RegistryEntryDisabler.USER

    assert await hass.config_entries.async_unload(entry.entry_id) is True
