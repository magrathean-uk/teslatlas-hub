// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Clone, PartialEq)]
pub struct AddressCacheRecord {
    pub osm_type: String,
    pub osm_id: i64,
    pub display_name: String,
    pub name: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub house_number: Option<String>,
    pub road: Option<String>,
    pub neighbourhood: Option<String>,
    pub city: Option<String>,
    pub county: Option<String>,
    pub postcode: Option<String>,
    pub state: Option<String>,
    pub state_district: Option<String>,
    pub country: Option<String>,
    pub raw_json: Option<String>,
    pub lookup_latitude: f64,
    pub lookup_longitude: f64,
    pub looked_up_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddressEnrichmentJob {
    pub job_key: String,
    pub vehicle_id: Uuid,
    pub target_type: String,
    pub target_id: i64,
    pub field: String,
    pub latitude: f64,
    pub longitude: f64,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressEnrichmentCompletion {
    pub vehicle_id: Uuid,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerrainCandidate {
    pub vehicle_id: Uuid,
    pub position: ProjectionPosition,
}

/// One opaque, single-use pairing invitation. The secret is intentionally not
/// `Debug` or `Display`; it is safe only for a local terminal or a QR payload.
#[derive(PartialEq, Eq)]
pub struct PairingInvitation {
    pub pairing_id: Uuid,
    secret: PairingSecret,
    created_at_ms: i64,
    pub expires_at_ms: i64,
}

impl PairingInvitation {
    pub fn secret(&self) -> &str {
        self.secret.as_wire()
    }
}

impl std::fmt::Debug for PairingInvitation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingInvitation")
            .field("pairing_id", &self.pairing_id)
            .field("secret", &"[redacted]")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(PartialEq, Eq, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
struct PairingSecret(String);

impl PairingSecret {
    fn generate() -> Result<Self, StoreError> {
        Ok(Self(random_secret_wire()?))
    }

    fn as_wire(&self) -> &str {
        &self.0
    }

    fn digest(&self) -> [u8; PAIRING_SECRET_BYTES] {
        sha256_bytes(self.0.as_bytes())
    }

    fn digest_from_wire(value: &str) -> Option<[u8; PAIRING_SECRET_BYTES]> {
        digest_valid_wire_secret(value)
    }
}

/// A paired device's bearer token. It is returned once at claim time and is
/// stored only as a hash in the Hub database. It is intentionally not
/// cloneable.
///
/// ```compile_fail
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<teslatlas_hub::db::DeviceAccessToken>();
/// ```
#[derive(PartialEq, Eq, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct DeviceAccessToken(String);

impl DeviceAccessToken {
    fn generate() -> Result<Self, StoreError> {
        Ok(Self(random_secret_wire()?))
    }

    pub fn as_bearer(&self) -> &str {
        &self.0
    }

    /// Move the bearer into an explicitly zeroizing response buffer without
    /// making an ordinary secret copy.
    pub(crate) fn take_bearer(&mut self) -> Zeroizing<String> {
        Zeroizing::new(std::mem::take(&mut self.0))
    }

    fn digest(&self) -> [u8; ACCESS_TOKEN_BYTES] {
        sha256_bytes(self.0.as_bytes())
    }

    fn digest_from_wire(value: &str) -> Option<[u8; ACCESS_TOKEN_BYTES]> {
        digest_valid_wire_secret(value)
    }
}

impl std::fmt::Debug for DeviceAccessToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DeviceAccessToken([redacted])")
    }
}

/// The only credential-bearing result of a successful pairing claim.
#[derive(Debug, PartialEq, Eq)]
pub struct PairedDeviceAccess {
    pub device_id: Uuid,
    pub access_token: DeviceAccessToken,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairedDeviceRecord {
    pub device_id: Uuid,
    pub display_name: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
    pub last_authenticated_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PublishedVehicle {
    pub vehicle_id: Uuid,
    pub display_name: Option<String>,
}
