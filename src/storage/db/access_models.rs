// SPDX-License-Identifier: AGPL-3.0-only

/// The only reverse-geocoding provider family the Hub speaks to.
pub const ADDRESS_LOOKUP_PROVIDER: &str = "nominatim";
const MAX_ADDRESS_ENDPOINT_BYTES: usize = 2048;
const MAX_ADDRESS_HOST_BYTES: usize = 255;
const MAX_ADDRESS_LANGUAGE_BYTES: usize = 64;
/// Longest provider licence text kept with a lookup row.
pub const MAX_ADDRESS_LICENCE_BYTES: usize = 512;

/// What a geocoder lookup is bound to besides its coordinates: the provider,
/// the exact endpoint it was sent to and the requested language. Two lookups
/// share a cache row only when all of these and the rounded coordinates agree,
/// so a changed endpoint or language is a miss, never someone else's answer.
///
/// `endpoint` is the normalised request base `scheme://host:port/path` (default
/// port made explicit, no user information, query or fragment); `endpoint_host`
/// is its host, kept for provenance. Build one with [`Self::nominatim`]; a
/// hand-assembled value that is not what that constructor would produce is
/// refused when it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressLookupBinding {
    pub provider: String,
    pub endpoint: String,
    pub endpoint_host: String,
    pub language: String,
}

impl AddressLookupBinding {
    pub fn nominatim(endpoint: &url::Url, language: &str) -> Result<Self, StoreError> {
        let host = endpoint
            .host_str()
            .filter(|host| !host.is_empty() && host.len() <= MAX_ADDRESS_HOST_BYTES);
        let port = endpoint.port_or_known_default();
        let (Some(host), Some(port)) = (host, port) else {
            return Err(StoreError::InvalidAddressCache);
        };
        if !matches!(endpoint.scheme(), "http" | "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || language.is_empty()
            || language.len() > MAX_ADDRESS_LANGUAGE_BYTES
            || language.chars().any(char::is_control)
        {
            return Err(StoreError::InvalidAddressCache);
        }
        let normalised = format!(
            "{}://{}:{}{}",
            endpoint.scheme(),
            host,
            port,
            endpoint.path()
        );
        if normalised.len() > MAX_ADDRESS_ENDPOINT_BYTES {
            return Err(StoreError::InvalidAddressCache);
        }
        Ok(Self {
            provider: ADDRESS_LOOKUP_PROVIDER.to_owned(),
            endpoint: normalised,
            endpoint_host: host.to_owned(),
            language: language.to_owned(),
        })
    }

    fn is_canonical(&self) -> bool {
        url::Url::parse(&self.endpoint)
            .ok()
            .and_then(|endpoint| Self::nominatim(&endpoint, &self.language).ok())
            .is_some_and(|canonical| canonical == *self)
    }
}

/// One geocoder lookup as stored: the coordinates asked for, the binding it
/// was asked under, and the complete record the provider returned for it. The
/// record is the lookup's own: a later lookup that reaches the same OSM
/// identity with another name does not change it.
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
    pub binding: AddressLookupBinding,
    /// The provider's `licence` member, when it sent a printable one.
    pub licence: Option<String>,
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

/// The SHA-256 (lowercase hex) of a terrain tile exactly as the Hub downloaded
/// and stored it. A cached `.hgt` file is trusted only while its digest equals
/// this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainTileDigest {
    pub sha256: String,
    pub recorded_at_ms: i64,
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

    /// Wrap a bearer a client presented, after it passed the wire check, so a
    /// rotation can hand the same credential back.
    fn from_wire(value: &str) -> Option<Self> {
        DeviceAccessToken::digest_from_wire(value).map(|_| Self(value.to_owned()))
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

/// The only credential-bearing result of a successful pairing claim or
/// rotation.
#[derive(Debug, PartialEq, Eq)]
pub struct PairedDeviceAccess {
    pub device_id: Uuid,
    pub access_token: DeviceAccessToken,
    pub expires_at_ms: i64,
    /// Set only by a rotation at the device's absolute maximum lifetime: the
    /// token is the one that was presented, nothing was issued or written,
    /// and the client should ask the owner to pair again before
    /// `expires_at_ms`.
    pub at_maximum_lifetime: bool,
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
