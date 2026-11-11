# Entity and Registry Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose only protocol `1.2.0` read-only vehicle state, preserve stable entity identities where meanings match, and migrate every temporary entity/device artifact without touching unrelated registry data.

**Architecture:** The protocol-aligned immutable models from the contract plan feed one shared vehicle entity base. Setup creates a fixed-name Hub parent device explicitly, sensors and binary sensors attach vehicle devices through that parent, and a narrowly filtered minor-version migration removes only obsolete integration-owned registry entries.

**Tech Stack:** Python 3.14.2+, Home Assistant 2026.8.3 entity/device registries, sensor and binary-sensor platforms, pytest-homeassistant-custom-component, Ruff.

**Spec:** `docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md`

## Global Constraints

- Start after all six tasks in `docs/superpowers/plans/2026-08-31-contract-security-foundation.md` pass.
- Consume `VehicleState.quality`; do not restore the temporary `data_quality` model field or any unsupported Hub status model.
- Keep domain `teslatlas_hub` and all semantically unchanged unique-ID suffixes.
- Add no commands, controls, services, polling, raw attributes, location entities, or Hub-level sensors.
- Treat `state`, quality, and observation age as data, never as availability policy.
- Preserve user entity IDs, user entity names, `name_by_user`, unrelated config entries/devices/entities, and the separate dirty local checkout.
- Execute Tasks 1-4 in order. Each task begins with the listed failing test and ends with a focused gate and commit.

---

### Task 1: Create the explicit Hub parent and shared vehicle entity base

**Files:**
- Modify: `custom_components/teslatlas_hub/const.py`
- Modify: `custom_components/teslatlas_hub/entity.py`
- Modify: `custom_components/teslatlas_hub/__init__.py`
- Modify: `tests/helpers.py`
- Modify: `tests/test_init.py`
- Verify: `tests/test_config_flow.py`

**Interfaces:**
- Consumes: `HUB_NAME = "Teslatlas Hub"` from the contract/security config task; produces `PLATFORMS = (Platform.BINARY_SENSOR, Platform.SENSOR)`.
- Produces: `async_ensure_hub_device(hass, entry, hub_id) -> DeviceEntry` using identifier `(DOMAIN, hub_id)`.
- Produces: `TeslatlasVehicleEntity(entry, vehicle_id, entity_key)` with stable unique ID, vehicle lookup, availability, and vehicle `DeviceInfo` using `via_device=(DOMAIN, hub_id)`.

- [ ] **Step 1: Add the exact shared setup helpers**

Add these imports and functions to `tests/helpers.py`. Keep the contract-plan
`FixtureHubClient`; do not create a second fake client.

```python
from unittest.mock import patch

from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_CREDENTIAL_GENERATION,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
    HUB_NAME,
)


def configured_entry(
    hass: HomeAssistant,
    *,
    title: str = HUB_NAME,
    hub_id: str = "hub-fixture",
    credential_generation: int = 1,
) -> MockConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        title=title,
        unique_id=hub_id,
        version=1,
        minor_version=1,
        data={
            CONF_HOST: "hub-fixture.local",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: hub_id,
            CONF_ACCESS_TOKEN: "fixture-device-bearer",
            CONF_CREDENTIAL_GENERATION: credential_generation,
        },
    )
    entry.add_to_hass(hass)
    return entry


async def setup_fixture_entry(
    hass: HomeAssistant,
    client: FixtureHubClient,
    *,
    title: str = HUB_NAME,
) -> MockConfigEntry:
    entry = configured_entry(hass, title=title)
    with patch("custom_components.teslatlas_hub.create_client", return_value=client):
        assert await hass.config_entries.async_setup(entry.entry_id)
    await hass.async_block_till_done()
    return entry
```

- [ ] **Step 2: Write the failing parent/platform tests**

Add the shown imports and tests to `tests/test_init.py`; replace its private
entry builder with `configured_entry` from `tests.helpers`.

```python
from unittest.mock import AsyncMock, patch

from homeassistant.const import Platform
from homeassistant.helpers import device_registry as dr

from custom_components.teslatlas_hub.const import CONF_HUB_ID, DOMAIN, HUB_NAME
from custom_components.teslatlas_hub.models import HubSnapshot
from tests.helpers import FixtureHubClient, configured_entry, setup_fixture_entry


async def test_setup_creates_fixed_name_hub_parent_and_forwards_both_platforms(
    hass,
) -> None:
    fixture_client = FixtureHubClient()
    entry = configured_entry(hass, title="PRIVATE_LEGACY_HUB")
    forward = AsyncMock()
    with (
        patch(
            "custom_components.teslatlas_hub.create_client",
            return_value=fixture_client,
        ),
        patch.object(hass.config_entries, "async_forward_entry_setups", forward),
    ):
        assert await hass.config_entries.async_setup(entry.entry_id)
    parent = dr.async_get(hass).async_get_device(
        identifiers={(DOMAIN, entry.data[CONF_HUB_ID])}
    )
    assert parent is not None
    assert parent.name == HUB_NAME
    forward.assert_awaited_once_with(entry, (Platform.BINARY_SENSOR, Platform.SENSOR))
    await entry.runtime_data.async_shutdown()


async def test_vehicle_devices_link_to_explicit_hub_parent(hass) -> None:
    entry = await setup_fixture_entry(hass, FixtureHubClient())
    registry = dr.async_get(hass)
    parent = registry.async_get_device(identifiers={(DOMAIN, entry.data[CONF_HUB_ID])})
    assert parent is not None
    for vehicle_id in ("vehicle-alpha", "vehicle-beta"):
        vehicle = registry.async_get_device(
            identifiers={(DOMAIN, f"hub-fixture:{vehicle_id}")}
        )
        assert vehicle is not None
        assert vehicle.via_device_id == parent.id
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_zero_vehicle_snapshot_still_creates_hub_parent(hass) -> None:
    client = FixtureHubClient()
    client.snapshot = HubSnapshot.create(info=client.snapshot.info, vehicles=())
    entry = await setup_fixture_entry(hass, client)
    registry = dr.async_get(hass)
    parent = registry.async_get_device(identifiers={(DOMAIN, entry.data[CONF_HUB_ID])})
    assert parent is not None
    owned = [
        device
        for device in registry.devices.values()
        if entry.entry_id in device.config_entries
    ]
    assert owned == [parent]
    assert await hass.config_entries.async_unload(entry.entry_id)
```

- [ ] **Step 3: Run the parent/platform tests and verify red**

Run: `uv run pytest tests/test_init.py::test_setup_creates_fixed_name_hub_parent_and_forwards_both_platforms tests/test_init.py::test_vehicle_devices_link_to_explicit_hub_parent tests/test_init.py::test_zero_vehicle_snapshot_still_creates_hub_parent -q`

Expected: FAIL because setup forwards only `sensor`, has no explicit parent, and
vehicle devices have no `via_device`.

- [ ] **Step 4: Verify the fixed name and change the platform tuple**

In `const.py`, retain the contract-plan fixed name and replace the platform
tuple with these exact values:

```python
HUB_NAME: Final = "Teslatlas Hub"
PLATFORMS: Final = (Platform.BINARY_SENSOR, Platform.SENSOR)
```

- [ ] **Step 5: Replace `entity.py` with the complete shared boundary**

```python
"""Shared entity support for Teslatlas Hub."""

from __future__ import annotations

from typing import override

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers.device_registry import DeviceEntry, DeviceInfo
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import CONF_HUB_ID, DOMAIN, HUB_NAME
from .coordinator import TeslatlasDataCoordinator
from .models import VehicleState


class TeslatlasCoordinatorEntity(CoordinatorEntity[TeslatlasDataCoordinator]):
    """Base for translated, push-updated Teslatlas entities."""

    _attr_has_entity_name = True


@callback
def async_ensure_hub_device(
    hass: HomeAssistant,
    entry: ConfigEntry,
    hub_id: str,
) -> DeviceEntry:
    """Create or refresh the fixed-name parent even with zero vehicles."""
    return dr.async_get(hass).async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={(DOMAIN, hub_id)},
        manufacturer="Teslatlas",
        model=HUB_NAME,
        name=HUB_NAME,
    )


class TeslatlasVehicleEntity(TeslatlasCoordinatorEntity):
    """Base for one entity attached to one public vehicle projection."""

    def __init__(
        self,
        entry: ConfigEntry[TeslatlasDataCoordinator],
        vehicle_id: str,
        entity_key: str,
    ) -> None:
        super().__init__(entry.runtime_data)
        self._vehicle_id = vehicle_id
        hub_id = entry.data[CONF_HUB_ID]
        self._attr_unique_id = f"{hub_id}_{vehicle_id}_{entity_key}"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, f"{hub_id}:{vehicle_id}")},
            manufacturer="Tesla",
            model="Vehicle",
            name=self.coordinator.data.vehicles[vehicle_id].name,
            via_device=(DOMAIN, hub_id),
        )

    @property
    def vehicle(self) -> VehicleState | None:
        """Return the current projection, or none after retirement."""
        return self.coordinator.data.vehicles.get(self._vehicle_id)

    @property
    @override
    def available(self) -> bool:
        """Require live coordinator continuity and a current vehicle."""
        return super().available and self.vehicle is not None
```

- [ ] **Step 6: Register the parent at the exact setup boundary**

Add the import and one call in `__init__.py`. The call belongs after the
successful first refresh and before platform forwarding.

```python
from .entity import async_ensure_hub_device

# Inside async_setup_entry, in the existing try block:
await coordinator.async_config_entry_first_refresh()
async_ensure_hub_device(hass, entry, entry.data[CONF_HUB_ID])
await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
```

Do not read a display name from `HubInfo`; it has no name field. Do not add a
synthetic Hub entity.

- [ ] **Step 7: Run the exact Task 1 green gate**

Run: `uv run pytest tests/test_init.py tests/test_config_flow.py -q`

Expected: PASS, including the contract-plan fixed-title config-flow regression.

- [ ] **Step 8: Run Task 1 static checks**

Run: `uv run ruff check custom_components/teslatlas_hub/const.py custom_components/teslatlas_hub/entity.py custom_components/teslatlas_hub/__init__.py tests/helpers.py tests/test_init.py tests/test_config_flow.py`

Run: `uv run ruff format --check custom_components/teslatlas_hub/const.py custom_components/teslatlas_hub/entity.py custom_components/teslatlas_hub/__init__.py tests/helpers.py tests/test_init.py tests/test_config_flow.py`

Expected: PASS.

- [ ] **Step 9: Commit Task 1**

```bash
git add custom_components/teslatlas_hub/const.py custom_components/teslatlas_hub/entity.py custom_components/teslatlas_hub/__init__.py tests/helpers.py tests/test_init.py
git commit -m "feat: create stable Hub device hierarchy"
```

### Task 2: Replace temporary sensors with the exact protocol surface

**Files:**
- Rewrite: `custom_components/teslatlas_hub/sensor.py`
- Create: `custom_components/teslatlas_hub/binary_sensor.py`
- Rewrite: `tests/test_sensor.py`
- Create: `tests/test_binary_sensor.py`
- Verify: `tests/helpers.py`

**Interfaces:**
- Sensor suffixes: `state_of_charge`, `charging_state`, `estimated_range`, `odometer`, `activity_state`, `inside_temperature`, `outside_temperature`, `data_quality`, `observation_timestamp`.
- Binary-sensor suffixes: `locked`, `climate_on`.
- `locked` uses `BinarySensorDeviceClass.LOCK` and `None if locked is None else not locked`; `climate_on` returns the source boolean directly and declares no device class.
- `data_quality` and `observation_timestamp` use `entity_registry_enabled_default=False` and `EntityCategory.DIAGNOSTIC`.

- [ ] **Step 1: Write the exact sensor-description and value tests**

Replace `tests/test_sensor.py` with the complete body shown in this step. The
contract-plan parser test already proves list/current divergence; this module
proves the entity consumes only `VehicleState.state` from that validated current
projection.

```python
"""Tests for the exact Teslatlas vehicle sensor surface."""

from __future__ import annotations

from dataclasses import replace
from datetime import datetime, timezone

from homeassistant.components.sensor import SensorDeviceClass, SensorStateClass
from homeassistant.const import (
    PERCENTAGE,
    EntityCategory,
    UnitOfLength,
    UnitOfTemperature,
)
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from homeassistant.helpers.entity_registry import RegistryEntryDisabler

from custom_components.teslatlas_hub.const import DOMAIN
from custom_components.teslatlas_hub.models import HubSnapshot
from custom_components.teslatlas_hub.sensor import (
    VEHICLE_SENSORS,
    TeslatlasVehicleSensor,
)
from tests.helpers import FixtureHubClient, setup_fixture_entry

SENSOR_KEYS = {
    "state_of_charge",
    "charging_state",
    "estimated_range",
    "odometer",
    "activity_state",
    "inside_temperature",
    "outside_temperature",
    "data_quality",
    "observation_timestamp",
}

EXPECTED_SENSOR_METADATA = {
    "state_of_charge": (
        SensorDeviceClass.BATTERY,
        PERCENTAGE,
        SensorStateClass.MEASUREMENT,
        None,
        True,
    ),
    "charging_state": (None, None, None, None, True),
    "estimated_range": (
        SensorDeviceClass.DISTANCE,
        UnitOfLength.KILOMETERS,
        SensorStateClass.MEASUREMENT,
        None,
        True,
    ),
    "odometer": (
        SensorDeviceClass.DISTANCE,
        UnitOfLength.KILOMETERS,
        None,
        None,
        True,
    ),
    "activity_state": (None, None, None, None, True),
    "inside_temperature": (
        SensorDeviceClass.TEMPERATURE,
        UnitOfTemperature.CELSIUS,
        SensorStateClass.MEASUREMENT,
        None,
        True,
    ),
    "outside_temperature": (
        SensorDeviceClass.TEMPERATURE,
        UnitOfTemperature.CELSIUS,
        SensorStateClass.MEASUREMENT,
        None,
        True,
    ),
    "data_quality": (None, None, None, EntityCategory.DIAGNOSTIC, False),
    "observation_timestamp": (
        SensorDeviceClass.TIMESTAMP,
        None,
        None,
        EntityCategory.DIAGNOSTIC,
        False,
    ),
}


def test_sensor_units_device_classes_and_state_classes_are_exact() -> None:
    by_key = {description.key: description for description in VEHICLE_SENSORS}
    assert set(by_key) == SENSOR_KEYS
    assert {
        key: (
            value.device_class,
            value.native_unit_of_measurement,
            value.state_class,
            value.entity_category,
            value.entity_registry_enabled_default,
        )
        for key, value in by_key.items()
    } == EXPECTED_SENSOR_METADATA


async def test_activity_sensor_consumes_current_projection_state(hass) -> None:
    client = FixtureHubClient()
    vehicle = replace(
        client.snapshot.vehicles["vehicle-alpha"],
        state="current-state-wins",
    )
    client.snapshot = HubSnapshot.create(
        info=client.snapshot.info,
        vehicles=(vehicle, client.snapshot.vehicles["vehicle-beta"]),
    )
    entry = await setup_fixture_entry(hass, client)
    description = next(item for item in VEHICLE_SENSORS if item.key == "activity_state")
    entity = TeslatlasVehicleSensor(entry, vehicle.vehicle_id, description)
    assert entity.native_value == "current-state-wins"
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_observation_timestamp_is_exact_utc_datetime(hass) -> None:
    expected = datetime(2026, 8, 31, 12, 34, 56, tzinfo=timezone.utc)
    client = FixtureHubClient()
    vehicle = replace(client.snapshot.vehicles["vehicle-alpha"], observed_at=expected)
    client.snapshot = HubSnapshot.create(
        info=client.snapshot.info,
        vehicles=(vehicle, client.snapshot.vehicles["vehicle-beta"]),
    )
    entry = await setup_fixture_entry(hass, client)
    description = next(
        item for item in VEHICLE_SENSORS if item.key == "observation_timestamp"
    )
    entity = TeslatlasVehicleSensor(entry, vehicle.vehicle_id, description)
    assert description.device_class is SensorDeviceClass.TIMESTAMP
    assert entity.native_value == expected
    assert entity.native_value.tzinfo is timezone.utc
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_diagnostic_entities_start_disabled(hass) -> None:
    entry = await setup_fixture_entry(hass, FixtureHubClient())
    registry = er.async_get(hass)
    alpha = {
        item.unique_id.removeprefix("hub-fixture_vehicle-alpha_"): item
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
        if item.unique_id.startswith("hub-fixture_vehicle-alpha_")
    }
    assert set(alpha) == SENSOR_KEYS | {"locked", "climate_on"}
    assert {
        key
        for key, item in alpha.items()
        if item.disabled_by is RegistryEntryDisabler.INTEGRATION
    } == {"data_quality", "observation_timestamp"}
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_exact_unique_id_surface_has_no_hub_entities(hass) -> None:
    entry = await setup_fixture_entry(hass, FixtureHubClient())
    entity_registry = er.async_get(hass)
    device_registry = dr.async_get(hass)
    parent = device_registry.async_get_device(identifiers={(DOMAIN, "hub-fixture")})
    assert parent is not None
    entries = er.async_entries_for_config_entry(entity_registry, entry.entry_id)
    expected_suffixes = SENSOR_KEYS | {"locked", "climate_on"}
    for vehicle_id in ("vehicle-alpha", "vehicle-beta"):
        prefix = f"hub-fixture_{vehicle_id}_"
        assert {
            item.unique_id.removeprefix(prefix)
            for item in entries
            if item.unique_id.startswith(prefix)
        } == expected_suffixes
    assert len(entries) == 2 * len(expected_suffixes)
    assert all(item.device_id != parent.id for item in entries)
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_listener_adds_each_new_vehicle_surface_once(hass) -> None:
    entry = await setup_fixture_entry(hass, FixtureHubClient())
    snapshot = entry.runtime_data.data
    gamma = replace(
        snapshot.vehicles["vehicle-alpha"],
        vehicle_id="vehicle-gamma",
        name="Fixture Gamma",
    )
    updated = HubSnapshot.create(
        info=snapshot.info,
        vehicles=(*snapshot.vehicles.values(), gamma),
    )
    entry.runtime_data.async_set_updated_data(updated)
    entry.runtime_data.async_set_updated_data(updated)
    await hass.async_block_till_done()
    entries = er.async_entries_for_config_entry(er.async_get(hass), entry.entry_id)
    assert (
        sum(item.unique_id.startswith("hub-fixture_vehicle-gamma_") for item in entries)
        == len(SENSOR_KEYS) + 2
    )
    assert await hass.config_entries.async_unload(entry.entry_id)
```

- [ ] **Step 2: Run the sensor tests and verify red**

Run: `uv run pytest tests/test_sensor.py -q`

Expected: FAIL because unsupported Hub/vehicle descriptions remain, the model
field names differ, and there is no observation-timestamp entity.

- [ ] **Step 3: Write both exact binary-sensor polarity tests**

Create `tests/test_binary_sensor.py` with this complete body:

```python
"""Tests for protocol-backed Teslatlas binary sensors."""

from __future__ import annotations

from dataclasses import replace

import pytest
from homeassistant.components.binary_sensor import BinarySensorDeviceClass

from custom_components.teslatlas_hub.models import HubSnapshot
from custom_components.teslatlas_hub.binary_sensor import (
    VEHICLE_BINARY_SENSORS,
    TeslatlasVehicleBinarySensor,
)
from tests.helpers import FixtureHubClient, setup_fixture_entry


@pytest.mark.parametrize(
    ("locked", "expected"), ((True, False), (False, True), (None, None))
)
async def test_lock_binary_sensor_uses_ha_lock_polarity(
    hass, locked: bool | None, expected: bool | None
) -> None:
    client = FixtureHubClient()
    vehicle = replace(client.snapshot.vehicles["vehicle-alpha"], locked=locked)
    client.snapshot = HubSnapshot.create(
        info=client.snapshot.info,
        vehicles=(vehicle, client.snapshot.vehicles["vehicle-beta"]),
    )
    entry = await setup_fixture_entry(hass, client)
    description = next(item for item in VEHICLE_BINARY_SENSORS if item.key == "locked")
    entity = TeslatlasVehicleBinarySensor(entry, vehicle.vehicle_id, description)
    assert description.device_class is BinarySensorDeviceClass.LOCK
    assert entity.is_on is expected
    assert await hass.config_entries.async_unload(entry.entry_id)


@pytest.mark.parametrize(
    ("climate_on", "expected"), ((True, True), (False, False), (None, None))
)
async def test_climate_binary_sensor_preserves_source_polarity(
    hass, climate_on: bool | None, expected: bool | None
) -> None:
    client = FixtureHubClient()
    vehicle = replace(client.snapshot.vehicles["vehicle-alpha"], climate_on=climate_on)
    client.snapshot = HubSnapshot.create(
        info=client.snapshot.info,
        vehicles=(vehicle, client.snapshot.vehicles["vehicle-beta"]),
    )
    entry = await setup_fixture_entry(hass, client)
    description = next(
        item for item in VEHICLE_BINARY_SENSORS if item.key == "climate_on"
    )
    entity = TeslatlasVehicleBinarySensor(entry, vehicle.vehicle_id, description)
    assert description.device_class is None
    assert entity.is_on is expected
    assert await hass.config_entries.async_unload(entry.entry_id)
```

- [ ] **Step 4: Run the binary-sensor tests and verify red**

Run: `uv run pytest tests/test_binary_sensor.py -q`

Expected: FAIL on import because `binary_sensor.py` does not exist.

- [ ] **Step 5: Replace `sensor.py` with the complete supported platform**

```python
"""Read-only protocol-backed sensors for Teslatlas Hub."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime
from typing import Final, override

from homeassistant.components.sensor import (
    SensorDeviceClass,
    SensorEntity,
    SensorEntityDescription,
    SensorStateClass,
)
from homeassistant.const import (
    PERCENTAGE,
    EntityCategory,
    UnitOfLength,
    UnitOfTemperature,
)
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.entity_platform import AddConfigEntryEntitiesCallback
from homeassistant.helpers.typing import StateType

from . import TeslatlasConfigEntry
from .entity import TeslatlasVehicleEntity
from .models import VehicleState


@dataclass(frozen=True, kw_only=True)
class VehicleSensorDescription(SensorEntityDescription):
    """Describe one protocol-backed vehicle sensor."""

    value_fn: Callable[[VehicleState], StateType | datetime]


VEHICLE_SENSORS: Final = (
    VehicleSensorDescription(
        key="state_of_charge",
        translation_key="state_of_charge",
        device_class=SensorDeviceClass.BATTERY,
        native_unit_of_measurement=PERCENTAGE,
        state_class=SensorStateClass.MEASUREMENT,
        value_fn=lambda vehicle: vehicle.battery_level_percent,
    ),
    VehicleSensorDescription(
        key="charging_state",
        translation_key="charging_state",
        value_fn=lambda vehicle: vehicle.charging_state,
    ),
    VehicleSensorDescription(
        key="estimated_range",
        translation_key="estimated_range",
        device_class=SensorDeviceClass.DISTANCE,
        native_unit_of_measurement=UnitOfLength.KILOMETERS,
        state_class=SensorStateClass.MEASUREMENT,
        value_fn=lambda vehicle: vehicle.range_km,
    ),
    VehicleSensorDescription(
        key="odometer",
        translation_key="odometer",
        device_class=SensorDeviceClass.DISTANCE,
        native_unit_of_measurement=UnitOfLength.KILOMETERS,
        state_class=None,
        value_fn=lambda vehicle: vehicle.odometer_km,
    ),
    VehicleSensorDescription(
        key="activity_state",
        translation_key="activity_state",
        value_fn=lambda vehicle: vehicle.state,
    ),
    VehicleSensorDescription(
        key="inside_temperature",
        translation_key="inside_temperature",
        device_class=SensorDeviceClass.TEMPERATURE,
        native_unit_of_measurement=UnitOfTemperature.CELSIUS,
        state_class=SensorStateClass.MEASUREMENT,
        value_fn=lambda vehicle: vehicle.inside_temperature_c,
    ),
    VehicleSensorDescription(
        key="outside_temperature",
        translation_key="outside_temperature",
        device_class=SensorDeviceClass.TEMPERATURE,
        native_unit_of_measurement=UnitOfTemperature.CELSIUS,
        state_class=SensorStateClass.MEASUREMENT,
        value_fn=lambda vehicle: vehicle.outside_temperature_c,
    ),
    VehicleSensorDescription(
        key="data_quality",
        translation_key="data_quality",
        entity_category=EntityCategory.DIAGNOSTIC,
        entity_registry_enabled_default=False,
        value_fn=lambda vehicle: vehicle.quality,
    ),
    VehicleSensorDescription(
        key="observation_timestamp",
        translation_key="observation_timestamp",
        device_class=SensorDeviceClass.TIMESTAMP,
        entity_category=EntityCategory.DIAGNOSTIC,
        entity_registry_enabled_default=False,
        value_fn=lambda vehicle: vehicle.observed_at,
    ),
)


class TeslatlasVehicleSensor(TeslatlasVehicleEntity, SensorEntity):
    """One read-only value from a validated vehicle projection."""

    entity_description: VehicleSensorDescription

    def __init__(
        self,
        entry: TeslatlasConfigEntry,
        vehicle_id: str,
        description: VehicleSensorDescription,
    ) -> None:
        super().__init__(entry, vehicle_id, description.key)
        self.entity_description = description

    @property
    @override
    def native_value(self) -> StateType | datetime:
        """Return only this metric; null never affects sibling availability."""
        if (vehicle := self.vehicle) is None:
            return None
        return self.entity_description.value_fn(vehicle)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
    async_add_entities: AddConfigEntryEntitiesCallback,
) -> None:
    """Add each vehicle's sensor set exactly once."""
    coordinator = entry.runtime_data
    known_vehicle_ids: set[str] = set()

    @callback
    def async_add_new_entities() -> None:
        vehicle_ids = tuple(sorted(set(coordinator.data.vehicles) - known_vehicle_ids))
        if not vehicle_ids:
            return
        async_add_entities(
            [
                TeslatlasVehicleSensor(entry, vehicle_id, description)
                for vehicle_id in vehicle_ids
                for description in VEHICLE_SENSORS
            ]
        )
        known_vehicle_ids.update(vehicle_ids)

    async_add_new_entities()
    entry.async_on_unload(coordinator.async_add_listener(async_add_new_entities))
```

Delete `HubSensorDescription`, `TeslatlasHubSensor`, every temporary
description, and every now-unused import. Add no raw extra-state attributes.

- [ ] **Step 6: Run the sensor platform green gate**

Run: `uv run pytest tests/test_sensor.py -q`

Expected: PASS.

- [ ] **Step 7: Create the complete read-only binary-sensor platform**

```python
"""Read-only protocol-backed binary sensors for Teslatlas Hub."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Final, override

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
    BinarySensorEntityDescription,
)
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.entity_platform import AddConfigEntryEntitiesCallback

from . import TeslatlasConfigEntry
from .entity import TeslatlasVehicleEntity
from .models import VehicleState


@dataclass(frozen=True, kw_only=True)
class VehicleBinarySensorDescription(BinarySensorEntityDescription):
    """Describe one protocol-backed vehicle boolean."""

    value_fn: Callable[[VehicleState], bool | None]


VEHICLE_BINARY_SENSORS: Final = (
    VehicleBinarySensorDescription(
        key="locked",
        translation_key="locked",
        device_class=BinarySensorDeviceClass.LOCK,
        value_fn=lambda vehicle: None if vehicle.locked is None else not vehicle.locked,
    ),
    VehicleBinarySensorDescription(
        key="climate_on",
        translation_key="climate_on",
        value_fn=lambda vehicle: vehicle.climate_on,
    ),
)


class TeslatlasVehicleBinarySensor(TeslatlasVehicleEntity, BinarySensorEntity):
    """One read-only boolean from a validated vehicle projection."""

    entity_description: VehicleBinarySensorDescription

    def __init__(
        self,
        entry: TeslatlasConfigEntry,
        vehicle_id: str,
        description: VehicleBinarySensorDescription,
    ) -> None:
        super().__init__(entry, vehicle_id, description.key)
        self.entity_description = description

    @property
    @override
    def is_on(self) -> bool | None:
        """Return protocol polarity transformed only for HA's lock class."""
        if (vehicle := self.vehicle) is None:
            return None
        return self.entity_description.value_fn(vehicle)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
    async_add_entities: AddConfigEntryEntitiesCallback,
) -> None:
    """Add each vehicle's binary-sensor set exactly once."""
    coordinator = entry.runtime_data
    known_vehicle_ids: set[str] = set()

    @callback
    def async_add_new_entities() -> None:
        vehicle_ids = tuple(sorted(set(coordinator.data.vehicles) - known_vehicle_ids))
        if not vehicle_ids:
            return
        async_add_entities(
            [
                TeslatlasVehicleBinarySensor(entry, vehicle_id, description)
                for vehicle_id in vehicle_ids
                for description in VEHICLE_BINARY_SENSORS
            ]
        )
        known_vehicle_ids.update(vehicle_ids)

    async_add_new_entities()
    entry.async_on_unload(coordinator.async_add_listener(async_add_new_entities))
```

Add no `LockEntity`, `ClimateEntity`, service, setter, or writable method.

- [ ] **Step 8: Run the binary-sensor platform green gate**

Run: `uv run pytest tests/test_binary_sensor.py -q`

Expected: PASS.

- [ ] **Step 9: Run the combined entity gate**

Run: `uv run pytest tests/test_sensor.py tests/test_binary_sensor.py tests/test_init.py -q`

Expected: PASS with exactly eleven entries per vehicle and no Hub entity.

- [ ] **Step 10: Run Task 2 static checks**

Run: `uv run ruff check custom_components/teslatlas_hub/sensor.py custom_components/teslatlas_hub/binary_sensor.py tests/test_sensor.py tests/test_binary_sensor.py tests/helpers.py`

Run: `uv run ruff format --check custom_components/teslatlas_hub/sensor.py custom_components/teslatlas_hub/binary_sensor.py tests/test_sensor.py tests/test_binary_sensor.py tests/helpers.py`

Expected: PASS.

- [ ] **Step 11: Commit Task 2**

```bash
git add custom_components/teslatlas_hub/sensor.py custom_components/teslatlas_hub/binary_sensor.py tests/test_sensor.py tests/test_binary_sensor.py
git commit -m "feat: align entities with protocol 1.2"
```

### Task 3: Migrate only this entry's temporary registry surface

**Files:**
- Modify: `custom_components/teslatlas_hub/const.py`
- Modify: `custom_components/teslatlas_hub/__init__.py`
- Rewrite: `tests/test_migration.py`

**Interfaces:**
- Bumps `CONFIG_ENTRY_MINOR_VERSION` from `1` to `2`; unknown major versions still fail closed.
- Adds missing `credential_generation=1` to preexisting entries and preserves any existing positive generation unchanged.
- Preserves suffixes: `state_of_charge`, `charging_state`, `estimated_range`, `odometer`, `activity_state`, `inside_temperature`, `outside_temperature`, `data_quality`.
- Removes vehicle suffixes: `access_state`, `charging_power`, `charge_limit`, `software_version`, `software_update_state`, `telemetry_age`.
- Removes Hub suffixes: `collector_health`, `fleet_cost`, `backup_age`.
- Never converts `access_state` into `locked`; the new binary entity has a new unique ID and semantic history.

- [ ] **Step 1: Write the exact preexisting-registry fixture**

Start `tests/test_migration.py` with this fixture code. It creates every owned
retained/removed entry plus four isolation sentinels; no registry state is
implicit.

```python
from dataclasses import dataclass
from typing import Any

import pytest
from homeassistant.core import HomeAssistant
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub import async_migrate_entry
from custom_components.teslatlas_hub.const import (
    CONF_CREDENTIAL_GENERATION,
    CONF_HUB_ID,
    DOMAIN,
    HUB_NAME,
)

RETAINED_SUFFIXES = (
    "state_of_charge",
    "charging_state",
    "estimated_range",
    "odometer",
    "activity_state",
    "inside_temperature",
    "outside_temperature",
    "data_quality",
)
LEGACY_VEHICLE_SUFFIXES = (
    "access_state",
    "charging_power",
    "charge_limit",
    "software_version",
    "software_update_state",
    "telemetry_age",
)
LEGACY_HUB_SUFFIXES = ("collector_health", "fleet_cost", "backup_age")
MISSING = object()


@dataclass(frozen=True)
class MigrationSeed:
    entry: MockConfigEntry
    retained: dict[str, tuple[str, str | None]]
    removed: frozenset[str]
    protected: frozenset[str]
    hub_device_id: str


def _entity(
    registry: er.EntityRegistry,
    entry: MockConfigEntry,
    device_id: str,
    unique_id: str,
    *,
    platform: str = DOMAIN,
) -> er.RegistryEntry:
    return registry.async_get_or_create(
        "sensor",
        platform,
        unique_id,
        config_entry=entry,
        device_id=device_id,
        suggested_object_id=unique_id,
    )


def seed_minor_one_registries(
    hass: HomeAssistant,
    *,
    generation: object = MISSING,
) -> MigrationSeed:
    data: dict[str, Any] = {CONF_HUB_ID: "hub-fixture", "opaque": "preserved"}
    if generation is not MISSING:
        data[CONF_CREDENTIAL_GENERATION] = generation
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="PRIVATE_LEGACY_HUB",
        unique_id="hub-fixture",
        version=1,
        minor_version=1,
        data=data,
    )
    entry.add_to_hass(hass)
    other_entry = MockConfigEntry(
        domain=DOMAIN,
        title="Other Hub",
        unique_id="hub-other",
        version=1,
        minor_version=1,
        data={CONF_HUB_ID: "hub-other", CONF_CREDENTIAL_GENERATION: 1},
    )
    other_entry.add_to_hass(hass)

    devices = dr.async_get(hass)
    entities = er.async_get(hass)
    hub = devices.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={(DOMAIN, "hub-fixture")},
        name="PRIVATE_LEGACY_HUB",
    )
    devices.async_update_device(hub.id, name_by_user="User Garage Hub")
    vehicle = devices.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={(DOMAIN, "hub-fixture:vehicle-alpha")},
        name="Fixture Alpha",
        via_device=(DOMAIN, "hub-fixture"),
    )

    retained: dict[str, tuple[str, str | None]] = {}
    for index, suffix in enumerate(RETAINED_SUFFIXES):
        item = _entity(
            entities,
            entry,
            vehicle.id,
            f"hub-fixture_vehicle-alpha_{suffix}",
        )
        item = entities.async_update_entity(
            item.entity_id,
            new_entity_id=f"sensor.user_kept_{index}",
            name=f"User name {index}",
        )
        retained[item.unique_id] = (item.entity_id, item.name)

    removed = {
        _entity(
            entities,
            entry,
            vehicle.id,
            f"hub-fixture_vehicle-alpha_{suffix}",
        ).entity_id
        for suffix in LEGACY_VEHICLE_SUFFIXES
    }
    removed.update(
        _entity(
            entities,
            entry,
            hub.id,
            f"hub-fixture_hub_{suffix}",
        ).entity_id
        for suffix in LEGACY_HUB_SUFFIXES
    )

    foreign_platform = _entity(
        entities,
        entry,
        vehicle.id,
        "hub-fixture_vehicle-alpha_access_state",
        platform="foreign_fixture",
    )
    malformed_device = devices.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={
            (DOMAIN, "hub-fixture:malformed:extra"),
            ("foreign_fixture", "mixed-device"),
        },
        name="Malformed",
    )
    malformed = _entity(
        entities,
        entry,
        malformed_device.id,
        "hub-fixture_malformed:extra_access_state",
    )
    other_device = devices.async_get_or_create(
        config_entry_id=other_entry.entry_id,
        identifiers={(DOMAIN, "hub-other:vehicle-other")},
        name="Other vehicle",
    )
    other = _entity(
        entities,
        other_entry,
        other_device.id,
        "hub-other_vehicle-other_access_state",
    )
    unrelated_device = devices.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={("foreign_fixture", "unrelated")},
        name="Unrelated",
    )
    unrelated = _entity(
        entities,
        entry,
        unrelated_device.id,
        "unrelated_sensor",
        platform="foreign_fixture",
    )
    return MigrationSeed(
        entry=entry,
        retained=retained,
        removed=frozenset(removed),
        protected=frozenset(
            {
                foreign_platform.entity_id,
                malformed.entity_id,
                other.entity_id,
                unrelated.entity_id,
            }
        ),
        hub_device_id=hub.id,
    )


def registry_snapshot(
    hass: HomeAssistant, entry: MockConfigEntry
) -> tuple[object, ...]:
    entities = er.async_get(hass)
    devices = dr.async_get(hass)
    return (
        entry.title,
        entry.version,
        entry.minor_version,
        tuple(sorted(entry.data.items())),
        tuple(
            sorted(
                (
                    item.entity_id,
                    item.platform,
                    item.unique_id,
                    item.device_id,
                    item.name,
                )
                for item in entities.entities.values()
            )
        ),
        tuple(
            sorted(
                (
                    item.id,
                    tuple(sorted(item.config_entries)),
                    tuple(sorted(item.identifiers)),
                    item.name,
                    item.name_by_user,
                )
                for item in devices.devices.values()
            )
        ),
    )
```

- [ ] **Step 2: Write the exact migration/isolation tests**

Append these tests to `tests/test_migration.py`:

```python
async def test_minor_one_migrates_exact_legacy_surface(hass) -> None:
    seeded = seed_minor_one_registries(hass)
    entry = seeded.entry
    assert await async_migrate_entry(hass, entry)
    registry = er.async_get(hass)
    for unique_id in seeded.retained:
        assert registry.async_get_entity_id("sensor", DOMAIN, unique_id) is not None
    for entity_id in seeded.removed:
        assert registry.async_get(entity_id) is None
    for entity_id in seeded.protected:
        assert registry.async_get(entity_id) is not None
    assert (
        registry.async_get_entity_id(
            "binary_sensor", DOMAIN, "hub-fixture_vehicle-alpha_locked"
        )
        is None
    )
    assert entry.title == HUB_NAME
    assert entry.minor_version == 2
    assert entry.data[CONF_CREDENTIAL_GENERATION] == 1


async def test_minor_one_migration_preserves_retained_customization(hass) -> None:
    seeded = seed_minor_one_registries(hass)
    assert await async_migrate_entry(hass, seeded.entry)
    registry = er.async_get(hass)
    assert {
        unique_id: (
            registry.async_get_entity_id("sensor", DOMAIN, unique_id),
            registry.async_get(entity_id).name,
        )
        for unique_id, (entity_id, _name) in seeded.retained.items()
    } == seeded.retained
    hub = dr.async_get(hass).async_get(seeded.hub_device_id)
    assert hub is not None
    assert hub.name == HUB_NAME
    assert hub.name_by_user == "User Garage Hub"


async def test_minor_one_migration_removes_only_exact_owned_legacy_ids(hass) -> None:
    seeded = seed_minor_one_registries(hass)
    assert await async_migrate_entry(hass, seeded.entry)
    registry = er.async_get(hass)
    assert all(registry.async_get(entity_id) is None for entity_id in seeded.removed)
    assert all(
        registry.async_get(entity_id) is not None for entity_id in seeded.protected
    )


@pytest.mark.parametrize("generation", (1, 7))
async def test_minor_one_migration_preserves_positive_generation(
    hass, generation: int
) -> None:
    seeded = seed_minor_one_registries(hass, generation=generation)
    assert await async_migrate_entry(hass, seeded.entry)
    assert seeded.entry.data[CONF_CREDENTIAL_GENERATION] == generation


@pytest.mark.parametrize("generation", (None, True, 0, -1, 1.5, "1"))
async def test_invalid_generation_is_rejected_before_any_mutation(
    hass, generation: object
) -> None:
    seeded = seed_minor_one_registries(hass, generation=generation)
    before = registry_snapshot(hass, seeded.entry)
    assert not await async_migrate_entry(hass, seeded.entry)
    assert registry_snapshot(hass, seeded.entry) == before
```

- [ ] **Step 3: Run migration tests and verify red**

Run: `uv run pytest tests/test_migration.py -q`

Expected: FAIL because minor version `1` only increments metadata and performs no registry reconciliation.

- [ ] **Step 4: Bump the minor version and add exact legacy suffix sets**

Set the target minor version and add these immutable sets to `const.py`:

```python
CONFIG_ENTRY_MINOR_VERSION: Final = 2
LEGACY_HUB_SUFFIXES: Final = frozenset({"collector_health", "fleet_cost", "backup_age"})
LEGACY_VEHICLE_SUFFIXES: Final = frozenset(
    {
        "access_state",
        "charging_power",
        "charge_limit",
        "software_version",
        "software_update_state",
        "telemetry_age",
    }
)
```

- [ ] **Step 5: Replace migration with the complete narrow traversal**

Add `device_registry as dr`, `entity_registry as er`, and `DeviceEntry` imports
to `__init__.py`, import the two suffix sets and `HUB_NAME`, then replace
`async_migrate_entry` and add the helper exactly as shown:

```python
def _is_exact_owned_legacy_unique_id(
    unique_id: str,
    device: DeviceEntry,
    hub_id: str,
) -> bool:
    if len(device.identifiers) != 1:
        return False
    domain, identifier = next(iter(device.identifiers))
    if domain != DOMAIN:
        return False
    if identifier == hub_id:
        return unique_id in {f"{hub_id}_hub_{suffix}" for suffix in LEGACY_HUB_SUFFIXES}
    vehicle_prefix = f"{hub_id}:"
    if not identifier.startswith(vehicle_prefix):
        return False
    vehicle_id = identifier.removeprefix(vehicle_prefix)
    if not vehicle_id:
        return False
    return unique_id in {
        f"{hub_id}_{vehicle_id}_{suffix}" for suffix in LEGACY_VEHICLE_SUFFIXES
    }


async def async_migrate_entry(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
) -> bool:
    """Migrate only exact integration-owned temporary registry entries."""
    if entry.version != CONFIG_ENTRY_VERSION:
        return False
    if entry.minor_version >= CONFIG_ENTRY_MINOR_VERSION:
        return True

    data = dict(entry.data)
    if CONF_CREDENTIAL_GENERATION not in data:
        data[CONF_CREDENTIAL_GENERATION] = 1
    else:
        generation = data[CONF_CREDENTIAL_GENERATION]
        if type(generation) is not int or generation <= 0:
            return False

    entity_registry = er.async_get(hass)
    device_registry = dr.async_get(hass)
    hub_id = entry.data[CONF_HUB_ID]
    entries = tuple(er.async_entries_for_config_entry(entity_registry, entry.entry_id))
    for registry_entry in entries:
        if registry_entry.platform != DOMAIN or registry_entry.device_id is None:
            continue
        device = device_registry.async_get(registry_entry.device_id)
        if device is None or entry.entry_id not in device.config_entries:
            continue
        if _is_exact_owned_legacy_unique_id(registry_entry.unique_id, device, hub_id):
            entity_registry.async_remove(registry_entry.entity_id)

    hub_device = device_registry.async_get_device_by_identifier(
        (DOMAIN, hub_id), entry.entry_id
    )
    if hub_device is not None:
        device_registry.async_update_device(hub_device.id, name=HUB_NAME)
    hass.config_entries.async_update_entry(
        entry,
        data=data,
        title=HUB_NAME,
        version=CONFIG_ENTRY_VERSION,
        minor_version=CONFIG_ENTRY_MINOR_VERSION,
    )
    return True
```

Do not pass `name_by_user` to `async_update_device`; omission preserves it. Do
not update retained entries and do not synthesize `locked` from `access_state`.

- [ ] **Step 6: Run the migration green gate**

Run: `uv run pytest tests/test_migration.py -q`

Expected: PASS with nine removals and all four isolation sentinels preserved.

- [ ] **Step 7: Run the post-migration setup gate**

Run: `uv run pytest tests/test_migration.py tests/test_init.py tests/test_sensor.py tests/test_binary_sensor.py -q`

Expected: PASS, including a post-migration setup that creates new `locked`/`climate_on` entities and vehicle-to-parent links.

- [ ] **Step 8: Run Task 3 static checks**

Run: `uv run ruff check custom_components/teslatlas_hub/const.py custom_components/teslatlas_hub/__init__.py tests/test_migration.py`

Run: `uv run ruff format --check custom_components/teslatlas_hub/const.py custom_components/teslatlas_hub/__init__.py tests/test_migration.py`

Expected: PASS.

- [ ] **Step 9: Commit Task 3**

```bash
git add custom_components/teslatlas_hub/const.py custom_components/teslatlas_hub/__init__.py tests/test_migration.py
git commit -m "feat: migrate temporary entity registries"
```

### Task 4: Enforce availability, stale-device, translation, and privacy contracts

**Files:**
- Modify: `custom_components/teslatlas_hub/__init__.py`
- Modify: `custom_components/teslatlas_hub/strings.json`
- Modify: `custom_components/teslatlas_hub/translations/en.json`
- Modify: `tests/test_sensor.py`
- Modify: `tests/test_binary_sensor.py`
- Modify: `tests/test_init.py`
- Verify: `tests/test_diagnostics.py`
- Create: `tests/test_translations.py`

**Interfaces:**
- Produces: `async_remove_config_entry_device(hass, entry, device_entry) -> bool` which permits removal only for an integration-owned retired vehicle.
- Keeps all vehicle entities unavailable whenever coordinator continuity is false or their vehicle is absent; nullable metric values become only that entity's `unknown` state.
- Translation keys contain only current forms/entities/errors. Precise `observed_at` is exposed only as the disabled-by-default `observation_timestamp` entity's native value; diagnostics, logs, errors, raw attributes, and every other entity state/attribute contain no precise timestamp, identity, location, replay value, secret, or raw protocol field.

- [ ] **Step 1: Write exact stale-device ownership tests**

Add these imports, fixture, and test to `tests/test_init.py`:

```python
from homeassistant.helpers import device_registry as dr

from custom_components.teslatlas_hub import async_remove_config_entry_device
from tests.helpers import FixtureHubClient, setup_fixture_entry


def seed_device_cases(hass, entry):
    registry = dr.async_get(hass)
    active = registry.async_get_device(
        identifiers={(DOMAIN, "hub-fixture:vehicle-alpha")}
    )
    parent = registry.async_get_device(identifiers={(DOMAIN, "hub-fixture")})
    assert active is not None
    assert parent is not None
    retired = registry.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={(DOMAIN, "hub-fixture:vehicle-retired")},
        name="Retired",
        via_device=(DOMAIN, "hub-fixture"),
    )
    foreign = registry.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={("foreign_fixture", "hub-fixture:vehicle-retired")},
        name="Foreign",
    )
    return active, retired, parent, foreign


async def test_remove_config_entry_device_allows_only_retired_owned_vehicle(
    hass,
) -> None:
    entry = await setup_fixture_entry(hass, FixtureHubClient())
    active, retired, parent, foreign = seed_device_cases(hass, entry)
    assert not await async_remove_config_entry_device(hass, entry, active)
    assert await async_remove_config_entry_device(hass, entry, retired)
    assert not await async_remove_config_entry_device(hass, entry, parent)
    assert not await async_remove_config_entry_device(hass, entry, foreign)
    assert await hass.config_entries.async_unload(entry.entry_id)
```

- [ ] **Step 2: Write exact availability and nullable-value tests**

Add these imports/helpers/tests to `tests/test_sensor.py`:

```python
import pytest
from homeassistant.const import STATE_UNAVAILABLE, STATE_UNKNOWN
from homeassistant.helpers.update_coordinator import UpdateFailed


def _state_for(hass, domain: str, suffix: str):
    registry = er.async_get(hass)
    unique_id = f"hub-fixture_vehicle-alpha_{suffix}"
    entity_id = registry.async_get_entity_id(domain, DOMAIN, unique_id)
    assert entity_id is not None
    state = hass.states.get(entity_id)
    assert state is not None
    return state


def _replace_alpha(client: FixtureHubClient, **changes) -> None:
    alpha = replace(client.snapshot.vehicles["vehicle-alpha"], **changes)
    client.snapshot = HubSnapshot.create(
        info=client.snapshot.info,
        vehicles=(alpha, client.snapshot.vehicles["vehicle-beta"]),
    )


async def test_disconnect_makes_every_enabled_vehicle_entity_unavailable(hass) -> None:
    entry = await setup_fixture_entry(hass, FixtureHubClient())
    registry = er.async_get(hass)
    entry.runtime_data.async_set_update_error(UpdateFailed("offline"))
    await hass.async_block_till_done()
    for item in er.async_entries_for_config_entry(registry, entry.entry_id):
        if item.disabled_by is None:
            state = hass.states.get(item.entity_id)
            assert state is not None
            assert state.state == STATE_UNAVAILABLE
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_retired_vehicle_is_unavailable_before_device_cleanup(hass) -> None:
    entry = await setup_fixture_entry(hass, FixtureHubClient())
    snapshot = entry.runtime_data.data
    entry.runtime_data.async_set_updated_data(
        HubSnapshot.create(
            info=snapshot.info,
            vehicles=(snapshot.vehicles["vehicle-beta"],),
        )
    )
    await hass.async_block_till_done()
    assert _state_for(hass, "sensor", "state_of_charge").state == STATE_UNAVAILABLE
    assert _state_for(hass, "binary_sensor", "locked").state == STATE_UNAVAILABLE
    assert await hass.config_entries.async_unload(entry.entry_id)


@pytest.mark.parametrize("reported_state", ("offline", "asleep", "unknown"))
async def test_activity_values_do_not_control_availability(
    hass, reported_state: str
) -> None:
    client = FixtureHubClient()
    _replace_alpha(client, state=reported_state)
    entry = await setup_fixture_entry(hass, client)
    state = _state_for(hass, "sensor", "activity_state")
    assert state.state == reported_state
    assert state.state != STATE_UNAVAILABLE
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_old_observation_time_does_not_control_availability(hass) -> None:
    client = FixtureHubClient()
    _replace_alpha(client, observed_at=datetime(2025, 8, 31, tzinfo=timezone.utc))
    entry = await setup_fixture_entry(hass, client)
    assert _state_for(hass, "sensor", "activity_state").state != STATE_UNAVAILABLE
    assert await hass.config_entries.async_unload(entry.entry_id)


@pytest.mark.parametrize("quality", ("partial", "degraded"))
async def test_quality_values_do_not_control_availability(hass, quality: str) -> None:
    client = FixtureHubClient()
    _replace_alpha(client, quality=quality)
    entry = await setup_fixture_entry(hass, client)
    assert _state_for(hass, "sensor", "activity_state").state != STATE_UNAVAILABLE
    assert await hass.config_entries.async_unload(entry.entry_id)


@pytest.mark.parametrize(
    ("field", "domain", "suffix"),
    (
        ("battery_level_percent", "sensor", "state_of_charge"),
        ("range_km", "sensor", "estimated_range"),
        ("odometer_km", "sensor", "odometer"),
        ("charging_state", "sensor", "charging_state"),
        ("locked", "binary_sensor", "locked"),
        ("climate_on", "binary_sensor", "climate_on"),
    ),
)
async def test_required_nullable_metric_only_makes_its_entity_unknown(
    hass, field: str, domain: str, suffix: str
) -> None:
    client = FixtureHubClient()
    _replace_alpha(client, **{field: None})
    entry = await setup_fixture_entry(hass, client)
    assert _state_for(hass, domain, suffix).state == STATE_UNKNOWN
    assert _state_for(hass, "sensor", "activity_state").state != STATE_UNAVAILABLE
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_missing_optional_temperature_only_makes_temperature_unknown(
    hass,
) -> None:
    client = FixtureHubClient()
    _replace_alpha(client, inside_temperature_c=None)
    entry = await setup_fixture_entry(hass, client)
    assert _state_for(hass, "sensor", "inside_temperature").state == STATE_UNKNOWN
    assert _state_for(hass, "sensor", "outside_temperature").state != STATE_UNKNOWN
    assert await hass.config_entries.async_unload(entry.entry_id)
```

- [ ] **Step 3: Write exact state-attribute and timestamp privacy tests**

Append this code to `tests/test_sensor.py`; the contract plan remains the owner
of sentinel coverage for diagnostics, exceptions, errors, and logs.

```python
import json

from homeassistant.components.sensor import ATTR_STATE_CLASS
from homeassistant.const import (
    ATTR_DEVICE_CLASS,
    ATTR_FRIENDLY_NAME,
    ATTR_UNIT_OF_MEASUREMENT,
)

ALLOWED_ENTITY_ATTRIBUTES = {
    ATTR_DEVICE_CLASS,
    ATTR_FRIENDLY_NAME,
    ATTR_STATE_CLASS,
    ATTR_UNIT_OF_MEASUREMENT,
}


async def test_entity_states_have_only_home_assistant_metadata(hass) -> None:
    entry = await setup_fixture_entry(hass, FixtureHubClient())
    registry = er.async_get(hass)
    for item in er.async_entries_for_config_entry(registry, entry.entry_id):
        if (state := hass.states.get(item.entity_id)) is not None:
            assert set(state.attributes) <= ALLOWED_ENTITY_ATTRIBUTES
    assert await hass.config_entries.async_unload(entry.entry_id)


async def test_precise_timestamp_exists_only_in_observation_native_value(hass) -> None:
    exact = datetime(2026, 8, 31, 12, 34, 56, tzinfo=timezone.utc)
    client = FixtureHubClient()
    _replace_alpha(client, observed_at=exact)
    entry = await setup_fixture_entry(hass, client)
    description = next(
        item for item in VEHICLE_SENSORS if item.key == "observation_timestamp"
    )
    observation = TeslatlasVehicleSensor(
        entry, "vehicle-alpha", description
    ).native_value
    assert observation == exact
    registry = er.async_get(hass)
    other_states = {
        item.entity_id: hass.states.get(item.entity_id).as_dict()
        for item in er.async_entries_for_config_entry(registry, entry.entry_id)
        if not item.unique_id.endswith("_observation_timestamp")
        and hass.states.get(item.entity_id) is not None
    }
    encoded = json.dumps(other_states, sort_keys=True)
    assert exact.isoformat() not in encoded
    assert "hub-fixture" not in encoded
    assert "vehicle-alpha" not in encoded
    assert await hass.config_entries.async_unload(entry.entry_id)
```

- [ ] **Step 4: Run lifecycle/privacy tests and verify red**

Run: `uv run pytest tests/test_init.py::test_remove_config_entry_device_allows_only_retired_owned_vehicle tests/test_sensor.py -q`

Expected: FAIL because the stale-device callback is absent; the availability
and privacy assertions establish the retained entity-base behavior.

- [ ] **Step 5: Implement the exact stale-device callback**

Add `DeviceEntry` to the `device_registry` imports in `__init__.py`, then add
this complete callback:

```python
async def async_remove_config_entry_device(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
    device_entry: DeviceEntry,
) -> bool:
    """Permit removal only for this entry's retired vehicle devices."""
    hub_id = entry.data[CONF_HUB_ID]
    if entry.entry_id not in device_entry.config_entries:
        return False
    if len(device_entry.identifiers) != 1:
        return False
    domain, identifier = next(iter(device_entry.identifiers))
    if domain != DOMAIN or identifier == hub_id:
        return False
    prefix = f"{hub_id}:"
    if not identifier.startswith(prefix):
        return False
    vehicle_id = identifier.removeprefix(prefix)
    if not vehicle_id:
        return False
    return vehicle_id not in entry.runtime_data.data.vehicles
```

- [ ] **Step 6: Run the stale-device callback green gate**

Run: `uv run pytest tests/test_init.py::test_remove_config_entry_device_allows_only_retired_owned_vehicle -q`

Expected: PASS.

- [ ] **Step 7: Write the exact translation-corpus test and verify red**

Create `tests/test_translations.py` with this complete body:

```python
"""Tests for the literal entity translation surface."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

INTEGRATION = Path("custom_components/teslatlas_hub")
EXPECTED_ENTITY_TRANSLATIONS = {
    "binary_sensor": {
        "climate_on": {"name": "Climate on"},
        "locked": {"name": "Locked"},
    },
    "sensor": {
        "activity_state": {"name": "Activity state"},
        "charging_state": {"name": "Charging state"},
        "data_quality": {"name": "Data quality"},
        "estimated_range": {"name": "Estimated range"},
        "inside_temperature": {"name": "Inside temperature"},
        "observation_timestamp": {"name": "Observation timestamp"},
        "odometer": {"name": "Odometer"},
        "outside_temperature": {"name": "Outside temperature"},
        "state_of_charge": {"name": "State of charge"},
    },
}
OBSOLETE_KEYS = {
    "access_state",
    "backup_age",
    "charge_limit",
    "charging_power",
    "collector_health",
    "fleet_cost",
    "software_update_state",
    "software_version",
    "telemetry_age",
}


@pytest.mark.parametrize("relative_path", ("strings.json", "translations/en.json"))
def test_entity_translation_corpus_is_exact(relative_path: str) -> None:
    document = json.loads((INTEGRATION / relative_path).read_text(encoding="utf-8"))
    assert document["entity"] == EXPECTED_ENTITY_TRANSLATIONS
    encoded = json.dumps(document["entity"], sort_keys=True)
    assert all(key not in encoded for key in OBSOLETE_KEYS)
    assert "zeroconf" not in json.dumps(document, sort_keys=True).lower()
```

Run: `uv run pytest tests/test_translations.py -q`

Expected: FAIL because both files still contain the temporary entity keys and
neither contains binary-sensor or observation-timestamp translations.

- [ ] **Step 8: Replace the entity member in both translation files exactly**

In both `custom_components/teslatlas_hub/strings.json` and
`custom_components/teslatlas_hub/translations/en.json`, preserve the
contract-plan `config` member byte-for-byte and replace the complete `entity`
member with this exact JSON object:

```json
"entity": {
  "binary_sensor": {
    "climate_on": {
      "name": "Climate on"
    },
    "locked": {
      "name": "Locked"
    }
  },
  "sensor": {
    "activity_state": {
      "name": "Activity state"
    },
    "charging_state": {
      "name": "Charging state"
    },
    "data_quality": {
      "name": "Data quality"
    },
    "estimated_range": {
      "name": "Estimated range"
    },
    "inside_temperature": {
      "name": "Inside temperature"
    },
    "observation_timestamp": {
      "name": "Observation timestamp"
    },
    "odometer": {
      "name": "Odometer"
    },
    "outside_temperature": {
      "name": "Outside temperature"
    },
    "state_of_charge": {
      "name": "State of charge"
    }
  }
}
```

Do not restore the contract-plan-removed Zeroconf flow text. Do not change any
config, reauthentication, reconfiguration, abort, or safe error copy.

- [ ] **Step 9: Run the translation JSON green gate**

Run: `uv run pytest tests/test_translations.py -q`

Run: `uv run python -m json.tool custom_components/teslatlas_hub/strings.json >/dev/null`

Run: `uv run python -m json.tool custom_components/teslatlas_hub/translations/en.json >/dev/null`

Expected: PASS.

- [ ] **Step 10: Run the combined entity/lifecycle/privacy gate**

Run: `uv run pytest tests/test_sensor.py tests/test_binary_sensor.py tests/test_init.py tests/test_diagnostics.py tests/test_translations.py -q`

Expected: PASS.

- [ ] **Step 11: Run the plan-wide regression gate**

Run: `uv run pytest -q`

Expected: PASS.

- [ ] **Step 12: Run the plan-wide static gate**

Run: `uv run ruff check .`

Run: `uv run ruff format --check .`

Run: `git diff --check`

Expected: PASS with no warnings or formatting diff.

- [ ] **Step 13: Commit Task 4**

```bash
git add custom_components/teslatlas_hub/__init__.py custom_components/teslatlas_hub/strings.json custom_components/teslatlas_hub/translations/en.json tests/test_sensor.py tests/test_init.py tests/test_translations.py
git commit -m "test: enforce entity lifecycle and privacy"
```
