// SPDX-License-Identifier: AGPL-3.0-only

//! Vehicle access for the paired-device routes: the grant check every
//! vehicle-scoped route runs, and the path parsing the sync routes share.

use super::*;
use crate::db::{VehicleScope, VehicleVisibility};

/// Why a vehicle-scoped route does not answer for a vehicle.
pub(super) enum VehicleAccessDenied {
    /// Unknown, retired, not visible at the route's visibility, nil, or not
    /// granted to the device. These are one answer by design: an ungranted
    /// vehicle must be indistinguishable from an unknown one.
    NotFound,
    /// The catalogue could not be read.
    Unavailable,
}

impl VehicleAccessDenied {
    /// The answer of a route that carries the structured public error.
    pub(super) fn into_public_api_response(self) -> Response {
        match self {
            Self::NotFound => vehicle_not_found(),
            Self::Unavailable => {
                service_unavailable("vehicle visibility is temporarily unavailable")
            }
        }
    }

    /// The answer of a route whose 404 and 503 carry no body.
    pub(super) fn into_status_response(self) -> Response {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND.into_response(),
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
}

impl AppState {
    /// The vehicles the authenticated principal of a request may see.
    ///
    /// Keyed on `require_device_auth == false`: that router has no device
    /// principal (`authorize_device` hands every route the nil-id principal),
    /// which is the production plain-loopback serve behind `LocalLoopbackAccess`
    /// as well as the test routers, and the nil principal is not a device that
    /// could hold grants, so it sees every vehicle. Any router that requires a
    /// bearer (the TLS listener) never bypasses: its principal is a paired
    /// device and only its grants count.
    pub(super) fn vehicle_scope(&self, device: &PairedDeviceRecord) -> VehicleScope {
        if self.require_device_auth {
            VehicleScope::Device(device.device_id)
        } else {
            VehicleScope::Unrestricted
        }
    }
}

/// The one vehicle check of every vehicle-scoped route, run after the bearer
/// and before the query is read: the vehicle exists, is visible at
/// `visibility`, and is granted to the device. One read-only query, so a
/// de-grant or a retirement applies to the next request.
pub(super) fn require_vehicle_access(
    state: &AppState,
    device: &PairedDeviceRecord,
    vehicle_id: Uuid,
    visibility: VehicleVisibility,
) -> Result<(), VehicleAccessDenied> {
    match state
        .store
        .vehicle_accessible(state.vehicle_scope(device), vehicle_id, visibility)
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(VehicleAccessDenied::NotFound),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot check public vehicle visibility");
            Err(VehicleAccessDenied::Unavailable)
        }
    }
}

/// The vehicle segment of a sync path, before any lookup.
pub(super) enum PathVehicle {
    /// Not a UUID, or not valid UTF-8 after percent-decoding.
    Malformed,
    /// The nil UUID: well-formed, and no vehicle can have it.
    Nil,
    Id(Uuid),
}

pub(super) fn path_vehicle(uri: &Uri) -> PathVehicle {
    match raw_path_segment(uri, VEHICLE_ID_SEGMENT).map(|value| Uuid::parse_str(&value)) {
        Some(Ok(vehicle_id)) if vehicle_id.is_nil() => PathVehicle::Nil,
        Some(Ok(vehicle_id)) => PathVehicle::Id(vehicle_id),
        Some(Err(_)) | None => PathVehicle::Malformed,
    }
}

/// The vehicle of manifest, no-op and prepared-artefacts: a malformed id is an
/// empty 404, the nil UUID (an unknown vehicle) the JSON `vehicle_not_found`.
#[allow(clippy::result_large_err)] // an axum Response is the handler error by design
pub(super) fn json_sync_route_vehicle(uri: &Uri) -> Result<Uuid, Response> {
    match path_vehicle(uri) {
        PathVehicle::Malformed => Err(StatusCode::NOT_FOUND.into_response()),
        PathVehicle::Nil => Err(vehicle_not_found()),
        PathVehicle::Id(vehicle_id) => Ok(vehicle_id),
    }
}
