"""Read-only sensors for Teslatlas Hub."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
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
    UnitOfPower,
    UnitOfTemperature,
    UnitOfTime,
)
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.device_registry import DeviceInfo
from homeassistant.helpers.entity_platform import AddConfigEntryEntitiesCallback
from homeassistant.helpers.typing import StateType

from . import TeslatlasConfigEntry
from .const import DOMAIN
from .entity import TeslatlasCoordinatorEntity
from .models import HubSnapshot, VehicleState


@dataclass(frozen=True, kw_only=True)
class HubSensorDescription(SensorEntityDescription):
    """Describe one Hub-level sensor value."""

    value_fn: Callable[[HubSnapshot], StateType]


@dataclass(frozen=True, kw_only=True)
class VehicleSensorDescription(SensorEntityDescription):
    """Describe one vehicle-level sensor value."""

    value_fn: Callable[[VehicleState], StateType]


HUB_SENSOR_DESCRIPTIONS: Final = (
    HubSensorDescription(
        key="collector_health",
        translation_key="collector_health",
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda snapshot: snapshot.status.collector_health,
    ),
    HubSensorDescription(
        key="fleet_cost",
        translation_key="fleet_cost",
        device_class=SensorDeviceClass.MONETARY,
        native_unit_of_measurement="USD",
        state_class=SensorStateClass.TOTAL,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda snapshot: snapshot.status.fleet_cost_usd,
    ),
    HubSensorDescription(
        key="backup_age",
        translation_key="backup_age",
        device_class=SensorDeviceClass.DURATION,
        native_unit_of_measurement=UnitOfTime.SECONDS,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda snapshot: snapshot.status.backup_age_seconds,
    ),
)

VEHICLE_SENSOR_DESCRIPTIONS: Final = (
    VehicleSensorDescription(
        key="state_of_charge",
        translation_key="state_of_charge",
        device_class=SensorDeviceClass.BATTERY,
        native_unit_of_measurement=PERCENTAGE,
        state_class=SensorStateClass.MEASUREMENT,
        value_fn=lambda vehicle: vehicle.state_of_charge,
    ),
    VehicleSensorDescription(
        key="charging_state",
        translation_key="charging_state",
        value_fn=lambda vehicle: vehicle.charging_state,
    ),
    VehicleSensorDescription(
        key="charging_power",
        translation_key="charging_power",
        device_class=SensorDeviceClass.POWER,
        native_unit_of_measurement=UnitOfPower.KILO_WATT,
        state_class=SensorStateClass.MEASUREMENT,
        value_fn=lambda vehicle: vehicle.charging_power_kw,
    ),
    VehicleSensorDescription(
        key="charge_limit",
        translation_key="charge_limit",
        native_unit_of_measurement=PERCENTAGE,
        state_class=SensorStateClass.MEASUREMENT,
        value_fn=lambda vehicle: vehicle.charge_limit_percent,
    ),
    VehicleSensorDescription(
        key="estimated_range",
        translation_key="estimated_range",
        device_class=SensorDeviceClass.DISTANCE,
        native_unit_of_measurement=UnitOfLength.KILOMETERS,
        state_class=SensorStateClass.MEASUREMENT,
        value_fn=lambda vehicle: vehicle.estimated_range_km,
    ),
    VehicleSensorDescription(
        key="odometer",
        translation_key="odometer",
        device_class=SensorDeviceClass.DISTANCE,
        native_unit_of_measurement=UnitOfLength.KILOMETERS,
        state_class=SensorStateClass.TOTAL_INCREASING,
        value_fn=lambda vehicle: vehicle.odometer_km,
    ),
    VehicleSensorDescription(
        key="activity_state",
        translation_key="activity_state",
        value_fn=lambda vehicle: vehicle.activity_state,
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
        key="access_state",
        translation_key="access_state",
        value_fn=lambda vehicle: vehicle.access_state,
    ),
    VehicleSensorDescription(
        key="software_version",
        translation_key="software_version",
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda vehicle: vehicle.software_version,
    ),
    VehicleSensorDescription(
        key="software_update_state",
        translation_key="software_update_state",
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda vehicle: vehicle.software_update_state,
    ),
    VehicleSensorDescription(
        key="telemetry_age",
        translation_key="telemetry_age",
        device_class=SensorDeviceClass.DURATION,
        native_unit_of_measurement=UnitOfTime.SECONDS,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda vehicle: vehicle.telemetry_age_seconds,
    ),
    VehicleSensorDescription(
        key="data_quality",
        translation_key="data_quality",
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda vehicle: vehicle.data_quality,
    ),
)


class TeslatlasHubSensor(TeslatlasCoordinatorEntity, SensorEntity):
    """One read-only Hub-level sensor."""

    entity_description: HubSensorDescription

    def __init__(
        self,
        entry: TeslatlasConfigEntry,
        description: HubSensorDescription,
    ) -> None:
        """Bind a stable Hub sensor to the push coordinator."""
        super().__init__(entry.runtime_data)
        self.entity_description = description
        info = self.coordinator.data.info
        self._attr_unique_id = f"{info.hub_id}_hub_{description.key}"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, info.hub_id)},
            manufacturer="Teslatlas",
            model="Teslatlas Hub",
            name=info.name,
        )

    @property
    @override
    def native_value(self) -> StateType:
        """Return the latest immutable Hub value."""
        return self.entity_description.value_fn(self.coordinator.data)


class TeslatlasVehicleSensor(TeslatlasCoordinatorEntity, SensorEntity):
    """One read-only sensor for a public vehicle projection."""

    entity_description: VehicleSensorDescription

    def __init__(
        self,
        entry: TeslatlasConfigEntry,
        vehicle_id: str,
        description: VehicleSensorDescription,
    ) -> None:
        """Bind a stable vehicle sensor to the push coordinator."""
        super().__init__(entry.runtime_data)
        self.entity_description = description
        self._vehicle_id = vehicle_id
        info = self.coordinator.data.info
        vehicle = self.coordinator.data.vehicles[vehicle_id]
        self._attr_unique_id = f"{info.hub_id}_{vehicle_id}_{description.key}"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, f"{info.hub_id}:{vehicle_id}")},
            manufacturer="Tesla",
            model="Vehicle",
            name=vehicle.name,
        )

    @property
    @override
    def available(self) -> bool:
        """Require both the stream and this vehicle projection."""
        return super().available and self._vehicle_id in self.coordinator.data.vehicles

    @property
    @override
    def native_value(self) -> StateType:
        """Return the latest immutable vehicle value."""
        vehicle = self.coordinator.data.vehicles.get(self._vehicle_id)
        if vehicle is None:
            return None
        return self.entity_description.value_fn(vehicle)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
    async_add_entities: AddConfigEntryEntitiesCallback,
) -> None:
    """Create sensors and add new vehicles observed through push."""
    coordinator = entry.runtime_data
    hub_added = False
    known_vehicle_ids: set[str] = set()

    @callback
    def async_add_new_entities() -> None:
        """Add each Hub or vehicle sensor set exactly once."""
        nonlocal hub_added
        entities: list[SensorEntity] = []
        if not hub_added:
            entities.extend(
                TeslatlasHubSensor(entry, description)
                for description in HUB_SENSOR_DESCRIPTIONS
            )
            hub_added = True

        for vehicle_id in coordinator.data.vehicles:
            if vehicle_id in known_vehicle_ids:
                continue
            entities.extend(
                TeslatlasVehicleSensor(entry, vehicle_id, description)
                for description in VEHICLE_SENSOR_DESCRIPTIONS
            )
            known_vehicle_ids.add(vehicle_id)

        if entities:
            async_add_entities(entities)

    async_add_new_entities()
    entry.async_on_unload(coordinator.async_add_listener(async_add_new_entities))
