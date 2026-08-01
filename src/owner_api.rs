//! Deliberate, one-shot compatibility reads from a Tesla Owner API endpoint.
//!
//! This is not a polling loop, a Fleet implementation, or a command client.
//! It only sends authenticated `GET` requests to the legacy-compatible product list and
//! crate-local `vehicle_data` paths. The collector owns the no-wake stream-power
//! confirmation contract; this module exposes no public manual collection shortcut.
//!
//! The owner token is a [`crate::credentials::OwnerToken`], which can only be
//! loaded from the service credential module. It is never accepted as a URL,
//! configuration string, environment value, or request query parameter.

use std::{
    fmt,
    time::{Duration, SystemTime},
};

use futures_util::StreamExt;
use reqwest::{
    Client,
    header::{ACCEPT, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    credentials::{LegacyAuthManager, LegacyAuthManagerError, OwnerToken},
    hub_pack::ProjectionCarSettings,
    legacy_auth::LegacyAuthFuse,
    tesla_stream::{StreamEvent, StreamRegion, TeslaStreamSupervisor},
};

/// Four MiB is comfortably above a normal vehicle-data response while keeping
/// a bad upstream response from turning a manual collection into an unbounded
/// allocation.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const ACCEPT_JSON: HeaderValue = HeaderValue::from_static("application/json");
const VEHICLE_DATA_ENDPOINTS: &str = "charge_state;climate_state;closures_state;drive_state;gui_settings;location_data;vehicle_config;vehicle_state;vehicle_data_combo";

/// A validated, explicit HTTPS Owner API base URL.
///
/// There is intentionally no implicit production endpoint. The operator must
/// select a base URL during future collector configuration, making the legacy
/// compatibility boundary visible and reversible.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnerApiBase {
    url: Url,
}

impl OwnerApiBase {
    pub fn parse(value: &str) -> Result<Self, OwnerApiConfigError> {
        let url = Url::parse(value).map_err(|_| OwnerApiConfigError::InvalidBaseUrl)?;
        Self::from_url(url, true)
    }

    fn from_url(mut url: Url, require_https: bool) -> Result<Self, OwnerApiConfigError> {
        if require_https && url.scheme() != "https" {
            return Err(OwnerApiConfigError::HttpsRequired);
        }
        if !require_https && !matches!(url.scheme(), "http" | "https") {
            return Err(OwnerApiConfigError::UnsupportedBaseScheme);
        }
        if url.cannot_be_a_base() || url.host_str().is_none() {
            return Err(OwnerApiConfigError::BaseHostRequired);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(OwnerApiConfigError::EmbeddedBaseCredential);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(OwnerApiConfigError::BaseParametersNotPermitted);
        }
        if url
            .path_segments()
            .is_some_and(|mut segments| segments.any(|segment| segment == ".."))
        {
            return Err(OwnerApiConfigError::BasePathTraversal);
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }

        Ok(Self { url })
    }

    fn endpoint(&self, suffix: &str) -> Result<Url, OwnerApiError> {
        self.url
            .join(suffix)
            .map_err(|_| OwnerApiError::InvalidEndpoint)
    }

    pub fn stream_region(&self) -> Option<StreamRegion> {
        let host = self.url.host_str()?.to_ascii_lowercase();
        if host == "auth.tesla.cn"
            || host.ends_with(".tesla.cn")
            || host.ends_with(".cloud.tesla.cn")
        {
            Some(StreamRegion::China)
        } else if host == "auth.tesla.com"
            || host.ends_with(".tesla.com")
            || host.ends_with(".teslamotors.com")
        {
            Some(StreamRegion::Global)
        } else {
            None
        }
    }
}

impl fmt::Debug for OwnerApiBase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("OwnerApiBase")
            .field(&self.url.as_str())
            .finish()
    }
}

/// Construction-only settings for a manually invoked compatibility read.
#[derive(Clone, Debug)]
pub struct OwnerApiOptions {
    pub base_url: OwnerApiBase,
    pub request_timeout: Duration,
}

impl OwnerApiOptions {
    pub fn new(base_url: OwnerApiBase, request_timeout: Duration) -> Self {
        Self {
            base_url,
            request_timeout,
        }
    }
}

/// A narrowly scoped, read-only Owner API client.
#[derive(Clone)]
pub struct OwnerApi {
    client: Client,
    base_url: OwnerApiBase,
}

/// The only production capability for issuing an Owner API request. It carries
/// no request material: just the durable audit store and a collection-run UUID.
/// A failed ledger write is intentionally an API error, so callers fail closed
/// before making an unaudited network request.
pub(crate) struct OwnerApiRequestAudit<'a> {
    store: &'a crate::db::HubStore,
    correlation_id: Uuid,
}

struct HubLegacyRefreshAudit {
    store: crate::db::HubStore,
    correlation_id: Uuid,
}

impl crate::legacy_auth::LegacyRefreshAuditSink for HubLegacyRefreshAudit {
    fn begin_token_refresh(
        &self,
    ) -> Result<crate::legacy_auth::LegacyRefreshAuditReceipt, crate::legacy_auth::LegacyAuthError>
    {
        let receipt = self
            .store
            .begin_outbound_request(&crate::db::OutboundRequestStart {
                correlation_id: self.correlation_id,
                vehicle_tesla_id: None,
                transport: crate::db::OutboundRequestTransport::LegacyAuth,
                operation: crate::db::OutboundRequestOperation::TokenRefresh,
                safety_class: crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                precondition: crate::db::OutboundRequestPrecondition::NotRequired,
            })
            .map_err(|_| crate::legacy_auth::LegacyAuthError::AuditUnavailable)?;
        Ok(crate::legacy_auth::LegacyRefreshAuditReceipt(receipt.0))
    }

    fn complete_token_refresh(
        &self,
        receipt: crate::legacy_auth::LegacyRefreshAuditReceipt,
        outcome: crate::legacy_auth::LegacyRefreshAuditOutcome,
    ) -> Result<(), crate::legacy_auth::LegacyAuthError> {
        let (outcome, http_status) = match outcome {
            crate::legacy_auth::LegacyRefreshAuditOutcome::Success => {
                (crate::db::OutboundRequestOutcome::Success, None)
            }
            crate::legacy_auth::LegacyRefreshAuditOutcome::HttpError(status) => {
                (crate::db::OutboundRequestOutcome::HttpError, Some(status))
            }
            crate::legacy_auth::LegacyRefreshAuditOutcome::AuthenticationRejected => {
                (crate::db::OutboundRequestOutcome::AuthenticationRejected, Some(401))
            }
            crate::legacy_auth::LegacyRefreshAuditOutcome::TransportError => {
                (crate::db::OutboundRequestOutcome::TransportError, None)
            }
            crate::legacy_auth::LegacyRefreshAuditOutcome::ResponseTooLarge => {
                (crate::db::OutboundRequestOutcome::ResponseTooLarge, None)
            }
            crate::legacy_auth::LegacyRefreshAuditOutcome::ProtocolError => {
                (crate::db::OutboundRequestOutcome::ProtocolError, None)
            }
        };
        self.store
            .complete_outbound_request(
                crate::db::OutboundRequestReceiptId(receipt.0),
                &crate::db::OutboundRequestCompletion {
                    outcome,
                    http_status,
                },
            )
            .map_err(|_| crate::legacy_auth::LegacyAuthError::AuditUnavailable)
    }
}

impl<'a> OwnerApiRequestAudit<'a> {
    pub(crate) fn new(store: &'a crate::db::HubStore, correlation_id: Uuid) -> Self {
        Self {
            store,
            correlation_id,
        }
    }

    fn begin(
        &self,
        vehicle_id: Option<VehicleId>,
        operation: crate::db::OutboundRequestOperation,
        safety_class: crate::db::OutboundRequestSafetyClass,
        precondition: crate::db::OutboundRequestPrecondition,
    ) -> Result<crate::db::OutboundRequestReceiptId, OwnerApiError> {
        let vehicle_tesla_id = vehicle_id
            .map(|id| i64::try_from(id.get()).map_err(|_| OwnerApiError::RequestAudit))
            .transpose()?;
        self.store
            .begin_outbound_request(&crate::db::OutboundRequestStart {
                correlation_id: self.correlation_id,
                vehicle_tesla_id,
                transport: crate::db::OutboundRequestTransport::OwnerApi,
                operation,
                safety_class,
                precondition,
            })
            .map_err(|_| OwnerApiError::RequestAudit)
    }

    fn complete<T>(
        &self,
        receipt_id: crate::db::OutboundRequestReceiptId,
        result: &Result<T, OwnerApiError>,
    ) -> Result<(), OwnerApiError> {
        let (outcome, http_status) = match result {
            Ok(_) => (crate::db::OutboundRequestOutcome::Success, None),
            Err(OwnerApiError::RequestTimeout) => (crate::db::OutboundRequestOutcome::Timeout, None),
            Err(OwnerApiError::Transport | OwnerApiError::ResponseRead) => {
                (crate::db::OutboundRequestOutcome::TransportError, None)
            }
            Err(OwnerApiError::ResponseTooLarge) => {
                (crate::db::OutboundRequestOutcome::ResponseTooLarge, None)
            }
            Err(OwnerApiError::HttpStatus(status)) => {
                if *status == 401 {
                    (crate::db::OutboundRequestOutcome::AuthenticationRejected, Some(*status))
                } else {
                    (crate::db::OutboundRequestOutcome::HttpError, Some(*status))
                }
            }
            Err(OwnerApiError::RateLimited { .. }) => {
                (crate::db::OutboundRequestOutcome::HttpError, Some(429))
            }
            Err(OwnerApiError::VehicleNotFound) => {
                (crate::db::OutboundRequestOutcome::HttpError, Some(404))
            }
            Err(OwnerApiError::VehicleInService) => {
                (crate::db::OutboundRequestOutcome::HttpError, Some(405))
            }
            Err(_) => (crate::db::OutboundRequestOutcome::ProtocolError, None),
        };
        self.store
            .complete_outbound_request(
                receipt_id,
                &crate::db::OutboundRequestCompletion {
                    outcome,
                    http_status,
                },
            )
            .map_err(|_| OwnerApiError::RequestAudit)
    }

    fn legacy_refresh_context(&self) -> crate::legacy_auth::LegacyRefreshAuditContext {
        crate::legacy_auth::LegacyRefreshAuditContext::new(std::sync::Arc::new(
            HubLegacyRefreshAudit {
                store: self.store.clone(),
                correlation_id: self.correlation_id,
            },
        ))
    }
}

impl OwnerApi {
    pub fn new(options: OwnerApiOptions) -> Result<Self, OwnerApiConfigError> {
        Self::build(options, false)
    }

    /// Build a stream supervisor from the same credential boundary as Owner
    /// API reads. The token is held only in memory by the supervisor.
    pub fn stream_supervisor(
        &self,
        vehicle_id: VehicleId,
        token: OwnerToken,
        region: StreamRegion,
        endpoint: String,
        events: tokio::sync::mpsc::Sender<StreamEvent>,
    ) -> Result<TeslaStreamSupervisor, crate::tesla_stream::StreamSupervisorError> {
        TeslaStreamSupervisor::new(vehicle_id, token, region, endpoint, events)
    }

    pub(crate) fn http_client(&self) -> Client {
        self.client.clone()
    }

    fn build(
        options: OwnerApiOptions,
        allow_insecure_test_base: bool,
    ) -> Result<Self, OwnerApiConfigError> {
        if options.request_timeout.is_zero() {
            return Err(OwnerApiConfigError::ZeroTimeout);
        }

        // Owner API construction is also used by standalone collection tests
        // and commands, so install the one Hub TLS provider at this boundary
        // instead of relying on the serving or PostgreSQL path to run first.
        crate::crypto::install_default_provider();
        let client = Client::builder()
            .https_only(!allow_insecure_test_base)
            .redirect(Policy::none())
            .timeout(options.request_timeout)
            .build()
            .map_err(|_| OwnerApiConfigError::ClientBuild)?;

        Ok(Self {
            client,
            base_url: options.base_url,
        })
    }

    /// Discover account vehicles. This is a GET-only request and does not
    /// wake a vehicle.
    #[cfg(test)]
    pub async fn list_vehicles(&self, token: &OwnerToken) -> Result<Vec<Vehicle>, OwnerApiError> {
        // Owner-token compatibility follows the current TeslaMate behavior:
        // discovery comes from `/products`. Fleet-specific `/vehicles` is not
        // silently substituted here.
        let envelope: ResponseEnvelope<Vec<ProductWire>> =
            self.get_envelope(token, "api/1/products").await?;

        if let Some(count) = envelope.count
            && count != envelope.response.len()
        {
            return Err(OwnerApiError::InvalidVehicleListCount);
        }

        parse_vehicle_list(envelope)
    }

    pub(crate) async fn list_vehicles_audited(
        &self,
        token: &OwnerToken,
        audit: &OwnerApiRequestAudit<'_>,
    ) -> Result<Vec<Vehicle>, OwnerApiError> {
        let envelope: ResponseEnvelope<Vec<ProductWire>> = self
            .get_envelope_audited(
                token.as_str(),
                self.base_url.endpoint("api/1/products")?,
                audit,
                None,
                crate::db::OutboundRequestOperation::Products,
                crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                crate::db::OutboundRequestPrecondition::NotRequired,
            )
            .await?;
        parse_vehicle_list(envelope)
    }

    /// Legacy owner-authenticated discovery. A single HTTP 401 causes one
    /// refresh and one retry; the retry never loops.
    #[cfg(test)]
    pub async fn list_vehicles_with_legacy_auth(
        &self,
        auth: &mut LegacyAuthManager,
    ) -> Result<Vec<Vehicle>, OwnerApiAuthError> {
        let mut fuse = LegacyAuthFuse::default();
        self.list_vehicles_with_legacy_auth_fused(auth, &mut fuse)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn list_vehicles_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
    ) -> Result<Vec<Vehicle>, OwnerApiAuthError> {
        let envelope: ResponseEnvelope<Vec<ProductWire>> = self
            .get_envelope_with_legacy_auth_fused(auth, fuse, "api/1/products")
            .await?;
        parse_vehicle_list(envelope).map_err(OwnerApiAuthError::Owner)
    }

    pub(crate) async fn list_vehicles_with_legacy_auth_fused_audited(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        audit: &OwnerApiRequestAudit<'_>,
    ) -> Result<Vec<Vehicle>, OwnerApiAuthError> {
        let endpoint = self
            .base_url
            .endpoint("api/1/products")
            .map_err(OwnerApiAuthError::Owner)?;
        let envelope: ResponseEnvelope<Vec<ProductWire>> = self
            .get_envelope_with_legacy_auth_url_fused_audited(
                auth,
                fuse,
                endpoint,
                audit,
                None,
                crate::db::OutboundRequestOperation::Products,
                crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                crate::db::OutboundRequestPrecondition::NotRequired,
            )
            .await?;
        parse_vehicle_list(envelope).map_err(OwnerApiAuthError::Owner)
    }

    /// Fetch one vehicle's reported state after the crate-local collector has
    /// established bounded numeric stream-power proof. This stays crate-local
    /// so external callers cannot turn this compatibility client into an
    /// arbitrary wake/poll mechanism.
    #[cfg(test)]
    pub(crate) async fn vehicle_data(
        &self,
        token: &OwnerToken,
        vehicle_id: VehicleId,
    ) -> Result<VehicleData, OwnerApiError> {
        let endpoint = self.vehicle_data_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> =
            self.get_envelope_url(token.as_str(), endpoint).await?;

        if envelope.count.is_some() || envelope.response.is_empty() {
            return Err(OwnerApiError::InvalidVehicleDataEnvelope);
        }
        let mut fields = envelope.response;
        scrub_sensitive_fields(&mut fields);
        if fields.is_empty() {
            return Err(OwnerApiError::SensitiveDataInResponse);
        }

        Ok(VehicleData { vehicle_id, fields })
    }

    pub(crate) async fn vehicle_data_audited(
        &self,
        token: &OwnerToken,
        vehicle_id: VehicleId,
        audit: &OwnerApiRequestAudit<'_>,
    ) -> Result<VehicleData, OwnerApiError> {
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_audited(
                token.as_str(),
                self.vehicle_data_endpoint(vehicle_id)?,
                audit,
                Some(vehicle_id),
                crate::db::OutboundRequestOperation::VehicleData,
                crate::db::OutboundRequestSafetyClass::ConditionalRead,
                crate::db::OutboundRequestPrecondition::StreamPowerConfirmed,
            )
            .await?;
        parse_vehicle_data(vehicle_id, envelope)
    }

    #[cfg(test)]
    pub(crate) async fn vehicle_data_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
    ) -> Result<VehicleData, OwnerApiAuthError> {
        let endpoint = self.vehicle_data_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused(auth, fuse, endpoint)
            .await?;
        parse_vehicle_data(vehicle_id, envelope).map_err(OwnerApiAuthError::Owner)
    }

    pub(crate) async fn vehicle_data_with_legacy_auth_fused_audited(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
        audit: &OwnerApiRequestAudit<'_>,
    ) -> Result<VehicleData, OwnerApiAuthError> {
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused_audited(
                auth,
                fuse,
                self.vehicle_data_endpoint(vehicle_id)
                    .map_err(OwnerApiAuthError::Owner)?,
                audit,
                Some(vehicle_id),
                crate::db::OutboundRequestOperation::VehicleData,
                crate::db::OutboundRequestSafetyClass::ConditionalRead,
                crate::db::OutboundRequestPrecondition::StreamPowerConfirmed,
            )
            .await?;
        parse_vehicle_data(vehicle_id, envelope).map_err(OwnerApiAuthError::Owner)
    }

    #[cfg(test)]
    pub(crate) async fn vehicle_probe(
        &self,
        token: &OwnerToken,
        vehicle_id: VehicleId,
    ) -> Result<bool, OwnerApiError> {
        let endpoint = self.vehicle_probe_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> =
            self.get_envelope_url(token.as_str(), endpoint).await?;
        Ok(envelope
            .response
            .get("in_service")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub(crate) async fn vehicle_probe_audited(
        &self,
        token: &OwnerToken,
        vehicle_id: VehicleId,
        audit: &OwnerApiRequestAudit<'_>,
    ) -> Result<bool, OwnerApiError> {
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_audited(
                token.as_str(),
                self.vehicle_probe_endpoint(vehicle_id)?,
                audit,
                Some(vehicle_id),
                crate::db::OutboundRequestOperation::VehicleProbe,
                crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                crate::db::OutboundRequestPrecondition::NotRequired,
            )
            .await?;
        Ok(envelope
            .response
            .get("in_service")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    #[cfg(test)]
    pub(crate) async fn vehicle_probe_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
    ) -> Result<bool, OwnerApiAuthError> {
        let endpoint = self.vehicle_probe_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused(auth, fuse, endpoint)
            .await?;
        Ok(envelope
            .response
            .get("in_service")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    pub(crate) async fn vehicle_probe_with_legacy_auth_fused_audited(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
        audit: &OwnerApiRequestAudit<'_>,
    ) -> Result<bool, OwnerApiAuthError> {
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused_audited(
                auth,
                fuse,
                self.vehicle_probe_endpoint(vehicle_id)
                    .map_err(OwnerApiAuthError::Owner)?,
                audit,
                Some(vehicle_id),
                crate::db::OutboundRequestOperation::VehicleProbe,
                crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                crate::db::OutboundRequestPrecondition::NotRequired,
            )
            .await?;
        Ok(envelope
            .response
            .get("in_service")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// Fetch only the vehicle's current state. This endpoint is read-only and
    /// must not be replaced with `vehicle_data`, which may wake a car.
    #[cfg(test)]
    pub(crate) async fn vehicle_state(
        &self,
        token: &OwnerToken,
        vehicle_id: VehicleId,
    ) -> Result<String, OwnerApiError> {
        let endpoint = self.vehicle_probe_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> =
            self.get_envelope_url(token.as_str(), endpoint).await?;
        parse_vehicle_state(envelope.response)
    }

    pub(crate) async fn vehicle_state_audited(
        &self,
        token: &OwnerToken,
        vehicle_id: VehicleId,
        audit: &OwnerApiRequestAudit<'_>,
    ) -> Result<String, OwnerApiError> {
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_audited(
                token.as_str(),
                self.vehicle_probe_endpoint(vehicle_id)?,
                audit,
                Some(vehicle_id),
                crate::db::OutboundRequestOperation::VehicleProbe,
                crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                crate::db::OutboundRequestPrecondition::NotRequired,
            )
            .await?;
        parse_vehicle_state(envelope.response)
    }

    #[cfg(test)]
    pub(crate) async fn vehicle_state_with_legacy_auth_fused(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
    ) -> Result<String, OwnerApiAuthError> {
        let endpoint = self.vehicle_probe_endpoint(vehicle_id)?;
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused(auth, fuse, endpoint)
            .await?;
        parse_vehicle_state(envelope.response).map_err(OwnerApiAuthError::Owner)
    }

    pub(crate) async fn vehicle_state_with_legacy_auth_fused_audited(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        vehicle_id: VehicleId,
        audit: &OwnerApiRequestAudit<'_>,
    ) -> Result<String, OwnerApiAuthError> {
        let envelope: ResponseEnvelope<Map<String, Value>> = self
            .get_envelope_with_legacy_auth_url_fused_audited(
                auth,
                fuse,
                self.vehicle_probe_endpoint(vehicle_id)
                    .map_err(OwnerApiAuthError::Owner)?,
                audit,
                Some(vehicle_id),
                crate::db::OutboundRequestOperation::VehicleProbe,
                crate::db::OutboundRequestSafetyClass::NonWakeEndpoint,
                crate::db::OutboundRequestPrecondition::NotRequired,
            )
            .await?;
        parse_vehicle_state(envelope.response).map_err(OwnerApiAuthError::Owner)
    }

    fn vehicle_data_endpoint(&self, vehicle_id: VehicleId) -> Result<Url, OwnerApiError> {
        let suffix = format!("api/1/vehicles/{vehicle_id}/vehicle_data");
        let mut endpoint = self.base_url.endpoint(&suffix)?;
        endpoint
            .query_pairs_mut()
            .append_pair("endpoints", VEHICLE_DATA_ENDPOINTS);
        Ok(endpoint)
    }

    fn vehicle_probe_endpoint(&self, vehicle_id: VehicleId) -> Result<Url, OwnerApiError> {
        self.base_url
            .endpoint(&format!("api/1/vehicles/{vehicle_id}"))
    }

    async fn get_envelope_with_legacy_auth_url_fused_audited<T>(
        &self,
        auth: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        endpoint: Url,
        audit: &OwnerApiRequestAudit<'_>,
        vehicle_id: Option<VehicleId>,
        operation: crate::db::OutboundRequestOperation,
        safety_class: crate::db::OutboundRequestSafetyClass,
        precondition: crate::db::OutboundRequestPrecondition,
    ) -> Result<T, OwnerApiAuthError>
    where
        T: DeserializeOwned,
    {
        if fuse.is_blown() {
            return Err(OwnerApiAuthError::NotSignedIn);
        }
        let first = self
            .get_envelope_url_audited(
                auth.access_token(),
                endpoint.clone(),
                audit,
                vehicle_id,
                operation,
                safety_class,
                precondition,
            )
            .await;
        if !matches!(first, Err(OwnerApiError::HttpStatus(401))) {
            return first.map_err(OwnerApiAuthError::Owner);
        }
        let now = SystemTime::now();
        fuse.record_unauthorized(now);
        if fuse.is_blown() {
            return Err(OwnerApiAuthError::NotSignedIn);
        }
        let refresh = crate::legacy_auth::with_legacy_refresh_audit(
            audit.legacy_refresh_context(),
            auth.refresh_now(&self.client, SystemTime::now()),
        )
        .await;
        refresh.map_err(OwnerApiAuthError::Auth)?;
        fuse.reset();
        let retry = self
            .get_envelope_url_audited(
                auth.access_token(),
                endpoint,
                audit,
                vehicle_id,
                operation,
                safety_class,
                precondition,
            )
            .await;
        if matches!(retry, Err(OwnerApiError::HttpStatus(401))) {
            fuse.record_unauthorized(SystemTime::now());
            if fuse.is_blown() {
                return Err(OwnerApiAuthError::NotSignedIn);
            }
        }
        retry.map_err(OwnerApiAuthError::Owner)
    }

    async fn get_envelope_audited<T>(
        &self,
        bearer: &str,
        url: Url,
        audit: &OwnerApiRequestAudit<'_>,
        vehicle_id: Option<VehicleId>,
        operation: crate::db::OutboundRequestOperation,
        safety_class: crate::db::OutboundRequestSafetyClass,
        precondition: crate::db::OutboundRequestPrecondition,
    ) -> Result<T, OwnerApiError>
    where
        T: DeserializeOwned,
    {
        self.get_envelope_url_audited(
            bearer,
            url,
            audit,
            vehicle_id,
            operation,
            safety_class,
            precondition,
        )
        .await
    }

    async fn get_envelope_url_audited<T>(
        &self,
        bearer: &str,
        url: Url,
        audit: &OwnerApiRequestAudit<'_>,
        vehicle_id: Option<VehicleId>,
        operation: crate::db::OutboundRequestOperation,
        safety_class: crate::db::OutboundRequestSafetyClass,
        precondition: crate::db::OutboundRequestPrecondition,
    ) -> Result<T, OwnerApiError>
    where
        T: DeserializeOwned,
    {
        let receipt_id = audit.begin(vehicle_id, operation, safety_class, precondition)?;
        let result = self.get_envelope_url(bearer, url).await;
        audit.complete(receipt_id, &result)?;
        result
    }

    async fn get_envelope_url<T>(&self, bearer: &str, url: Url) -> Result<T, OwnerApiError>
    where
        T: DeserializeOwned,
    {
        let response = self
            .client
            .get(url)
            .header(ACCEPT, ACCEPT_JSON.clone())
            .bearer_auth(bearer)
            .send()
            .await
            .map_err(classify_transport_error)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            if status == 408 || status == 504 {
                return Err(OwnerApiError::RequestTimeout);
            }
            if status == 429 {
                let retry_after_seconds = parse_retry_after(response.headers());
                return Err(OwnerApiError::RateLimited {
                    retry_after_seconds,
                });
            }
            if matches!(status, 403 | 404 | 405) {
                let bytes = read_limited_response(response).await?;
                if status == 405 && is_vehicle_in_service_body(&bytes) {
                    return Err(OwnerApiError::VehicleInService);
                }
                if status == 404 && is_owner_error_body(&bytes, "not_found") {
                    return Err(OwnerApiError::VehicleNotFound);
                }
                if status == 403
                    && is_owner_error_body(&bytes, "account disabled: EXCEEDED_LIMIT")
                {
                    return Err(OwnerApiError::RateLimited {
                        retry_after_seconds: 900,
                    });
                }
            }
            return Err(OwnerApiError::HttpStatus(status));
        }

        let bytes = read_limited_response(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| OwnerApiError::InvalidResponseEnvelope)
    }
}

fn is_owner_error_body(bytes: &[u8], expected: &str) -> bool {
    if bytes == expected.as_bytes() {
        return true;
    }
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| match value {
            Value::String(value) => Some(value),
            Value::Object(mut object) => object
                .remove("error")
                .and_then(|value| value.as_str().map(str::to_owned)),
            _ => None,
        })
        .is_some_and(|value| value == expected)
}

fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> u64 {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300)
}

fn parse_vehicle_list(
    envelope: ResponseEnvelope<Vec<ProductWire>>,
) -> Result<Vec<Vehicle>, OwnerApiError> {
    if let Some(count) = envelope.count
        && count != envelope.response.len()
    {
        return Err(OwnerApiError::InvalidVehicleListCount);
    }
    envelope
        .response
        .into_iter()
        .filter_map(ProductWire::into_vehicle)
        .collect::<Result<Vec<_>, _>>()
}

fn parse_vehicle_data(
    vehicle_id: VehicleId,
    envelope: ResponseEnvelope<Map<String, Value>>,
) -> Result<VehicleData, OwnerApiError> {
    if envelope.count.is_some() || envelope.response.is_empty() {
        return Err(OwnerApiError::InvalidVehicleDataEnvelope);
    }
    let mut fields = envelope.response;
    scrub_sensitive_fields(&mut fields);
    if fields.is_empty() {
        return Err(OwnerApiError::SensitiveDataInResponse);
    }
    Ok(VehicleData { vehicle_id, fields })
}

impl fmt::Debug for OwnerApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerApi")
            .field("base_url", &self.base_url)
            .field("redirects", &"disabled")
            .finish_non_exhaustive()
    }
}

/// Owner API vehicle identifiers are restricted to unsigned decimal values
/// before they become a path segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VehicleId(u64);

impl VehicleId {
    pub fn get(self) -> u64 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn from_test(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for VehicleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Minimal, non-secret vehicle discovery data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vehicle {
    pub id: VehicleId,
    pub vin: String,
    pub state: String,
    pub display_name: Option<String>,
    pub settings: ProjectionCarSettings,
}

impl Vehicle {
    pub fn is_online(&self) -> bool {
        self.state == "online"
    }

    #[cfg(test)]
    pub(crate) fn for_test(id: u64, vin: &str, state: &str) -> Self {
        Self {
            id: VehicleId(id),
            vin: vin.to_owned(),
            state: state.to_owned(),
            display_name: None,
            settings: ProjectionCarSettings::default(),
        }
    }
}

/// A successful vehicle-data response. The raw fields are intentionally kept
/// separate from the collector's future normalizer and never appear in errors.
#[derive(Clone, PartialEq)]
pub struct VehicleData {
    vehicle_id: VehicleId,
    fields: Map<String, Value>,
}

impl VehicleData {
    pub fn vehicle_id(&self) -> VehicleId {
        self.vehicle_id
    }

    pub fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }

    #[cfg(test)]
    pub(crate) fn for_test(vehicle_id: u64, fields: Value) -> Self {
        let fields = fields
            .as_object()
            .expect("test vehicle data must be an object")
            .clone();
        Self {
            vehicle_id: VehicleId(vehicle_id),
            fields,
        }
    }
}

impl fmt::Debug for VehicleData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VehicleData")
            .field("vehicle_id", &self.vehicle_id)
            .field("field_count", &self.fields.len())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ManualCollection {
    pub vehicles: Vec<Vehicle>,
    pub snapshots: Vec<VehicleData>,
    pub failures: Vec<VehicleCollectionFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VehicleCollectionFailure {
    pub vehicle_id: VehicleId,
    pub error: OwnerApiError,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OwnerApiConfigError {
    #[error("owner API base URL is invalid")]
    InvalidBaseUrl,
    #[error("owner API base URL must use HTTPS")]
    HttpsRequired,
    #[error("owner API test base URL must use HTTP or HTTPS")]
    UnsupportedBaseScheme,
    #[error("owner API base URL requires a host")]
    BaseHostRequired,
    #[error("owner API base URL cannot contain credentials")]
    EmbeddedBaseCredential,
    #[error("owner API base URL cannot contain query parameters or a fragment")]
    BaseParametersNotPermitted,
    #[error("owner API base URL cannot contain path traversal")]
    BasePathTraversal,
    #[error("owner API request timeout must be greater than zero")]
    ZeroTimeout,
    #[error("owner API HTTP client could not be constructed")]
    ClientBuild,
}

/// Every error is deliberately content-free. In particular it carries neither
/// the bearer token, a response body, nor a request URL that could contain a
/// mistakenly configured secret.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OwnerApiError {
    #[error("owner API request audit is unavailable")]
    RequestAudit,
    #[error("owner API endpoint is invalid")]
    InvalidEndpoint,
    #[error("owner API request timed out")]
    RequestTimeout,
    #[error("owner API transport failed")]
    Transport,
    #[error("owner API returned HTTP {0}")]
    HttpStatus(u16),
    #[error("owner API rate limited; retry after {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },
    #[error("owner API vehicle was not found")]
    VehicleNotFound,
    #[error("owner API vehicle is in service")]
    VehicleInService,
    #[error("owner API response exceeds the size limit")]
    ResponseTooLarge,
    #[error("owner API response body could not be read")]
    ResponseRead,
    #[error("owner API response envelope is invalid")]
    InvalidResponseEnvelope,
    #[error("owner API vehicle list count is inconsistent")]
    InvalidVehicleListCount,
    #[error("owner API vehicle record is invalid")]
    InvalidVehicleRecord,
    #[error("owner API vehicle-data envelope is invalid")]
    InvalidVehicleDataEnvelope,
    #[error("owner API response contains a credential-shaped field")]
    SensitiveDataInResponse,
    #[error("legacy owner authentication failed")]
    LegacyAuth,
}

#[derive(Debug, Error)]
pub enum OwnerApiAuthError {
    #[error("owner API request failed: {0}")]
    Owner(#[from] OwnerApiError),
    #[error("legacy auth failed: {0}")]
    Auth(#[from] LegacyAuthManagerError),
    #[error("not signed in")]
    NotSignedIn,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseEnvelope<T> {
    response: T,
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Deserialize)]
struct ProductWire {
    #[serde(default)]
    vehicle_id: Option<Value>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    vin: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

impl ProductWire {
    /// `/products` can include energy products. The documented legacy
    /// vehicle discriminator is the presence of `vehicle_id`; non-vehicle
    /// products are ignored before any vehicle-data request is made.
    fn into_vehicle(self) -> Option<Result<Vehicle, OwnerApiError>> {
        let Self {
            vehicle_id,
            id,
            vin,
            state,
            display_name,
        } = self;
        vehicle_id.as_ref()?;

        let id = match id.as_ref().map(parse_vehicle_id) {
            Some(Ok(id)) => id,
            Some(Err(error)) => return Some(Err(error)),
            None => return Some(Err(OwnerApiError::InvalidVehicleRecord)),
        };
        let vin = match vin {
            Some(vin) => vin,
            None => return Some(Err(OwnerApiError::InvalidVehicleRecord)),
        };
        let state = match state {
            Some(state) => state,
            None => return Some(Err(OwnerApiError::InvalidVehicleRecord)),
        };
        if !valid_vin(&vin)
            || !valid_state(&state)
            || display_name
                .as_deref()
                .is_some_and(|name| name.len() > 1024)
        {
            return Some(Err(OwnerApiError::InvalidVehicleRecord));
        }

        Some(Ok(Vehicle {
            id,
            vin,
            state,
            display_name,
            settings: ProjectionCarSettings::default(),
        }))
    }
}

fn parse_vehicle_id(value: &Value) -> Result<VehicleId, OwnerApiError> {
    let parsed = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text)
            if !text.is_empty()
                && text.len() <= 20
                && text.as_bytes().iter().all(u8::is_ascii_digit) =>
        {
            text.parse().ok()
        }
        _ => None,
    };
    parsed
        .filter(|id| (1..=i64::MAX as u64).contains(id))
        .map(VehicleId)
        .ok_or(OwnerApiError::InvalidVehicleRecord)
}

fn valid_vin(value: &str) -> bool {
    value.len() == 17
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && !value
            .bytes()
            .any(|byte| matches!(byte, b'I' | b'O' | b'Q' | b'i' | b'o' | b'q'))
}

fn valid_state(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn parse_vehicle_state(fields: Map<String, Value>) -> Result<String, OwnerApiError> {
    fields
        .get("state")
        .and_then(Value::as_str)
        .filter(|state| valid_state(state))
        .map(str::to_owned)
        .ok_or(OwnerApiError::InvalidVehicleRecord)
}

fn scrub_sensitive_fields(fields: &mut Map<String, Value>) {
    fields.retain(|key, value| {
        let sensitive = matches!(
            key.to_ascii_lowercase().as_str(),
            "access_token"
                | "refresh_token"
                | "authorization"
                | "token"
                | "tokens"
                | "backseat_token"
        );
        if !sensitive {
            scrub_sensitive_value(value);
        }
        !sensitive
    });
}

fn scrub_sensitive_value(value: &mut Value) {
    match value {
        Value::Object(fields) => scrub_sensitive_fields(fields),
        Value::Array(values) => values.iter_mut().for_each(scrub_sensitive_value),
        _ => {}
    }
}

fn classify_transport_error(error: reqwest::Error) -> OwnerApiError {
    if error.is_timeout() {
        OwnerApiError::RequestTimeout
    } else {
        OwnerApiError::Transport
    }
}

fn is_vehicle_in_service_body(bytes: &[u8]) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    fields.len() == 1
        && fields.get("error").and_then(Value::as_str) == Some("vehicle is currently in service")
}

async fn read_limited_response(response: reqwest::Response) -> Result<Vec<u8>, OwnerApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(OwnerApiError::ResponseTooLarge);
    }

    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| OwnerApiError::ResponseRead)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(OwnerApiError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
impl OwnerApi {
    pub(crate) fn for_fake_http(
        base_url: Url,
        request_timeout: Duration,
    ) -> Result<Self, OwnerApiConfigError> {
        let base_url = OwnerApiBase::from_url(base_url, false)?;
        Self::build(OwnerApiOptions::new(base_url, request_timeout), true)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        sync::{Arc, Mutex},
    };

    use axum::{
        Router,
        extract::{Path as AxumPath, State},
        http::{HeaderMap, StatusCode, Uri},
        response::IntoResponse,
        routing::get,
    };
    use tokio::{net::TcpListener, task::JoinHandle, time::sleep};

    use super::*;
    use crate::credentials::{CredentialDirectory, OWNER_TOKEN_CREDENTIAL};

    const TEST_TOKEN: &str = "test-owner-token";
    const TEST_VIN: &str = "5YJ3E1EA7KF000001";

    #[derive(Clone, Default)]
    struct FakeState {
        requests: Arc<Mutex<Vec<FakeRequest>>>,
        vehicles_body: Arc<Mutex<String>>,
        data_bodies: Arc<Mutex<BTreeMap<String, (StatusCode, String)>>>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeRequest {
        method: String,
        path: String,
        query: String,
        authorization_is_expected: bool,
    }

    impl FakeState {
        fn with_vehicles(body: &str) -> Self {
            Self {
                vehicles_body: Arc::new(Mutex::new(body.to_owned())),
                ..Self::default()
            }
        }

        fn set_data(&self, vehicle_id: u64, status: StatusCode, body: &str) {
            self.data_bodies
                .lock()
                .expect("fake data lock")
                .insert(vehicle_id.to_string(), (status, body.to_owned()));
        }
    }

    #[tokio::test]
    async fn discovery_is_get_only_and_never_queries_vehicle_data() {
        let state = FakeState::with_vehicles(&format!(
            r#"{{"response":[{{"id":1,"vehicle_id":10,"vin":"{TEST_VIN}","state":"online","tokens":["never-retained"]}},{{"id":"2","vehicle_id":20,"vin":"5YJ3E1EA7KF000002","state":"asleep"}},{{"id":3,"vehicle_id":30,"vin":"5YJ3E1EA7KF000003","state":"offline"}},{{"id":4,"vehicle_id":40,"vin":"5YJ3E1EA7KF000004","state":"suspended"}},{{"id":5,"vehicle_id":50,"vin":"5YJ3E1EA7KF000005","state":"unknown"}},{{"energy_site_id":60,"product_type":"powerwall"}}],"count":6}}"#
        ));
        let fake = FakeServer::spawn(state.clone()).await;
        let client = fake.client(Duration::from_secs(2));
        let token = fake_owner_token();

        let vehicles = client.list_vehicles(&token).await.expect("discovery");

        assert_eq!(vehicles.len(), 5);
        let requests = state.requests.lock().expect("fake request lock");
        assert_eq!(requests.len(), 1);
        assert!(requests.iter().all(|request| request.method == "GET"));
        assert!(
            requests
                .iter()
                .all(|request| request.authorization_is_expected)
        );
        assert_eq!(requests[0].path, "/api/1/products");
    }

    #[tokio::test]
    async fn lightweight_vehicle_state_uses_plain_vehicle_endpoint() {
        let request_state = FakeState::default();
        let fake = FakeServer::start(
            Router::new()
                .route("/api/1/vehicles/{vehicle_id}", get(state_handler))
                .with_state(request_state.clone()),
        )
        .await;
        let state = fake
            .client(Duration::from_secs(2))
            .vehicle_state(&fake_owner_token(), VehicleId(7))
            .await
            .expect("state response");

        assert_eq!(state, "offline");
        let requests = request_state.requests.lock().expect("request log");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/api/1/vehicles/7");
        assert!(requests[0].query.is_empty());
        assert!(requests[0].authorization_is_expected);
    }

    #[test]
    fn vehicle_data_endpoint_preserves_provider_path_and_encodes_only_the_endpoint_query() {
        let base = Url::parse("http://provider.example/owner-proxy/").expect("base URL");
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).expect("fake client");
        let endpoint = client
            .vehicle_data_endpoint(VehicleId(7))
            .expect("vehicle endpoint");

        assert_eq!(
            endpoint.path(),
            "/owner-proxy/api/1/vehicles/7/vehicle_data"
        );
        assert_eq!(
            endpoint.query(),
            Some(
                "endpoints=charge_state%3Bclimate_state%3Bclosures_state%3Bdrive_state%3Bgui_settings%3Blocation_data%3Bvehicle_config%3Bvehicle_state%3Bvehicle_data_combo"
            )
        );
        assert!(endpoint.username().is_empty());
        assert!(endpoint.password().is_none());
        assert!(!endpoint.query().unwrap().contains(TEST_TOKEN));
    }

    #[tokio::test]
    async fn per_vehicle_data_failure_is_isolated() {
        let state = FakeState::with_vehicles(&format!(
            r#"{{"response":[{{"id":1,"vehicle_id":10,"vin":"{TEST_VIN}","state":"online"}},{{"id":2,"vehicle_id":20,"vin":"5YJ3E1EA7KF000002","state":"online"}}],"count":2}}"#
        ));
        state.set_data(
            1,
            StatusCode::OK,
            r#"{"response":{"vehicle_state":{"odometer":1.0}}}"#,
        );
        state.set_data(
            2,
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":"hidden-secret"}"#,
        );
        let fake = FakeServer::spawn(state).await;
        let client = fake.client(Duration::from_secs(2));

        let token = fake_owner_token();
        let vehicles = client.list_vehicles(&token).await.expect("discovery");
        let snapshot = client
            .vehicle_data(&token, vehicles[0].id)
            .await
            .expect("first vehicle data");
        let error = client
            .vehicle_data(&token, vehicles[1].id)
            .await
            .expect_err("second vehicle failure stays typed");

        assert_eq!(snapshot.vehicle_id.get(), 1);
        assert_eq!(error, OwnerApiError::HttpStatus(503));
    }

    #[tokio::test]
    async fn teslamate_service_response_is_typed_without_body_leakage() {
        let state = FakeState::with_vehicles(&format!(
            r#"{{"response":[{{"id":1,"vehicle_id":10,"vin":"{TEST_VIN}","state":"online"}}],"count":1}}"#
        ));
        state.set_data(
            1,
            StatusCode::METHOD_NOT_ALLOWED,
            r#"{"error":"vehicle is currently in service"}"#,
        );
        let fake = FakeServer::spawn(state).await;
        let client = fake.client(Duration::from_secs(2));

        let error = client
            .vehicle_data(&fake_owner_token(), VehicleId(1))
            .await
            .expect_err("service response is not vehicle data");
        assert_eq!(error, OwnerApiError::VehicleInService);
        assert!(
            !error
                .to_string()
                .contains("vehicle is currently in service")
        );
        assert!(!format!("{error:?}").contains("vehicle is currently in service"));
    }

    #[tokio::test]
    async fn redirects_are_not_followed_or_replayed_with_the_bearer_token() {
        let state = FakeState::with_vehicles("redirect");
        let fake = FakeServer::spawn_redirecting(state.clone()).await;
        let client = fake.client(Duration::from_secs(2));

        let error = client
            .list_vehicles(&fake_owner_token())
            .await
            .expect_err("redirect is a non-success response");

        assert_eq!(error, OwnerApiError::HttpStatus(307));
        let requests = state.requests.lock().expect("fake request lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/api/1/products");
    }

    #[tokio::test]
    async fn strict_envelope_validation_and_errors_cannot_expose_a_token() {
        let state = FakeState::with_vehicles(r#"{"response":[],"count":1}"#);
        let fake = FakeServer::spawn(state).await;
        let client = fake.client(Duration::from_secs(2));
        let token = fake_owner_token();

        let error = client
            .list_vehicles(&token)
            .await
            .expect_err("count mismatch is rejected");
        assert_eq!(error, OwnerApiError::InvalidVehicleListCount);
        assert!(!error.to_string().contains(token.as_str()));
        assert!(!format!("{error:?}").contains(token.as_str()));
    }

    #[tokio::test]
    async fn response_with_credential_shaped_field_is_scrubbed_before_persistence() {
        let state = FakeState::with_vehicles(&format!(
            r#"{{"response":[{{"id":1,"vehicle_id":10,"vin":"{TEST_VIN}","state":"online"}}],"count":1}}"#
        ));
        state.set_data(
            1,
            StatusCode::OK,
            r#"{"response":{"drive_state":{"token":"do-not-store"}}}"#,
        );
        let fake = FakeServer::spawn(state).await;
        let client = fake.client(Duration::from_secs(2));

        let snapshot = client
            .vehicle_data(&fake_owner_token(), VehicleId(1))
            .await
            .expect("safe vehicle data remains usable");
        assert_eq!(
            snapshot.fields["drive_state"],
            serde_json::json!({})
        );
    }

    #[tokio::test]
    async fn request_timeout_is_bounded() {
        let fake = FakeServer::spawn_slow().await;
        let client = fake.client(Duration::from_millis(10));

        let error = client
            .list_vehicles(&fake_owner_token())
            .await
            .expect_err("slow response must respect timeout");
        assert_eq!(error, OwnerApiError::RequestTimeout);
    }

    #[test]
    fn production_base_requires_explicit_https_and_rejects_secret_bearing_forms() {
        assert!(matches!(
            OwnerApiBase::parse("http://owner.example"),
            Err(OwnerApiConfigError::HttpsRequired)
        ));
        assert!(matches!(
            OwnerApiBase::parse("https://token@owner.example"),
            Err(OwnerApiConfigError::EmbeddedBaseCredential)
        ));
        assert!(matches!(
            OwnerApiBase::parse("https://owner.example/?token=bad"),
            Err(OwnerApiConfigError::BaseParametersNotPermitted)
        ));
        let base = OwnerApiBase::parse("https://owner.example/api").expect("https base");
        assert_eq!(base.url.as_str(), "https://owner.example/api/");
        assert!(matches!(
            OwnerApi::new(OwnerApiOptions::new(base, Duration::ZERO)),
            Err(OwnerApiConfigError::ZeroTimeout)
        ));
    }

    fn fake_owner_token() -> OwnerToken {
        let directory = tempfile::tempdir().expect("fake credential directory");
        let path = directory.path().join(OWNER_TOKEN_CREDENTIAL);
        fs::write(&path, TEST_TOKEN).expect("fake credential");
        set_private_mode(&path);
        CredentialDirectory::from_path(directory.path())
            .owner_token()
            .expect("typed token from credential module")
    }

    #[cfg(unix)]
    fn set_private_mode(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .expect("private fake credential");
    }

    #[cfg(not(unix))]
    fn set_private_mode(_path: &std::path::Path) {}

    struct FakeServer {
        base_url: Url,
        _task: JoinHandle<()>,
    }

    impl FakeServer {
        async fn spawn(state: FakeState) -> Self {
            Self::start(
                Router::new()
                    .route("/api/1/products", get(list_handler))
                    .route(
                        "/api/1/vehicles/{vehicle_id}/vehicle_data",
                        get(data_handler),
                    )
                    .with_state(state),
            )
            .await
        }

        async fn spawn_redirecting(state: FakeState) -> Self {
            Self::start(
                Router::new()
                    .route("/api/1/products", get(redirect_handler))
                    .route("/redirect-capture", get(capture_redirect_handler))
                    .with_state(state),
            )
            .await
        }

        async fn spawn_slow() -> Self {
            Self::start(Router::new().route("/api/1/products", get(slow_handler))).await
        }

        async fn start(router: Router) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("fake listener");
            let address = listener.local_addr().expect("fake address");
            let task = tokio::spawn(async move {
                axum::serve(listener, router)
                    .await
                    .expect("fake server runs");
            });
            Self {
                base_url: Url::parse(&format!("http://{address}/")).expect("fake URL"),
                _task: task,
            }
        }

        fn client(&self, timeout: Duration) -> OwnerApi {
            OwnerApi::for_fake_http(self.base_url.clone(), timeout).expect("fake client")
        }
    }

    async fn list_handler(State(state): State<FakeState>, headers: HeaderMap) -> impl IntoResponse {
        record(&state, &headers, "/api/1/products");
        state.vehicles_body.lock().expect("fake list lock").clone()
    }

    async fn data_handler(
        State(state): State<FakeState>,
        AxumPath(vehicle_id): AxumPath<String>,
        headers: HeaderMap,
        uri: Uri,
    ) -> impl IntoResponse {
        let query = uri.query().unwrap_or_default();
        record_with_query(
            &state,
            &headers,
            &format!("/api/1/vehicles/{vehicle_id}/vehicle_data"),
            query,
        );
        if query
            != "endpoints=charge_state%3Bclimate_state%3Bclosures_state%3Bdrive_state%3Bgui_settings%3Blocation_data%3Bvehicle_config%3Bvehicle_state%3Bvehicle_data_combo"
        {
            return (
                StatusCode::BAD_REQUEST,
                r#"{"error":"vehicle_data endpoints query mismatch"}"#,
            )
                .into_response();
        }
        let response = state
            .data_bodies
            .lock()
            .expect("fake data lock")
            .get(&vehicle_id)
            .cloned()
            .unwrap_or((StatusCode::NOT_FOUND, r#"{"error":"not_found"}"#.to_owned()));
        response.into_response()
    }

    async fn state_handler(
        State(state): State<FakeState>,
        AxumPath(vehicle_id): AxumPath<String>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        record(
            &state,
            &headers,
            &format!("/api/1/vehicles/{vehicle_id}"),
        );
        r#"{"response":{"state":"offline"}}"#
    }

    async fn redirect_handler(
        State(state): State<FakeState>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        record(&state, &headers, "/api/1/products");
        (
            StatusCode::TEMPORARY_REDIRECT,
            [("location", "/redirect-capture")],
            "redirect",
        )
    }

    async fn capture_redirect_handler(
        State(state): State<FakeState>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        record(&state, &headers, "/redirect-capture");
        r#"{"response":[]}"#
    }

    async fn slow_handler() -> impl IntoResponse {
        sleep(Duration::from_millis(100)).await;
        r#"{"response":[]}"#
    }

    fn record(state: &FakeState, headers: &HeaderMap, path: &str) {
        record_with_query(state, headers, path, "");
    }

    fn record_with_query(state: &FakeState, headers: &HeaderMap, path: &str, query: &str) {
        let authorization_is_expected = headers
            .get("authorization")
            .is_some_and(|value| value.as_bytes() == b"Bearer test-owner-token");
        state
            .requests
            .lock()
            .expect("fake request lock")
            .push(FakeRequest {
                method: "GET".to_owned(),
                path: path.to_owned(),
                query: query.to_owned(),
                authorization_is_expected,
            });
    }

    #[test]
    fn retry_after_is_integer_exact_and_safe_on_bad_input() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("17"));
        assert_eq!(parse_retry_after(&headers), 17);

        headers.insert("retry-after", HeaderValue::from_static("bad"));
        assert_eq!(parse_retry_after(&headers), 300);
        headers.remove("retry-after");
        assert_eq!(parse_retry_after(&headers), 300);
    }

    #[test]
    fn exact_exceeded_limit_and_not_found_bodies_are_typed() {
        assert!(is_owner_error_body(
            br#"{"error":"account disabled: EXCEEDED_LIMIT"}"#,
            "account disabled: EXCEEDED_LIMIT"
        ));
        assert!(is_owner_error_body(br#"{"error":"not_found"}"#, "not_found"));
        assert!(!is_owner_error_body(br#"{"error":"other"}"#, "not_found"));
    }

    #[tokio::test]
    async fn http_429_and_exceeded_limit_are_typed_without_auth_retry() {
        let server = FakeServer::start(
            Router::new().route(
                "/api/1/products",
                get(|| async {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "17")],
                        "rate limited",
                    )
                }),
            ),
        )
        .await;
        let client = server.client(Duration::from_secs(2));
        let error = client
            .get_envelope_url::<Value>(TEST_TOKEN, server.base_url.join("api/1/products").unwrap())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            OwnerApiError::RateLimited {
                retry_after_seconds: 17
            }
        );

        let server = FakeServer::start(Router::new().route(
            "/api/1/products",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    "{\"error\":\"account disabled: EXCEEDED_LIMIT\"}",
                )
            }),
        ))
        .await;
        let error = server
            .client(Duration::from_secs(2))
            .get_envelope_url::<Value>(TEST_TOKEN, server.base_url.join("api/1/products").unwrap())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            OwnerApiError::RateLimited {
                retry_after_seconds: 900
            }
        );
    }
}
