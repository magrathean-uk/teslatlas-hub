"""Tests for Teslatlas Hub devices and read-only sensors."""

from __future__ import annotations

import json
import math
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

import pytest
from aiohttp import web
from homeassistant.const import (
    ATTR_DEVICE_CLASS,
    ATTR_UNIT_OF_MEASUREMENT,
    CONF_HOST,
    STATE_UNAVAILABLE,
    STATE_UNKNOWN,
    UnitOfLength,
)
from homeassistant.core import HomeAssistant
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers.entity_registry import RegistryEntryDisabler
from homeassistant.helpers.update_coordinator import UpdateFailed
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_DEVICE_ID,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_TOKEN_EXPIRES_AT_MS,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.models import HubSnapshot, VehicleState
from custom_components.teslatlas_hub.sensor import (
    HUB_SENSOR_DESCRIPTIONS,
    VEHICLE_SENSOR_DESCRIPTIONS,
)
from tests.helpers import FIXTURE_ACCESS_TOKEN, FixtureHubClient, vehicle_update

HTTP_HUB_ID = "11111111-1111-4111-8111-111111111111"
HTTP_ALPHA_ID = "22222222-2222-4222-8222-222222222222"
HTTP_BETA_ID = "33333333-3333-4333-8333-333333333333"


async def _setup_http(hass: HomeAssistant, aiohttp_server):
    """Exercise normal setup, real client decoding and actual HA publication."""
    example = (
        Path(__file__).parents[1]
        / "custom_components/teslatlas_hub/profile/hub-http-v1/1.0.0/examples"
        / "current.json"
    )
    template = json.loads(example.read_text())
    current = {
        HTTP_ALPHA_ID: template
        | {"vehicle_id": HTTP_ALPHA_ID, "display_name": "HTTP Alpha", "odometer": 42},
        HTTP_BETA_ID: template
        | {
            "vehicle_id": HTTP_BETA_ID,
            "display_name": "HTTP Beta",
            "battery_level": 40,
        },
    }

    async def discovery(_request):
        return web.json_response(
            {
                "hub_id": HTTP_HUB_ID,
                "protocol": "teslatlas-sync",
                "protocol_major": 1,
                "api_versions": ["1.0"],
                "capabilities": ["query.vehicles", "query.current"],
            }
        )

    async def vehicles(_request):
        return web.json_response(
            {
                "vehicles": [
                    {"vehicle_id": key, "display_name": value["display_name"]}
                    for key, value in current.items()
                ]
            }
        )

    async def projection(request):
        return web.json_response(current[request.match_info["vehicle"]])

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    app.router.add_get("/v1/vehicles/{vehicle}/current", projection)
    server = await aiohttp_server(app)
    entry = MockConfigEntry(
        domain=DOMAIN,
        unique_id=HTTP_HUB_ID,
        version=1,
        minor_version=2,
        data={
            CONF_HOST: server.host,
            CONF_PORT: server.port,
            CONF_USE_TLS: False,
            CONF_HUB_ID: HTTP_HUB_ID,
            CONF_ACCESS_TOKEN: "a" * 64,
            CONF_DEVICE_ID: "44444444-4444-4444-8444-444444444444",
            CONF_TOKEN_EXPIRES_AT_MS: 2**63 - 1,
        },
    )
    entry.add_to_hass(hass)
    assert await hass.config_entries.async_setup(entry.entry_id)
    await hass.async_block_till_done()
    return entry, current


async def test_repeated_polls_refresh_source_name_preserving_customizations(
    hass: HomeAssistant, aiohttp_server, socket_enabled
) -> None:
    """Follow two source renames without replacing IDs or user-provided names."""
    entry, current = await _setup_http(hass, aiohttp_server)
    devices = dr.async_get(hass)
    alpha = _device_by_identifier(
        hass, (DOMAIN, f"{HTTP_HUB_ID}:{HTTP_ALPHA_ID}"), entry.entry_id
    )
    beta = _device_by_identifier(
        hass, (DOMAIN, f"{HTTP_HUB_ID}:{HTTP_BETA_ID}"), entry.entry_id
    )
    assert alpha is not None and alpha.name == "HTTP Alpha"
    assert beta is not None and beta.name == "HTTP Beta"
    devices.async_update_device(alpha.id, name_by_user="Garage Tesla")
    registry = er.async_get(hass)
    charge_id = _entity_id(hass, f"{HTTP_HUB_ID}_{HTTP_ALPHA_ID}_state_of_charge")
    registry.async_update_entity(charge_id, name="My battery")
    await hass.async_block_till_done()
    original_entities = {
        item.unique_id: (item.id, item.entity_id, item.device_id, item.name)
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    }
    original_devices = {
        item.id for item in dr.async_entries_for_config_entry(devices, entry.entry_id)
    }

    for source_name in ("HTTP Renamed", "HTTP Renamed Again", "HTTP Renamed Again"):
        current[HTTP_ALPHA_ID]["display_name"] = source_name
        await entry.runtime_data.async_refresh()
        await hass.async_block_till_done()
        renamed = _device_by_identifier(
            hass, (DOMAIN, f"{HTTP_HUB_ID}:{HTTP_ALPHA_ID}"), entry.entry_id
        )
        assert renamed is not None
        assert renamed.id == alpha.id
        assert renamed.identifiers == alpha.identifiers
        assert renamed.name == source_name
        assert renamed.name_by_user == "Garage Tesla"
        assert devices.async_get(beta.id).name == "HTTP Beta"
        assert {
            item.unique_id: (item.id, item.entity_id, item.device_id, item.name)
            for item in er.async_entries_for_config_entry(registry, entry.entry_id)
        } == original_entities
        assert {
            item.id
            for item in dr.async_entries_for_config_entry(devices, entry.entry_id)
        } == original_devices

    assert await hass.config_entries.async_unload(entry.entry_id)


@pytest.mark.parametrize(
    ("unit", "factor", "extreme", "expected", "control"),
    [
        (UnitOfLength.KILOMETERS, 1, 10**400, STATE_UNKNOWN, 1e300),
        (UnitOfLength.METERS, 1000, 1e308, STATE_UNKNOWN, 1e305),
        (UnitOfLength.MILLIMETERS, 1000000, 1e308, STATE_UNKNOWN, 1e302),
        (UnitOfLength.KILOMETERS, 1, 1e308, "1e+308", 1e300),
    ],
)
async def test_repeated_numeric_polls_publish_controlled_state_and_recover(
    hass: HomeAssistant,
    aiohttp_server,
    socket_enabled,
    caplog,
    unit,
    factor,
    extreme,
    expected,
    control,
) -> None:
    """Never retain old success or publish infinity; siblings and recovery work."""
    entry, current = await _setup_http(hass, aiohttp_server)
    odometer_id = _entity_id(hass, f"{HTTP_HUB_ID}_{HTTP_ALPHA_ID}_odometer")
    sibling_id = _entity_id(hass, f"{HTTP_HUB_ID}_{HTTP_BETA_ID}_state_of_charge")
    registry = er.async_get(hass)
    registry.async_update_entity_options(
        odometer_id, "sensor", {"unit_of_measurement": unit}
    )
    await hass.async_block_till_done()
    assert float(hass.states.get(odometer_id).state) == 42 * factor
    original_entities = {
        item.unique_id: (item.id, item.entity_id)
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    }

    for sibling_value in (41, 42):
        current[HTTP_ALPHA_ID]["odometer"] = extreme
        current[HTTP_BETA_ID]["battery_level"] = sibling_value
        await entry.runtime_data.async_refresh()
        await hass.async_block_till_done()
        assert entry.runtime_data.last_update_success
        assert hass.states.get(odometer_id).state == expected
        assert hass.states.get(sibling_id).state == str(sibling_value)

    for value, expected_native in ((control, control), (43, 43), (0, 0), (None, None)):
        current[HTTP_ALPHA_ID]["odometer"] = value
        await entry.runtime_data.async_refresh()
        await hass.async_block_till_done()
        assert entry.runtime_data.last_update_success
        vehicle = entry.runtime_data.data.vehicles[HTTP_ALPHA_ID]
        assert vehicle.odometer_km == expected_native
        state = hass.states.get(odometer_id)
        assert state.attributes[ATTR_UNIT_OF_MEASUREMENT] == unit
        if value is None:
            assert state.state == STATE_UNKNOWN
        else:
            published = float(state.state)
            assert math.isfinite(published)
            assert published == pytest.approx(value * factor)

    current[HTTP_ALPHA_ID]["odometer"] = 44
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert float(hass.states.get(odometer_id).state) == 44 * factor
    assert {
        item.unique_id: (item.id, item.entity_id)
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    } == original_entities
    assert "OverflowError" not in caplog.text
    assert "Exception in callback" not in caplog.text
    assert await hass.config_entries.async_unload(entry.entry_id)


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
            CONF_ACCESS_TOKEN: FIXTURE_ACCESS_TOKEN,
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


def _device_by_identifier(
    hass: HomeAssistant,
    identifier: tuple[str, str],
    config_entry_id: str,
):
    device_registry = dr.async_get(hass)
    get_device_by_identifier = getattr(
        device_registry, "async_get_device_by_identifier", None
    )
    if get_device_by_identifier is not None:
        return get_device_by_identifier(identifier, config_entry_id)
    return device_registry.async_get_device(identifiers={identifier})


async def test_setup_creates_hub_and_vehicle_devices_with_stable_entities(
    hass: HomeAssistant,
    caplog,
) -> None:
    """Catch unstable IDs, merged vehicles, or missing device ownership."""
    client = FixtureHubClient()
    entry = await _setup(hass, client)

    alpha_device = _device_by_identifier(
        hass, (DOMAIN, "hub-fixture:vehicle-alpha"), entry.entry_id
    )
    beta_device = _device_by_identifier(
        hass, (DOMAIN, "hub-fixture:vehicle-beta"), entry.entry_id
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


async def test_current_failure_is_unavailable_but_missing_observation_is_unknown(
    hass: HomeAssistant,
) -> None:
    client = FixtureHubClient()
    entry = await _setup(hass, client)
    coordinator = entry.runtime_data
    before = coordinator.data
    alpha_id = _entity_id(hass, "hub-fixture_vehicle-alpha_state_of_charge")
    beta_id = _entity_id(hass, "hub-fixture_vehicle-beta_state_of_charge")
    registry = er.async_get(hass)
    original_ids = {
        item.unique_id
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    }

    failed = VehicleState(
        vehicle_id="vehicle-alpha", name="Fixture Alpha", current_read_failed=True
    )
    healthy = replace(before.vehicles["vehicle-beta"], state_of_charge=0)
    coordinator.async_set_updated_data(
        HubSnapshot.create(
            info=before.info,
            status=before.status,
            vehicles=[failed, healthy],
            received_at=before.received_at,
        )
    )
    await hass.async_block_till_done()
    assert hass.states.get(alpha_id).state == STATE_UNAVAILABLE
    assert hass.states.get(beta_id).state == "0"

    coordinator.async_set_updated_data(
        coordinator.data.with_vehicle(
            replace(failed, current_read_failed=False), before.received_at
        )
    )
    await hass.async_block_till_done()
    assert hass.states.get(alpha_id).state == STATE_UNKNOWN
    assert hass.states.get(beta_id).state == "0"

    coordinator.async_set_updated_data(before)
    await hass.async_block_till_done()
    assert hass.states.get(alpha_id).state == "72.5"
    assert {
        item.unique_id
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
    } == original_ids
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

    gamma_device = _device_by_identifier(
        hass, (DOMAIN, "hub-fixture:vehicle-gamma"), entry.entry_id
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


async def test_dynamic_vehicle_survives_restart_while_absent_without_duplicates(
    hass: HomeAssistant,
) -> None:
    """Retain exact device and entity identities through an absent restart."""
    dynamic_vehicle_id = "66666666-6666-4666-8666-666666666666"
    first_client = FixtureHubClient()
    entry = await _setup(hass, first_client)
    base_snapshot = entry.runtime_data.data
    dynamic_vehicle = replace(
        vehicle_update().vehicle,
        vehicle_id=dynamic_vehicle_id,
        name="Fixture Gamma",
    )
    with_dynamic = HubSnapshot.create(
        info=base_snapshot.info,
        status=base_snapshot.status,
        vehicles=[*base_snapshot.vehicles.values(), dynamic_vehicle],
        received_at=base_snapshot.received_at,
    )
    without_dynamic = HubSnapshot.create(
        info=base_snapshot.info,
        status=base_snapshot.status,
        vehicles=base_snapshot.vehicles.values(),
        received_at=base_snapshot.received_at,
    )

    entry.runtime_data.async_set_updated_data(with_dynamic)
    await hass.async_block_till_done()

    entity_registry = er.async_get(hass)
    device_identifier = (DOMAIN, f"hub-fixture:{dynamic_vehicle_id}")
    dynamic_device = _device_by_identifier(hass, device_identifier, entry.entry_id)
    assert dynamic_device is not None
    original_device_registry_id = dynamic_device.id
    original_entities = {
        item.unique_id: (item.id, item.entity_id)
        for item in er.async_entries_for_config_entry(entity_registry, entry.entry_id)
        if item.unique_id.startswith(f"hub-fixture_{dynamic_vehicle_id}_")
    }
    assert len(original_entities) == len(VEHICLE_SENSOR_DESCRIPTIONS)

    entry.runtime_data.async_set_updated_data(without_dynamic)
    await hass.async_block_till_done()
    assert all(
        hass.states.get(item[1]).state == STATE_UNAVAILABLE
        for item in original_entities.values()
    )

    assert await hass.config_entries.async_unload(entry.entry_id) is True
    restart_client = FixtureHubClient()
    restart_client.snapshot = without_dynamic
    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=restart_client,
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True
    await hass.async_block_till_done()

    restarted_device = _device_by_identifier(hass, device_identifier, entry.entry_id)
    assert restarted_device is not None
    assert restarted_device.id == original_device_registry_id
    restarted_entities = {
        item.unique_id: (item.id, item.entity_id)
        for item in er.async_entries_for_config_entry(entity_registry, entry.entry_id)
        if item.unique_id.startswith(f"hub-fixture_{dynamic_vehicle_id}_")
    }
    assert restarted_entities == original_entities
    assert all(
        hass.states.get(item[1]).state == STATE_UNAVAILABLE
        for item in restarted_entities.values()
    )

    entry.runtime_data.async_set_updated_data(with_dynamic)
    await hass.async_block_till_done()
    returned_device = _device_by_identifier(hass, device_identifier, entry.entry_id)
    assert returned_device is not None
    assert returned_device.id == original_device_registry_id
    returned_entities = {
        item.unique_id: (item.id, item.entity_id)
        for item in er.async_entries_for_config_entry(entity_registry, entry.entry_id)
        if item.unique_id.startswith(f"hub-fixture_{dynamic_vehicle_id}_")
    }
    assert returned_entities == original_entities
    assert all(
        hass.states.get(item[1]).state != STATE_UNAVAILABLE
        for item in returned_entities.values()
    )

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
