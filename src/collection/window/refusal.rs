// SPDX-License-Identifier: AGPL-3.0-only

//! Stable refusal codes. A refused request changes no state.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum Refusal {
    // Grant shape and signature.
    #[error("grant file is larger than the limit")]
    GrantTooLarge,
    #[error("grant file is malformed")]
    GrantMalformed,
    #[error("grant format is not known")]
    FormatUnknown,
    #[error("grant signature is not valid")]
    SignatureInvalid,
    #[error("grant key is not the trusted issuer key")]
    KeyIdMismatch,
    #[error("grant names another host")]
    HostMismatch,
    #[error("grant approver is not G")]
    ApproverNotG,
    #[error("grant reason is too long")]
    ReasonTooLong,
    #[error("grant expiry is not after its start")]
    ExpiryInvalid,
    #[error("grant is longer than the code ceiling")]
    LengthExceedsCeiling,
    #[error("grant has already expired")]
    GrantExpired,
    #[error("grant belongs to another trust epoch")]
    TrustEpochMismatch,
    #[error("grant sequence is already consumed")]
    GrantConsumed,
    // Hub state.
    #[error("no issuer is trusted on this Hub")]
    TrustMissing,
    #[error("trust epoch is not newer than the recorded one")]
    TrustEpochNotNewer,
    #[error("trust document is not valid")]
    TrustInvalid,
    #[error("window state or ledger cannot be verified")]
    StateUnverifiable,
    #[error("a window is not closed")]
    WindowActive,
    #[error("the last window's handback is not recorded")]
    HandbackPending,
    #[error("an import holds the window lock")]
    ImportRunning,
    #[error("another collector holds this host")]
    CollectorBusy,
    #[error("the clock is not valid")]
    ClockInvalid,
    #[error("the clock offset is too large or cannot be measured")]
    ClockOffset,
    #[error("the consumption anchor is unavailable")]
    AnchorUnavailable,
    #[error("the consumption anchor is missing")]
    AnchorMissing,
    #[error("the consumption anchor does not match the trust record")]
    AnchorMismatch,
    #[error("window storage failed")]
    Io,
    // Issuer.
    #[error("issuer key is missing")]
    IssuerKeyMissing,
    #[error("issuer key is unreadable or not a plain 0600 file")]
    IssuerKeyUnreadable,
    #[error("issuer key has the wrong length")]
    IssuerKeyLength,
    #[error("issuer key id differs from the ledger header")]
    IssuerKeyIdMismatch,
    #[error("issuer ledger is missing")]
    IssuerLedgerMissing,
    #[error("issuer ledger is unreadable or its chain is broken")]
    IssuerLedgerBroken,
    #[error("issuer ledger is older than its keychain pin")]
    IssuerLedgerRolledBack,
    #[error("issuer ledger pin is missing")]
    IssuerPinMissing,
    #[error("issuer ledger belongs to another host")]
    IssuerWrongHost,
    #[error("approval reference is empty")]
    ApprovalRefMissing,
    #[error("interval overlaps a recorded grant")]
    Overlap,
    #[error("grant could not be published")]
    PublicationFailed,
    #[error("issuer ledger already exists")]
    IssuerExists,
    #[error("void target is not a reservation")]
    VoidTarget,
}

impl Refusal {
    /// The stable machine-readable code.
    pub fn code(self) -> &'static str {
        match self {
            Self::GrantTooLarge => "grant_too_large",
            Self::GrantMalformed => "grant_malformed",
            Self::FormatUnknown => "format_unknown",
            Self::SignatureInvalid => "signature_invalid",
            Self::KeyIdMismatch => "key_id_mismatch",
            Self::HostMismatch => "host_mismatch",
            Self::ApproverNotG => "approver_not_g",
            Self::ReasonTooLong => "reason_too_long",
            Self::ExpiryInvalid => "expiry_invalid",
            Self::LengthExceedsCeiling => "length_exceeds_ceiling",
            Self::GrantExpired => "grant_expired",
            Self::TrustEpochMismatch => "trust_epoch_mismatch",
            Self::GrantConsumed => "grant_consumed",
            Self::TrustMissing => "trust_missing",
            Self::TrustEpochNotNewer => "trust_epoch_not_newer",
            Self::TrustInvalid => "trust_invalid",
            Self::StateUnverifiable => "state_unverifiable",
            Self::WindowActive => "window_active",
            Self::HandbackPending => "handback_pending",
            Self::ImportRunning => "import_running",
            Self::CollectorBusy => "collector_busy",
            Self::ClockInvalid => "clock_invalid",
            Self::ClockOffset => "clock_offset",
            Self::AnchorUnavailable => "anchor_unavailable",
            Self::AnchorMissing => "anchor_missing",
            Self::AnchorMismatch => "anchor_mismatch",
            Self::Io => "window_io",
            Self::IssuerKeyMissing => "issuer_key_missing",
            Self::IssuerKeyUnreadable => "issuer_key_unreadable",
            Self::IssuerKeyLength => "issuer_key_length",
            Self::IssuerKeyIdMismatch => "issuer_key_id_mismatch",
            Self::IssuerLedgerMissing => "issuer_ledger_missing",
            Self::IssuerLedgerBroken => "issuer_ledger_broken",
            Self::IssuerLedgerRolledBack => "issuer_ledger_rolled_back",
            Self::IssuerPinMissing => "issuer_pin_missing",
            Self::IssuerWrongHost => "issuer_wrong_host",
            Self::ApprovalRefMissing => "approval_ref_missing",
            Self::Overlap => "overlap",
            Self::PublicationFailed => "publication_failed",
            Self::IssuerExists => "issuer_exists",
            Self::VoidTarget => "void_target",
        }
    }
}
