// SPDX-License-Identifier: AGPL-3.0-only

//! The closed code vocabularies of the legacy auth unit (C5 R52, R56).
//!
//! Every code is a fixed, bounded, lower-case ASCII word that carries no
//! secret, URL, vehicle or location (R53). They are the wire codes of the
//! `collector_auth_state` row and of readiness, and the reasons the error
//! type of R56 refers to, so the three places cannot drift apart.

/// Why an account is in `sign_in_required` or `signed_out` (C5 section 3.1,
/// R45 rows 4, 6 and 8, R8a, R41, R46).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignOutReason {
    /// R45 row 8: the token endpoint said the refresh token is dead.
    RefreshTokenRejected,
    /// R45 row 6: the request was sent and the answer never arrived.
    RefreshOutcomeUnknown,
    /// R45 row 4: the endpoint issued a grant the Hub cannot use.
    RefreshResponseUnusable,
    /// R41: the fuse counted enough rejected generations.
    AccessTokenRejected,
    /// R46: the stored pair fails the token grammar, or R8a: Tesla mode with
    /// an empty refresh token.
    StoredPairInvalid,
    /// R8a: an access-token-only provider token was refused after the pause.
    ProviderTokenRejected,
    /// R8a: an access-token-only provider token passed its own expiry.
    ProviderTokenExpired,
}

impl SignOutReason {
    /// The reason code of R52.
    pub const fn code(self) -> &'static str {
        match self {
            Self::RefreshTokenRejected => "refresh_token_rejected",
            Self::RefreshOutcomeUnknown => "refresh_outcome_unknown",
            Self::RefreshResponseUnusable => "refresh_response_unusable",
            Self::AccessTokenRejected => "access_token_rejected",
            Self::StoredPairInvalid => "stored_pair_invalid",
            Self::ProviderTokenRejected => "provider_token_rejected",
            Self::ProviderTokenExpired => "provider_token_expired",
        }
    }
}

/// Which side refused, when the state is `api_refused` (C5 R45 rows 7 and 10,
/// R49).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefusalReason {
    /// The Owner API itself was withdrawn (a 403 naming the Fleet API).
    OwnerApi,
    /// The token endpoint refused the Owner API client id.
    Client,
}

impl RefusalReason {
    /// The reason code of R52.
    pub const fn code(self) -> &'static str {
        match self {
            Self::OwnerApi => "owner_api",
            Self::Client => "client",
        }
    }
}

/// Why a token attempt will be retried, the reason codes of the `retrying`
/// state (C5 R52).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RetryReason {
    /// R45 row 1: the connection was never made.
    Network,
    /// R45 row 1: the sensitive-access guard refused before sending.
    SensitiveAccess,
    /// R45 row 13: 408, 425, 429 or a 5xx.
    TeslaUnavailable,
    /// R45 row 12: a 403 that does not name the Fleet API.
    RefreshBlocked,
    /// R45 row 9: the endpoint rejected the shape of the request.
    RefreshRequestRejected,
    /// R45 row 11: a redirect, 404, 405, 410, 415 or another unlisted 4xx.
    TokenEndpointMisrouted,
    /// R45 row 7, first answer: `invalid_client` or `unauthorized_client`.
    ClientCheck,
    /// R29a: the new token cannot be renewed before it expires.
    ShortTokenLifetime,
    /// R29a, R55: the lifetime is short because the local clock is wrong.
    ClockSkew,
    /// R45 row 14: the endpoint returned the pair it was given.
    RefreshNotRenewed,
    /// The store could not answer a reservation or a read of the pair row, so
    /// nothing was sent. The sensitive-access guard did not refuse and the
    /// network was not tried, so neither of the reasons above is true of it;
    /// the store is usually back within seconds (a busy database), and the
    /// attempt follows the same schedule as any other retry (R31).
    StoreUnavailable,
}

impl RetryReason {
    /// The reason code of R52.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::SensitiveAccess => "sensitive_access",
            Self::TeslaUnavailable => "tesla_unavailable",
            Self::RefreshBlocked => "refresh_blocked",
            Self::RefreshRequestRejected => "refresh_request_rejected",
            Self::TokenEndpointMisrouted => "token_endpoint_misrouted",
            Self::ClientCheck => "client_check",
            Self::ShortTokenLifetime => "short_token_lifetime",
            Self::ClockSkew => "clock_skew",
            Self::RefreshNotRenewed => "refresh_not_renewed",
            Self::StoreUnavailable => "store_unavailable",
        }
    }
}

/// Why the state is `configuration_error` (C5 R5, R7, R52). A configuration
/// error is re-evaluated at every start and never stored as a stop (R5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConfigurationReason {
    /// R5: the token's issuer and the configured region name different regions.
    RegionConflict,
    /// R5: Tesla mode, an opaque token and no configured region.
    RegionRequired,
    /// R5: Tesla mode and a JWT issuer that is not one of the two pinned ones.
    IssuerNotAllowed,
    /// R7: provider mode without a usable `token_url`.
    TokenUrlInvalid,
    /// R7: Tesla mode with `token_url` set.
    TokenUrlNotAllowed,
}

impl ConfigurationReason {
    /// The reason code of R52.
    pub const fn code(self) -> &'static str {
        match self {
            Self::RegionConflict => "region_conflict",
            Self::RegionRequired => "region_required",
            Self::IssuerNotAllowed => "issuer_not_allowed",
            Self::TokenUrlInvalid => "token_url_invalid",
            Self::TokenUrlNotAllowed => "token_url_not_allowed",
        }
    }
}

/// The state and reason a load-time failure puts the account in (C5 R5, R7,
/// R46, R52): the pair of codes written to `collector_auth_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadFailureState {
    /// `configuration_error` or `signed_out`.
    pub state: &'static str,
    /// The matching reason code.
    pub reason: &'static str,
}

impl LoadFailureState {
    /// State code of a condition the owner fixes by changing configuration.
    pub const CONFIGURATION_ERROR: &'static str = "configuration_error";
    /// State code of a condition the owner fixes by signing in again.
    pub const SIGNED_OUT: &'static str = "signed_out";

    pub(super) const fn configuration(reason: ConfigurationReason) -> Self {
        Self {
            state: Self::CONFIGURATION_ERROR,
            reason: reason.code(),
        }
    }

    pub(super) const fn signed_out(reason: SignOutReason) -> Self {
        Self {
            state: Self::SIGNED_OUT,
            reason: reason.code(),
        }
    }
}
