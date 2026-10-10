// SPDX-License-Identifier: AGPL-3.0-only

// Per-device vehicle grants (schema v68). A paired device sees a vehicle only
// when `device_vehicle_grants` holds a row for the pair; every vehicle-scoped
// lookup resolves that inside its own SQL, so an ungranted vehicle or pack is
// indistinguishable from an unknown one.

/// Which vehicles one request may see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VehicleScope {
    /// No device principal: the plain-loopback owner router (and the owner's
    /// own tools) see every vehicle. The HTTP layer picks this only when
    /// `require_device_auth` is false.
    Unrestricted,
    /// One paired device: only the vehicles it holds a grant for.
    Device(Uuid),
}

impl VehicleScope {
    /// The device bound into a grant predicate. `None` binds SQL NULL, which
    /// makes `(?n IS NULL OR EXISTS (...))` pass for the unrestricted scope.
    pub(crate) fn grant_device_param(self) -> Option<String> {
        match self {
            Self::Unrestricted => None,
            Self::Device(device_id) => Some(device_id.to_string()),
        }
    }
}

/// How visible a vehicle must be for a route to answer for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VehicleVisibility {
    /// Not retired. The sync routes: a vehicle without a published full
    /// snapshot is still answered (as `no_full_snapshot`), not as unknown.
    Active,
    /// Not retired and published (it has a sync manifest): the predicate of
    /// `/v1/vehicles`, `/current` and the history routes.
    PublishedAndActive,
}

/// The vehicles an invitation grants to the device that claims it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingScope {
    /// Every vehicle that is active when the invitation is claimed.
    AllVehicles,
    /// Exactly these vehicles (those still active at claim time).
    Vehicles(Vec<Uuid>),
}

/// One paired device with the vehicles it is granted, for `control
/// paired-devices`. `PairedDeviceRecord` (the per-request authentication
/// result) stays free of the list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PairedDeviceListing {
    #[serde(flatten)]
    pub device: PairedDeviceRecord,
    /// Granted vehicle ids, sorted. A retired vehicle stays listed: the grant
    /// outlives retirement and applies again if the vehicle is reactivated.
    pub vehicles: Vec<Uuid>,
}

impl HubStore {
    /// Whether `vehicle_id` may be served in `scope` at `visibility`: one
    /// read-only query, never cached, so a de-grant or a retirement takes
    /// effect on the next request. The nil UUID is a well-formed id no vehicle
    /// can have, so it is simply not accessible (never an error).
    pub fn vehicle_accessible(
        &self,
        scope: VehicleScope,
        vehicle_id: Uuid,
        visibility: VehicleVisibility,
    ) -> Result<bool, StoreError> {
        if vehicle_id.is_nil() {
            return Ok(false);
        }
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM vehicles AS vehicle
                     WHERE vehicle.vehicle_id = ?1 AND vehicle.retired_at_ms IS NULL
                       AND (?2 = 0 OR EXISTS (SELECT 1 FROM sync_manifests AS manifest
                                               WHERE manifest.vehicle_id = vehicle.vehicle_id))
                       AND (?3 IS NULL OR EXISTS (SELECT 1 FROM device_vehicle_grants AS grants
                                                   WHERE grants.device_id = ?3
                                                     AND grants.vehicle_id = vehicle.vehicle_id)))",
                params![
                    vehicle_id.to_string(),
                    matches!(visibility, VehicleVisibility::PublishedAndActive),
                    scope.grant_device_param(),
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    /// Check that every named vehicle exists and is active, and return each
    /// one's name for the pairing presentation. The first one that is nil,
    /// unknown or retired is [`StoreError::PairingVehicleUnavailable`], so the
    /// CLI can refuse before anything is persisted. A vehicle without a sync
    /// manifest is accepted. `persist_pairing_with_scope` repeats the
    /// retirement check inside its own transaction.
    pub fn pairing_scope_vehicles(
        &self,
        vehicles: &[Uuid],
    ) -> Result<Vec<PublishedVehicle>, StoreError> {
        if vehicles.is_empty() {
            return Err(StoreError::EmptyPairingScope);
        }
        let connection = self.open_read_only_connection()?;
        let mut resolved: Vec<PublishedVehicle> = Vec::with_capacity(vehicles.len());
        for &vehicle_id in vehicles {
            if resolved
                .iter()
                .any(|vehicle| vehicle.vehicle_id == vehicle_id)
            {
                continue;
            }
            let display_name: Option<Option<String>> = connection
                .query_row(
                    "SELECT display_name FROM vehicles
                      WHERE vehicle_id = ?1 AND retired_at_ms IS NULL",
                    params![vehicle_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(StoreError::Query)?;
            match display_name {
                Some(display_name) if !vehicle_id.is_nil() => resolved.push(PublishedVehicle {
                    vehicle_id,
                    display_name,
                }),
                _ => return Err(StoreError::PairingVehicleUnavailable(vehicle_id)),
            }
        }
        Ok(resolved)
    }

    /// Every paired device with its granted vehicle ids (sorted), oldest
    /// device first. Both tables are read in one read transaction, so a claim
    /// that commits meanwhile cannot list a device with no vehicles.
    pub fn list_paired_devices_with_vehicles(
        &self,
    ) -> Result<Vec<PairedDeviceListing>, StoreError> {
        let connection = self.open_read_only_connection()?;
        let connection = connection
            .unchecked_transaction()
            .map_err(StoreError::Query)?;
        let mut grants = connection
            .prepare("SELECT device_id, vehicle_id FROM device_vehicle_grants ORDER BY vehicle_id")
            .map_err(StoreError::Query)?;
        let mut by_device: std::collections::HashMap<String, Vec<Uuid>> =
            std::collections::HashMap::new();
        let rows = grants
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(StoreError::Query)?;
        for row in rows {
            let (device_id, vehicle_id) = row.map_err(StoreError::Query)?;
            let vehicle_id = Uuid::parse_str(&vehicle_id).map_err(|error| {
                StoreError::Query(rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                ))
            })?;
            by_device.entry(device_id).or_default().push(vehicle_id);
        }
        let mut statement = connection
            .prepare(
                "SELECT device_id, display_name, created_at_ms, expires_at_ms, revoked_at_ms, last_authenticated_at_ms
                 FROM paired_devices ORDER BY created_at_ms ASC, device_id ASC",
            )
            .map_err(StoreError::Query)?;
        let devices = statement
            .query_map([], paired_device_from_row)
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        Ok(devices
            .into_iter()
            .map(|device| {
                let vehicles = by_device
                    .remove(&device.device_id.to_string())
                    .unwrap_or_default();
                PairedDeviceListing { device, vehicles }
            })
            .collect())
    }
}
