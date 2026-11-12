// SPDX-License-Identifier: AGPL-3.0-only

//! Small local credential helpers. Hub owns one encrypted TeslaMate pair.

#[cfg(test)]
use crate::legacy_auth::Exchange;
use crate::legacy_auth::{
    AuthEndpointConfig, AuthMode, ConfigurationReason, HolderKind, LegacyAuth, LegacyAuthError,
    LegacyAuthFuse, RefusalReason, RejectionContext, RequestScope, RetryReason, SignOutReason,
    attempt::{
        Attempt, AttemptPorts, AuthEnv, BeginFailure, Closing, ExchangeInput, ExchangeKind,
        ExchangeResult, Flight, PendingClose, PersistStep, PortError, SuccessorPair, SystemEnv,
        persist_step,
    },
    authority::{Applied, Block, stop_error},
    schedule::EXCHANGE_WAIT_AT_MOST,
    state::{
        AuthState, AuthStatus, AuthStop, CLOCK_RECHECK_SECONDS, EXTERNAL_WRITE_RECHECK_SECONDS,
        FuseSnapshot, MIN_REFRESH_INTERVAL_SECONDS, PairLoad, PersistReread,
        StartInputs, StatusReason, StatusRecorder, StatusSink, StoppedMode, StoredPairRelation,
        clock_is_valid, skew_after_answer,
    },
};
use crate::auth::credential_envelope::{MAX_TOKEN_PLAINTEXT_BYTES, CredentialSlot, RefreshOwner};
use crate::auth::credential_vault::{CredentialVault, CredentialStoreError};
mod hub_persistence;
use hub_persistence::HubCredentialPersistence;
use reqwest::Client;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const MAX_TOKEN_BYTES: usize = MAX_TOKEN_PLAINTEXT_BYTES;
const MAX_POSTGRES_PASSWORD_BYTES: usize = 4 * 1024;

type SensitiveAccessGuard = Arc<dyn Fn() -> Result<(), CredentialError> + Send + Sync>;

/// The run's irreversible admission-loss fact, as the credential units see it (B2 fix
/// round, P3b fix 1, R-P3bF-1): once a supervisor has observed the loss of the
/// retained admission it answers `true` for good, whatever the admission itself says
/// afterwards (a lock file put back, a path restored).
pub(crate) type LossObserver = Arc<dyn Fn() -> bool + Send + Sync>;

/// The irreversible loss of the unit's sensitive access (B2 fix round, P3b fix 1,
/// R-P3bF-1, R-P3bF-2). It is lost when the run's supervisor has observed the loss of
/// the retained admission (the shared fence, handed in by
/// [`LegacyAuthManager::share_loss_fence`]) or when the unit itself met a sensitive
/// failure at its shutdown (it latches), and it never clears: the admission passing
/// again (a lock file put back, a path restored) changes nothing. After it the unit
/// reads and applies in memory and journals, and writes nothing: no successor, no
/// receipt closing or reservation, no status row, no adoption of a stored pair.
#[derive(Default)]
struct LossGate {
    latched: AtomicBool,
    observer: Mutex<Option<LossObserver>>,
}

impl LossGate {
    fn lost(&self) -> bool {
        if self.latched.load(Ordering::SeqCst) {
            return true;
        }
        let observer = self
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        observer.is_some_and(|observed| observed())
    }

    fn latch(&self) {
        self.latched.store(true, Ordering::SeqCst);
    }

    /// A gate that consults the run's guard from its construction on (B2 fix round,
    /// P3b fix 3, R-P3bF3-1), so the unit's first status write is guarded too.
    fn observing(observer: Option<LossObserver>) -> Self {
        Self {
            latched: AtomicBool::new(false),
            observer: Mutex::new(observer),
        }
    }

    fn share(&self, observer: LossObserver) {
        *self
            .observer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observer);
    }
}

/// A sensitive outcome kept as it was (B2 fix round, P3b fix 2, R-P3bF2-2): the unit's
/// own typed error, or the bare loss of the admission for any credential-side one.
#[derive(Clone, Debug)]
enum KeptSensitive {
    Admission,
    Auth(LegacyAuthError),
}

impl KeptSensitive {
    fn of(error: &LegacyAuthManagerError) -> Self {
        match error {
            LegacyAuthManagerError::Auth(error) => Self::Auth(error.clone()),
            _ => Self::Admission,
        }
    }

    fn error(&self) -> LegacyAuthManagerError {
        match self {
            Self::Admission => CredentialError::SensitiveAccessUnavailable.into(),
            Self::Auth(error) => LegacyAuthManagerError::Auth(error.clone()),
        }
    }
}

fn permitted_sensitive_access() -> SensitiveAccessGuard {
    Arc::new(|| Ok(()))
}
/// One decrypted owner credential pair. It is intentionally not cloneable.
///
/// ```compile_fail
/// fn requires_clone<T: Clone>() {}
/// requires_clone::<teslatlas_hub::credentials::OwnerTokens>();
/// ```
#[derive(PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct OwnerTokens {
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
}
impl OwnerTokens {
    /// Build one bounded token pair from private files. Exactly one trailing
    /// LF or CRLF is accepted so normal text files do not change the token.
    pub fn from_file_bytes(
        access_token: Zeroizing<Vec<u8>>,
        refresh_token: Zeroizing<Vec<u8>>,
    ) -> Result<Self, CredentialError> {
        fn decode(bytes: &Zeroizing<Vec<u8>>) -> Result<super::credential_sign_in::DecodedFileToken, CredentialError> {
            let bytes = bytes
                .strip_suffix(b"\r\n")
                .or_else(|| bytes.strip_suffix(b"\n"))
                .unwrap_or(bytes);
            let text = std::str::from_utf8(bytes)
                .map_err(|_| CredentialError::InvalidTokenBytes)?;
            Ok(super::credential_sign_in::decoded_file_token(
                text,
            ))
        }

        let mut access_token = decode(&access_token)?;
        let mut refresh_token = decode(&refresh_token)?;
        Self::from_secret_parts(
            std::mem::take(&mut *access_token),
            std::mem::take(&mut *refresh_token),
        )
    }

    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }
    pub(crate) fn refresh_token(&self) -> &str {
        &self.refresh_token
    }
    pub(crate) fn from_secret_parts(
        access_token: String,
        refresh_token: String,
    ) -> Result<Self, CredentialError> {
        let access_token = Zeroizing::new(access_token);
        let refresh_token = Zeroizing::new(refresh_token);
        validate_token_component(&access_token)?;
        validate_token_component(&refresh_token)?;
        Ok(Self {
            access_token,
            refresh_token,
        })
    }
}
impl std::fmt::Debug for OwnerTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnerTokens([redacted])")
    }
}

#[derive(Debug, Error)]
pub enum LegacyAuthManagerError {
    #[error("legacy credential error: {0}")]
    Credential(#[from] CredentialError),
    #[error("legacy auth error: {0}")]
    Auth(#[from] LegacyAuthError),
    #[error("legacy token refresh is disabled for observer mode")]
    ObserverRefreshDisabled,
}
impl LegacyAuthManagerError {
    /// C5 R56 and C4 R57 (b): true exactly for errors that set
    /// `configuration_error`, `signed_out` or `api_refused`. `AuthUnavailable`
    /// is neither: it is a wait. A credential error (the lost authority) is the
    /// other predicate, [`is_sensitive_access_failure`](Self::is_sensitive_access_failure).
    pub(crate) fn is_terminal_auth_failure(&self) -> bool {
        matches!(self, Self::Auth(error) if error.is_terminal_auth_failure())
    }

    /// The loss of the authority to use the credential: the collector's fence ends
    /// the run for it. A failed write of a refreshed successor is not one (C5 R45
    /// row 5, R24): the successor is held and the write retried while collection
    /// goes on, so `Persistence` and `SensitivePersistenceUnavailable` are waits
    /// and never reach a caller as errors of the run.
    pub(crate) fn is_sensitive_access_failure(&self) -> bool {
        matches!(self, Self::Credential(_))
            || matches!(
                self,
                Self::Auth(
                    LegacyAuthError::SensitiveAccessUnavailable
                        | LegacyAuthError::SensitiveRotationOutcomeUnknown
                )
            )
    }
}

#[derive(Clone)]
enum LegacyAuthPersistence {
    Hub(Arc<HubCredentialPersistence>),
    #[cfg(test)]
    TestCallback(Arc<dyn Fn(&str, &str) -> Result<(), CredentialError> + Send + Sync>),
}

impl LegacyAuthPersistence {

}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LegacyAuthRefreshPolicy {
    Managed,
    Observer,
}

/// A fault a test plants in the next write of a successor (C5 tests 30 to 32).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistFault {
    /// The write fails before anything is written.
    Fail,
    /// The write commits and then reports an error.
    Ambiguous,
}

/// The sink of the account's status row: the store, and in tests a record of every
/// write attempt (R-P3F3-1) and a failure the test plants.
struct StatusStore {
    store: crate::db::HubStore,
    /// Test only: every row a write was attempted with, in order, failed or not.
    #[cfg(test)]
    attempts: Arc<Mutex<Vec<crate::db::CollectorAuthRow>>>,
    /// Test only: while set, every write fails before anything is written.
    #[cfg(test)]
    fail_writes: Arc<AtomicBool>,
}

impl StatusSink for StatusStore {
    fn write_status(
        &self,
        row: &crate::db::CollectorAuthRow,
        now_ms: i64,
    ) -> Result<bool, crate::db::StoreError> {
        #[cfg(test)]
        {
            self.attempts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(row.clone());
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(crate::db::StoreError::InvalidStoredAuthStatus);
            }
        }
        self.store.write_collector_auth_row(row, now_ms)
    }
}




/// The fencing and the guard as the exchange task sees them (C5 R15a): its own
/// handles, so the task never takes the manager lock.
struct ManagerPorts {
    persistence: LegacyAuthPersistence,
    /// The unit's guard with its loss gate in front (see `LegacyAuthManager::ports`).
    sensitive_access: SensitiveAccessGuard,
    /// The unit's irreversible loss: no reservation, closing or write once it is set.
    loss: Arc<LossGate>,
    /// The generation a manager without a store presents.
    #[cfg(test)]
    callback_generation: uuid::Uuid,
    /// Test only: the time of every write attempt of a successor.
    #[cfg(test)]
    env: Arc<dyn AuthEnv>,
    #[cfg(test)]
    write_log: Arc<Mutex<Vec<i64>>>,
}

impl AttemptPorts for ManagerPorts {
    fn sensitive_access(&self) -> Result<(), LegacyAuthError> {
        (self.sensitive_access)().map_err(|_| LegacyAuthError::SensitiveAccessUnavailable)
    }
    fn begin(&self) -> Result<Attempt, BeginFailure> {
        if self.loss.lost() { return Err(BeginFailure::Store); }
        match &self.persistence {
            LegacyAuthPersistence::Hub(persistence) => match persistence.begin_refresh() {
                Ok((receipt, input)) => Ok(Attempt { receipt: Some(receipt), input }),
                Err(CredentialError::LegacyOwnerTokenStore(crate::db::StoreError::RefreshOutcomeUnknown { .. })) =>
                    match persistence.relation() {
                        Ok(StoredPairRelation::External) => Err(BeginFailure::ExternalWrite),
                        Ok(StoredPairRelation::OwnOutput(_)) => Err(BeginFailure::OwnOutput),
                        Ok(StoredPairRelation::Current | StoredPairRelation::Missing) => Err(BeginFailure::Unresolved),
                        Err(_) => Err(BeginFailure::Store),
                    },
                Err(_) => Err(BeginFailure::Store),
            },
            #[cfg(test)]
            LegacyAuthPersistence::TestCallback(_) => Ok(Attempt { receipt: None, input: self.callback_generation }),
        }
    }
    fn cancel_unsent(&self, attempt: &Attempt) -> Result<(), PortError> {
        if self.loss.lost() { return Err(PortError); }
        match (&self.persistence, attempt.receipt) {
            (LegacyAuthPersistence::Hub(persistence), Some(receipt)) => persistence.cancel_refresh(receipt, attempt.input).map_err(|_| PortError),
            _ => Ok(()),
        }
    }
    fn close(&self, attempt: &Attempt, closing: &Closing) -> Result<(), PortError> {
        if self.loss.lost() { return Err(PortError); }
        match (&self.persistence, attempt.receipt) {
            (LegacyAuthPersistence::Hub(persistence), Some(receipt)) => persistence.close_refresh(receipt, attempt.input, closing.outcome, closing.http_status, closing.retry_after_seconds, closing.release_input).map_err(|_| PortError),
            _ => Ok(()),
        }
    }
    fn finish(&self, pending: &PendingClose) -> Result<(), PortError> {
        let done = match pending { PendingClose::Cancel(attempt) => self.cancel_unsent(attempt), PendingClose::Close(attempt, closing) => self.close(attempt, closing) };
        match (done, &self.persistence) {
            (Ok(()), _) => Ok(()),
            (Err(_), LegacyAuthPersistence::Hub(persistence)) =>
                match persistence.vault.has_unresolved_refresh(CredentialSlot::OwnerApi) { Ok(false) => Ok(()), _ => Err(PortError) },
            #[cfg(test)]
            (Err(error), LegacyAuthPersistence::TestCallback(_)) => Err(error),
        }
    }
    fn persist(&self, attempt: &Attempt, successor: &SuccessorPair) -> Result<(), PortError> {
        let result = match (&self.persistence, attempt.receipt) {
            (LegacyAuthPersistence::Hub(persistence), Some(receipt)) =>
                persistence.persist_refreshed(receipt, attempt.input, successor.output, successor.access_token(), successor.refresh_token(), successor.schedule.expires_at, successor.schedule.next_refresh_at),
            #[cfg(test)]
            (LegacyAuthPersistence::TestCallback(persist), _) => persist(successor.access_token(), successor.refresh_token()).map(|_| crate::db::RefreshCompletion::Completed),
            _ => return Err(PortError),
        };
        #[cfg(test)]
        if !matches!(&result, Ok(crate::db::RefreshCompletion::AlreadyComplete)) {
            self.write_log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(self.env.now());
        }
        result.map(|_| ()).map_err(|_| PortError)
    }
    fn reread(&self, attempt: &Attempt, successor: uuid::Uuid) -> PersistReread {
        match &self.persistence {
            LegacyAuthPersistence::Hub(persistence) => match persistence.stored_generation() {
                // X8: neither the generation nor the classification proves the
                // successor durable. StillInput makes persist_step retry R31.
                Ok(Some(stored)) if stored == attempt.input || stored == successor => PersistReread::StillInput,
                Ok(Some(_)) => PersistReread::External,
                Ok(None) => PersistReread::Missing,
                Err(_) => PersistReread::StillInput,
            },
            #[cfg(test)]
            LegacyAuthPersistence::TestCallback(_) => PersistReread::StillInput,
        }
    }
}

/// Rebuilds the in-memory pair from a stored row: at start, and when the row
/// changes under the unit (R44a, R62).
type Rebuild = Arc<dyn Fn(&OwnerTokens, &(i64, i64)) -> Result<LegacyAuth, LegacyAuthError> + Send + Sync>;

fn endpoint_rebuild(endpoint: &AuthEndpointConfig) -> Rebuild {
    let endpoint = endpoint.clone();
    Arc::new(move |tokens, stored| {
        LegacyAuth::from_persisted_state(
            tokens.access_token(),
            tokens.refresh_token(),
            stored.0,
            stored.1,
            &endpoint,
        )
    })
}

/// Test only: the store dates its receipts by the real clock, and a test runs the
/// manager on a manual one (the fixtures start at 2027-01-15). A receipt that
/// began after the manual clock would hold every restart back by the minute of
/// R58 (a receipt in the future is bounded by a minute from now), so a test that
/// restarts a manager would behave differently after that date. Such receipts are
/// dated an hour before the manual clock: the tests do not depend on the day
/// they run, and a test that wants a floor seeds a receipt on the manual clock
/// itself (C5T-08).
#[cfg(test)]
fn redate_receipts_after(store: &crate::db::HubStore, now: i64) {
    let now_ms = now.saturating_mul(1_000);
    let earlier = now_ms.saturating_sub(3_600_000);
    store
        .open()
        .expect("a connection")
        .execute(
            "UPDATE outbound_request_receipts
                SET started_at_ms = ?1,
                    completed_at_ms = CASE WHEN outcome = 'started' THEN NULL ELSE ?1 + 5 END,
                    duration_ms = CASE WHEN outcome = 'started' THEN NULL ELSE 5 END
              WHERE started_at_ms > ?2",
            rusqlite::params![earlier, now_ms],
        )
        .expect("receipts are redated");
}

/// The region a stopped manager names for the data hosts it will not call: the
/// configured one, or the global one (C5-B2).
fn fallback_region(endpoint: &AuthEndpointConfig) -> crate::tesla_stream::StreamRegion {
    endpoint
        .region
        .unwrap_or(crate::tesla_stream::StreamRegion::Global)
}

/// The error a configuration reason reports (R5, R7).
fn configuration_error_of(reason: ConfigurationReason) -> LegacyAuthError {
    match reason {
        ConfigurationReason::RegionConflict => LegacyAuthError::RegionConflict,
        ConfigurationReason::RegionRequired => LegacyAuthError::RegionRequired,
        ConfigurationReason::IssuerNotAllowed => LegacyAuthError::InvalidIssuer,
        ConfigurationReason::TokenUrlInvalid => LegacyAuthError::TokenUrlInvalid,
        ConfigurationReason::TokenUrlNotAllowed => LegacyAuthError::TokenUrlNotAllowed,
    }
}

pub(crate) struct LegacyAuthManager {
    auth: LegacyAuth,
    persistence: LegacyAuthPersistence,
    refresh_policy: LegacyAuthRefreshPolicy,
    sensitive_access: SensitiveAccessGuard,
    /// The unit's irreversible loss (see [`LossGate`]), consulted with the guard.
    loss: Arc<LossGate>,
    /// The single-flight object of the account (R15, R15a).
    flight: Flight,
    /// The clock, the sleeper and the random source (R37).
    env: Arc<dyn AuthEnv>,
    /// Set when the collector's shutdown begins: no new token request starts
    /// (R33a).
    shutdown: Arc<AtomicBool>,
    /// R1: Tesla or Provider, as the mode of the stored refusal (R43).
    mode: AuthMode,
    /// The writer of the status row (R52a); `None` for a manager without a store.
    status: Option<StatusRecorder<StatusStore>>,
    /// The fuse values stored with the status (R42, R57 step 7). The rejection
    /// handling owns them; the manager keeps the last it was given.
    fuse: FuseSnapshot,
    /// A configuration error found when an external write changed the pair
    /// (R44a, R5): every request stops until it is fixed.
    config_error: Option<ConfigurationReason>,
    rebuild: Rebuild,
    /// When the stored generation was last compared with ours while stopped
    /// (R44a).
    last_stored_check: i64,
    /// Set when a pair written under the unit was loaded during the present
    /// call: the call sends nothing (R44a).
    adopted: bool,
    /// The fuse restored at start (R57 step 7), handed to the first fuse that
    /// asks, so a crash loop cannot evade it (C5R-13).
    fuse_handoff: Option<FuseSnapshot>,
    /// An external credential write or the owner's retry reset the fuse (R42):
    /// the fuse object resets itself the next time it is synchronised.
    fuse_reset_pending: bool,
    /// The forced refresh that follows the confirmation pause began, so the
    /// stored pause is spent (R41, R57 step 8).
    fuse_pause_spent: bool,
    /// The earliest second at which the refresh a restored count-1 fuse owes may run
    /// (the restart floor of R58 at construction, B2 apply, R3C-01), or 0. While the
    /// owed refresh (`forced_after_hold`) is outstanding, requests are held until then,
    /// as the pause of count 2 holds them: the fuse's synchronisation keeps the later of
    /// the two as the hold.
    owed_refresh_not_before: i64,
    /// A stored refusal of another mode that the constructor left in the store for
    /// the ordinary start's transaction to lift (C3 R7, three conditions), as the
    /// row held it. Until the start reports that it lifted the refusal
    /// (`apply_collector_start`), no write of a status that merely carries no stop
    /// may replace the row: such a status is written as this refusal, with the
    /// unit's own fuse and clock-skew values, so those changes are persisted and
    /// the refusal stays. Only a stop of this unit's own (R43) ends the
    /// retention.
    retained_refusal: Option<RetainedRefusal>,
    /// The credential attention last journaled (C5 R52, B2 clarification 3), in
    /// memory only: the identity is the attention code with the state and reason
    /// that gave it, and nothing else, so a change of the fuse, a retry time, the
    /// persistence of the row or a request error never repeats it.
    attention_logged: Option<(&'static str, String, Option<String>)>,
    /// The first sensitive outcome any application of an exchange's result met (B2 fix
    /// round, P3b fix 2, R-P3bF2-2): kept for the shutdown and for the supervisor, and
    /// never replaced. The unit's loss gate is latched with it, before anything else
    /// is written.
    sensitive_outcome: Option<KeptSensitive>,
    /// Test only: the status sink's record of write attempts, and its planted failure.
    #[cfg(test)]
    status_attempts: Arc<Mutex<Vec<crate::db::CollectorAuthRow>>>,
    #[cfg(test)]
    status_fail_writes: Arc<AtomicBool>,
    #[cfg(test)]
    write_log: Arc<Mutex<Vec<i64>>>,
    /// Test only: the outcome the next send is said to have had.
    #[cfg(test)]
    injected_exchange: Mutex<Option<Exchange>>,
    /// Test only: while set, every token request is answered with this status
    /// and an empty body in place of the network, so a scenario under paused time
    /// does no I/O that the paused clock could cut short.
    #[cfg(test)]
    injected_status: Mutex<Option<u16>>,
    /// Test only: when each token request answered by `injected_status` was made,
    /// on the runtime's clock.
    #[cfg(test)]
    injected_times: Mutex<Vec<tokio::time::Instant>>,
}

/// What the unit holds of a refusal it did not take over (see
/// `LegacyAuthManager::retained_refusal`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RetainedRefusal {
    /// The refusal row, as the store holds it.
    status: AuthStatus,
}

impl std::fmt::Debug for LegacyAuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.auth.fmt(f)
    }
}
impl LegacyAuthManager {
    #[cfg(test)]
    pub(crate) fn from_hub_source_db_store(
        store: crate::db::HubStore,
        data_dir: &Path,
        endpoint: &AuthEndpointConfig,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_source_db_store_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Managed,
            endpoint.mode,
            fallback_region(endpoint),
            endpoint_rebuild(endpoint),
            Arc::new(SystemEnv),
            None,
        )
    }

    /// Load the same local token pair for a bounded observer. This manager can
    /// use its current access token, but will never submit its refresh token.
    #[cfg(test)]
    pub(crate) fn from_hub_source_db_store_observer(
        store: crate::db::HubStore,
        data_dir: &Path,
        endpoint: &AuthEndpointConfig,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_source_db_store_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Observer,
            endpoint.mode,
            fallback_region(endpoint),
            endpoint_rebuild(endpoint),
            Arc::new(SystemEnv),
            None,
        )
    }

    /// The unit of a supervised run. `loss` is the run's guard (B2 fix round, P3b fix 3,
    /// R-P3bF3-1): it is consulted immediately before the unit binds the generation of
    /// the stored pair and before it writes its first status row, with no await between,
    /// and the unit keeps it as its loss observer from then on.
    #[cfg(unix)]
    pub(crate) fn from_hub_source_db_store_for_admitted_user(
        store: crate::db::HubStore,
        data_dir: &Path,
        endpoint: &AuthEndpointConfig,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
        loss: Option<LossObserver>,
        vault: Arc<CredentialVault>,
    ) -> Result<Self, LegacyAuthManagerError> {
        admission.assert_sensitive_access().map_err(|_|CredentialError::SensitiveAccessUnavailable)?;
        admission.assert_store_path(data_dir).map_err(|_|CredentialError::SensitiveAccessUnavailable)?;
        if loss.as_ref().is_some_and(|lost|lost()){return Err(CredentialError::SensitiveAccessUnavailable.into());}
        Self::from_vault_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Managed,
            endpoint.mode,
            fallback_region(endpoint),
            endpoint_rebuild(endpoint),
            Arc::new(SystemEnv),
            loss,
            vault,
        )
        .map(|manager| manager.with_runtime_admission(admission))
    }

    #[cfg(unix)]
    pub(crate) fn from_hub_source_db_store_observer_for_admitted_user(
        store: crate::db::HubStore,
        data_dir: &Path,
        endpoint: &AuthEndpointConfig,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
        loss: Option<LossObserver>,
        vault: Arc<CredentialVault>,
    ) -> Result<Self, LegacyAuthManagerError> {
        admission.assert_sensitive_access().map_err(|_|CredentialError::SensitiveAccessUnavailable)?;
        admission.assert_store_path(data_dir).map_err(|_|CredentialError::SensitiveAccessUnavailable)?;
        if loss.as_ref().is_some_and(|lost|lost()){return Err(CredentialError::SensitiveAccessUnavailable.into());}
        Self::from_vault_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Observer,
            endpoint.mode,
            fallback_region(endpoint),
            endpoint_rebuild(endpoint),
            Arc::new(SystemEnv),
            loss,
            vault,
        )
        .map(|manager| manager.with_runtime_admission(admission))
    }

    /// Load the stored pair, restore the unit's state from the store (R57 steps 1
    /// to 5 and 7 to 9) and write the status.
    ///
    /// The pair is decrypted first (R57 step 2), so every later step compares a
    /// generation it could compute. A condition that stops the unit before any
    /// request (an unresolved attempt, a pair that fails the grammar, a
    /// configuration the token contradicts) is written to the status row and
    /// the manager is built IN that state (C5-B2): it holds a stop or a
    /// configuration error and sends nothing, so the collector still starts,
    /// takes its lease, shows the state, keeps the imports, the read API, sync
    /// and backups running (R43) and lifts the stop when the owner acts (R39,
    /// R44, R44a). Failing the construction instead would end the run before its
    /// readiness, and the service would restart in a loop without ever showing
    /// the owner why. A configuration error is not stored as a stop and is
    /// evaluated again at every start (R5). What still fails the construction is
    /// a fault of the credential store itself: a missing row, an unreadable key
    /// or a ciphertext that does not open.
    ///
    /// `fallback_region` names the data hosts when the pair gives no region (a
    /// stopped manager calls none of them).
    #[cfg(test)]
    fn from_hub_source_db_store_with_policy(
        store: crate::db::HubStore, data_dir: &Path, refresh_policy: LegacyAuthRefreshPolicy,
        mode: AuthMode, fallback_region: crate::tesla_stream::StreamRegion,
        rebuild: Rebuild, env: Arc<dyn AuthEnv>, loss: Option<LossObserver>,
    ) -> Result<Self, LegacyAuthManagerError> {
        if loss.as_ref().is_some_and(|lost| lost()) { return Err(CredentialError::SensitiveAccessUnavailable.into()); }
        let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store.clone(), data_dir);
        Self::from_vault_with_policy(store, data_dir, refresh_policy, mode, fallback_region, rebuild, env, loss, vault)
    }

    fn from_vault_with_policy(
        store: crate::db::HubStore,
        _data_dir: &Path,
        refresh_policy: LegacyAuthRefreshPolicy,
        mode: AuthMode,
        fallback_region: crate::tesla_stream::StreamRegion,
        rebuild: Rebuild,
        env: Arc<dyn AuthEnv>,
        loss: Option<LossObserver>,
        vault: Arc<CredentialVault>,
    ) -> Result<Self, LegacyAuthManagerError> {
        if loss.as_ref().is_some_and(|lost| lost()) { return Err(CredentialError::SensitiveAccessUnavailable.into()); }
        let plain = vault.load(CredentialSlot::OwnerApi).map_err(CredentialError::from)?.ok_or(CredentialError::SourceDbTokenStoreMissing)?;
        let stored = (plain.expires_at, plain.next_refresh_at);
        let credential_generation = plain.generation;
        let refresh_policy = if plain.refresh_owner == RefreshOwner::External { LegacyAuthRefreshPolicy::Observer } else { refresh_policy };
        let tokens = OwnerTokens { access_token: plain.access, refresh_token: plain.refresh };
        let built = rebuild(&tokens, &stored);
        let now = env.now();
        let observer = refresh_policy == LegacyAuthRefreshPolicy::Observer;
        let (pair, configuration) = match &built {
            Ok(auth) => (
                PairLoad::Loaded {
                    generation: credential_generation,
                    usable_until: auth.usable_until(),
                },
                Ok(()),
            ),
            Err(error) => match error.configuration_reason() {
                Some(reason) => (
                    PairLoad::Loaded {
                        generation: credential_generation,
                        usable_until: None,
                    },
                    Err(reason),
                ),
                None => (
                    PairLoad::Invalid {
                        generation: credential_generation,
                    },
                    Ok(()),
                ),
            },
        };
        let mut restored = crate::legacy_auth::state::restore_from_facts(
            &StartInputs {
                now,
                mode,
                observer,
                pair,
                configuration,
                c3_reset_refusal: false,
            },
            &crate::legacy_auth::state::StartFacts {
                unresolved_attempt: vault.has_unresolved_refresh(CredentialSlot::OwnerApi).map_err(CredentialError::from)?,
                stored: store.load_collector_auth_row().map_err(CredentialError::LegacyOwnerTokenStore)?,
                current_is_refresh_output: vault.is_completed_output(CredentialSlot::OwnerApi, credential_generation).map_err(CredentialError::from)?,
                last_receipt_started_at_ms: store.latest_legacy_refresh_started_at_ms().map_err(CredentialError::LegacyOwnerTokenStore)?,
            },
        )
        .map_err(CredentialError::LegacyOwnerTokenStore)?;
        // The digest of the access token the stored pair holds (hub fix pass 3): what a
        // counted rejection's stored digest is compared with.
        let access_digest =
            crate::source_token_envelope::legacy_access_token_digest(tokens.access_token());
        // A row migrated from before the digest column (schema step 72) counted a
        // rejection outside a stop and holds no digest, and the pair is encrypted, so the
        // migration could not fill it: the rejected token is taken to be the one the pair
        // holds now, a conservative approximation (an unrotated refresh keeps the count
        // while replacing the token) for rows that predate the column (dev and lab stores
        // only): at worst one forced refresh that was not needed. The fill is committed
        // here, before any refresh work, by a conditional write (only while the digest is
        // NULL for the restored fuse and the stored pair is the one read); when it fails
        // the construction fails, so the unit retries and no refresh or request is made on
        // a row whose obligation is not durable (hub fix pass 4, F4-2). The write stands
        // behind the run's guard, as the generation bind below does, with no await between.
        if restored.status.fuse.consecutive >= 1
            && restored.status.fuse.rejected_access.is_none()
            && restored.status.stop().is_none()
        {
            if loss.as_ref().is_some_and(|lost| lost()) {
                return Err(CredentialError::SensitiveAccessUnavailable.into());
            }
            vault.fill_rejected_access_digest(credential_generation, access_digest, restored.status.fuse.generation, restored.status.fuse.consecutive).map_err(CredentialError::from)?;
            restored.status.fuse.rejected_access = Some(access_digest);
        }
        let stored_row = store
            .load_collector_auth_row()
            .map_err(CredentialError::LegacyOwnerTokenStore)?;
        let stored_refusal = stored_row
            .as_ref()
            .filter(|row| row.state == "api_refused")
            .and_then(|row| AuthStatus::from_row(row).ok());
        let defers_refusal_to_start = stored_refusal.is_some()
            && !matches!(restored.status.stop(), Some(AuthStop::ApiRefused(_)));
        // A refusal left to the start is also what the recorder holds as the current
        // status, so no flush before the first status write can replace the row.
        let recorder_status = stored_refusal
            .filter(|_| defers_refusal_to_start)
            .unwrap_or(restored.status);
        #[cfg(test)]
        let status_attempts = Arc::new(Mutex::new(Vec::new()));
        #[cfg(test)]
        let status_fail_writes = Arc::new(AtomicBool::new(false));
        let recorder = StatusRecorder::new(
            StatusStore {
                store: store.clone(),
                #[cfg(test)]
                attempts: Arc::clone(&status_attempts),
                #[cfg(test)]
                fail_writes: Arc::clone(&status_fail_writes),
            },
            recorder_status,
            stored_row,
        );
        // The generation bind is a store mutation of its own: the run's guard stands in
        // front of it, with no await between (B2 fix round, P3b fix 3, R-P3bF3-1).
        if loss.as_ref().is_some_and(|lost| lost()) {
            return Err(CredentialError::SensitiveAccessUnavailable.into());
        }
        // R28 already binds every row to its stored generation.
        // A pair the configuration cannot use, or that fails the grammar, builds
        // no `LegacyAuth`: the manager holds an inert one, and the status the
        // restore decided (a stop, or a configuration error) stops it (C5-B2).
        let (mut auth, config_error) = match built {
            Ok(auth) => (auth, None),
            Err(error) => (
                LegacyAuth::inert(fallback_region),
                error.configuration_reason(),
            ),
        };
        auth.token_requests_not_before = restored.token_requests_not_before;
        auth.clock_skew = restored.status.clock_skew;
        auth.stop = restored.status.stop();
        let mut manager = Self {
            auth,
            persistence: LegacyAuthPersistence::Hub(Arc::new(HubCredentialPersistence::new(vault, credential_generation))),
            refresh_policy,
            sensitive_access: permitted_sensitive_access(),
            // The run's guard is the unit's observer from its construction on, so the
            // first status publication below is held back after a loss.
            loss: Arc::new(LossGate::observing(loss)),
            flight: Flight::default(),
            env,
            shutdown: Arc::new(AtomicBool::new(false)),
            mode,
            status: Some(recorder),
            fuse: restored.status.fuse,
            config_error,
            rebuild,
            last_stored_check: now,
            adopted: false,
            fuse_handoff: (restored.status.fuse != FuseSnapshot::default())
                .then_some(restored.status.fuse),
            fuse_reset_pending: false,
            fuse_pause_spent: false,
            owed_refresh_not_before: 0,
            retained_refusal: stored_refusal
                .filter(|_| defers_refusal_to_start)
                .map(|status| RetainedRefusal { status }),
            attention_logged: None,
            sensitive_outcome: None,
            #[cfg(test)]
            status_attempts,
            #[cfg(test)]
            status_fail_writes,
            #[cfg(test)]
            write_log: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            injected_exchange: Mutex::new(None),
            #[cfg(test)]
            injected_status: Mutex::new(None),
            #[cfg(test)]
            injected_times: Mutex::new(Vec::new()),
        };
        // The forced refresh a restored count owes, derived from stored data (hub fix pass
        // 3): the window is open and the stored pair still holds the rejected token.
        let owes_forced_refresh = crate::legacy_auth::state::fuse_owes_forced_refresh(
            &restored.status.fuse,
            credential_generation,
            access_digest,
            now,
            restored.clock_valid,
        );
        // R57 step 8: a restored pause holds requests until it ends, and the
        // forced refresh that follows it is owed while the rejected token is still the
        // stored one (a pair that cannot refresh has no such refresh; its confirmation is
        // the next request). A pause that is over with the rejected token already
        // replaced, the refresh having completed before the fuse was synchronised, owes
        // nothing.
        if let Some(until) = restored.status.fuse.pause_until.filter(|_| !observer) {
            manager.auth.hold_until = Some(until);
            manager.auth.forced_after_hold =
                manager.auth.token_endpoint().is_some() && owes_forced_refresh;
        }
        // R57 step 7 (B2 round 3, C5-R3-01, coordinator amendment): a restored fuse
        // whose rejected generation is still the current one and which owes no pause
        // is a count 1 whose action the restart threw away. A later 401 of that
        // generation would otherwise be coalesced behind an action that no longer
        // exists, for up to an hour. Put back what count 1 owed: a managed unit's
        // forced refresh (still through R16, R19 and R58, and only where a refresh
        // is possible), or an observer's wait for the refresh owner's next pair.
        //
        // The forced refresh is owed only while it is outstanding, and that is derived from
        // stored data, not from a flag (hub fix pass 3): the fuse keeps the digest of the
        // access token it counted as rejected, and the refresh is owed while the fuse window
        // is open and the stored pair still holds that token. A refresh that replaced the
        // token, rotated or not, an external write and a 2xx change the data or clear the
        // fuse; an answer that renewed nothing (C5 R45 row 14) leaves the token, so the
        // refresh stays owed; a status row written late cannot bring it back, because the
        // decision compares the stored pair. Requests are held until that refresh can
        // run (B2 apply, R3C-01): the later of the restart floor of R58 and R19 (nothing
        // of R19 survives a restart), as the pause of count 2 holds them, so the restored
        // fuse never sees a second 401 of the rejected token that it would coalesce.
        let restored_fuse = restored.status.fuse;
        if restored_fuse.consecutive >= 1
            && restored_fuse.pause_until.is_none()
            && restored_fuse.generation == Some(manager.generation())
            && manager.auth.stop.is_none()
        {
            if observer {
                manager.auth.observer_rejected = true;
            } else if manager.auth.token_endpoint().is_some() && owes_forced_refresh {
                manager.auth.forced_after_hold = true;
                let floor = manager.auth.token_requests_not_before;
                if floor > now {
                    manager.auth.hold_until = Some(floor);
                    manager.owed_refresh_not_before = floor;
                }
            }
        }
        // C3 R7, B2 clarification 3: a stored refusal that the restore dropped for
        // another mode belongs to the ordinary start's transaction, which lifts it
        // (or not) from the rows as they stand. Publishing here would clear it
        // before that decision; the status is written after the start commits
        // (`apply_collector_start`), and before any request.
        if !defers_refusal_to_start {
            manager.publish_status(now);
        }
        Ok(manager)
    }

    /// Hand the run's irreversible loss fence to the unit (B2 fix round, P3b fix 1,
    /// R-P3bF-1): once the supervisor has observed the loss of the retained admission,
    /// the unit and its detached exchange refuse every persistence step, whatever the
    /// admission says afterwards.
    pub(crate) fn share_loss_fence(&mut self, observer: LossObserver) {
        self.loss.share(observer);
    }

    #[cfg(unix)]
    pub(crate) fn with_runtime_admission(
        mut self,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ) -> Self {
        self.sensitive_access = Arc::new(move || {
            admission
                .assert_sensitive_access()
                .map_err(|_| CredentialError::SensitiveAccessUnavailable)
        });
        self
    }

    #[cfg(test)]
    pub(crate) fn from_hub_legacy_owner_store_with_issuer(
        store: crate::db::HubStore,
        data_dir: &Path,
        issuer: url::Url,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_source_db_store_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Managed,
            AuthMode::Tesla,
            crate::tesla_stream::StreamRegion::Global,
            issuer_rebuild(issuer),
            Arc::new(SystemEnv),
            None,
        )
    }
    #[cfg(test)]
    pub(crate) fn from_hub_source_db_store_observer_with_issuer(
        store: crate::db::HubStore,
        data_dir: &Path,
        issuer: url::Url,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_source_db_store_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Observer,
            AuthMode::Tesla,
            crate::tesla_stream::StreamRegion::Global,
            issuer_rebuild(issuer),
            Arc::new(SystemEnv),
            None,
        )
    }

    /// Test only: the production constructor with the request redirected to a
    /// loopback server while the selected endpoint stays the one asserted
    /// (section 8 fixtures), and an environment the test controls.
    #[cfg(test)]
    pub(crate) fn from_hub_source_db_store_for_test(
        store: crate::db::HubStore,
        data_dir: &Path,
        endpoint: &AuthEndpointConfig,
        loopback: Option<url::Url>,
        observer: bool,
        env: Arc<dyn AuthEnv>,
    ) -> Result<Self, LegacyAuthManagerError> {
        let vault = crate::auth::credential_vault::fixtures::open_fixture_vault(store.clone(), data_dir);
        Self::from_vault_for_test(store, data_dir, endpoint, loopback, observer, env, vault)
    }

    #[cfg(test)]
    pub(crate) fn from_vault_for_test(
        store: crate::db::HubStore, data_dir: &Path, endpoint: &AuthEndpointConfig,
        loopback: Option<url::Url>, observer: bool, env: Arc<dyn AuthEnv>,
        vault: Arc<CredentialVault>,
    ) -> Result<Self, LegacyAuthManagerError> {
        redate_receipts_after(&store, env.now());
        let inner = endpoint_rebuild(endpoint);
        let rebuild: Rebuild = Arc::new(move |tokens, stored| {
            let auth = inner(tokens, stored)?;
            Ok(match &loopback {
                Some(url) => auth.with_wire_override_for_test(url.clone()),
                None => auth,
            })
        });
        Self::from_vault_with_policy(
            store,
            data_dir,
            if observer {
                LegacyAuthRefreshPolicy::Observer
            } else {
                LegacyAuthRefreshPolicy::Managed
            },
            endpoint.mode,
            fallback_region(endpoint),
            rebuild,
            env,
            None,
            vault,
        )
    }

    #[cfg(test)]
    pub(crate) fn access_token(&self) -> &str {
        self.auth.access_token()
    }
    pub(crate) fn assert_sensitive_access(&self) -> Result<(), LegacyAuthManagerError> {
        self.check_sensitive_access().map_err(Into::into)
    }

    /// The guard with the loss gate in front of it: a lost unit is denied whatever the
    /// admission says.
    fn check_sensitive_access(&self) -> Result<(), CredentialError> {
        if self.loss.lost() {
            return Err(CredentialError::SensitiveAccessUnavailable);
        }
        (self.sensitive_access)()
    }

    /// The access token, while it may be used (R29, R38, R43): not before the
    /// clock is sane, not once `usable_until` has passed, and not while a stop
    /// that stops requests is set. A refusal here is not terminal unless it is
    /// a stop.
    pub(crate) fn access_token_for_sensitive_use(&self) -> Result<&str, LegacyAuthManagerError> {
        self.assert_sensitive_access()?;
        let now = self.env.now();
        if let Some(reason) = self.config_error {
            // The pair of a configuration error is inert and answers only
            // "unavailable": the stop that shows ahead of the error is named here.
            return Err(self
                .stop_view(now)
                .unwrap_or_else(|| configuration_error_of(reason))
                .into());
        }
        self.auth.token_available(now)?;
        Ok(self.auth.access_token())
    }
    /// The live refresh token, behind the same admission check as the access
    /// token. It exists only so a provider response that echoes it can be
    /// scrubbed before storage; it is never sent, logged or formatted.
    pub(crate) fn refresh_token_for_sensitive_use(&self) -> Result<&str, LegacyAuthManagerError> {
        self.assert_sensitive_access()?;
        Ok(self.auth.refresh_token())
    }
    #[cfg(test)]
    pub(crate) fn refresh_token(&self) -> &str {
        self.auth.refresh_token()
    }
    #[cfg(test)]
    pub(crate) fn next_refresh_at(&self) -> i64 {
        self.auth.next_refresh_at()
    }
    pub(crate) fn region(&self) -> crate::tesla_stream::StreamRegion {
        self.auth.region()
    }

    /// The generation of the pair in use (C5 section 2): the stored
    /// `credential_generation`, or the one computed from the tokens when the
    /// manager has no store.
    pub(crate) fn generation(&self) -> uuid::Uuid {
        match &self.persistence {
            LegacyAuthPersistence::Hub(persistence) => persistence.generation(),
            #[cfg(test)]
            LegacyAuthPersistence::TestCallback(_) => self.callback_generation(),
        }
    }
    pub(crate) fn allows_refresh(&self) -> bool { self.refresh_policy == LegacyAuthRefreshPolicy::Managed }
    pub(crate) fn collector_credential_identity(&self) -> Result<String, LegacyAuthManagerError> {
        self.assert_sensitive_access()?;
        match &self.persistence {
            LegacyAuthPersistence::Hub(persistence) => persistence.vault.collector_credential_identity(false).map_err(CredentialError::from).map_err(Into::into),
            #[cfg(test)] LegacyAuthPersistence::TestCallback(_) => Ok(String::new()),
        }
    }

    #[cfg(test)]
    fn callback_generation(&self) -> uuid::Uuid {
        OwnerTokens::from_secret_parts(
            self.auth.access_token().to_owned(),
            self.auth.refresh_token().to_owned(),
        )
        .map(|tokens| crate::source_token_envelope::legacy_refresh_credential_generation(&tokens))
        .unwrap_or_else(|_| uuid::Uuid::nil())
    }

    /// The flag a shutdown raises (R33a). It is held outside the manager, so it
    /// can be raised while a request or a stream's credential check keeps the lock:
    /// the collector raises it the moment its shutdown begins, before it waits for
    /// the manager (C5 R33a, Codex R12).
    pub(crate) fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    fn ports(&self) -> Arc<ManagerPorts> {
        let admission = Arc::clone(&self.sensitive_access);
        let loss = Arc::clone(&self.loss);
        Arc::new(ManagerPorts {
            persistence: self.persistence.clone(),
            sensitive_access: Arc::new({
                let loss = Arc::clone(&loss);
                move || {
                    if loss.lost() {
                        Err(CredentialError::SensitiveAccessUnavailable)
                    } else {
                        admission()
                    }
                }
            }),
            loss,
            #[cfg(test)]
            callback_generation: self.generation(),
            #[cfg(test)]
            env: Arc::clone(&self.env),
            #[cfg(test)]
            write_log: Arc::clone(&self.write_log),
        })
    }

    /// The status of the account at `now` (R52, section 5): what is stored when
    /// it changes (R52a) and what the collector reads for the lease and for its
    /// waits.
    fn current_status(&self, now: i64) -> AuthStatus {
        let generation = self.generation();
        let status = self.auth.derive_full_status(
            now,
            generation,
            StoppedMode::of(self.mode),
            self.flight.is_running(),
            self.fuse,
            self.holder(),
        );
        match self.visible_configuration_error() {
            Some(reason) => AuthStatus {
                state: AuthState::ConfigurationError,
                reason: Some(StatusReason::Configuration(reason)),
                collecting: false,
                retry_at: None,
                stopped_generation: None,
                stopped_mode: None,
                fuse: self.fuse,
                clock_skew: self.auth.clock_skew,
            },
            None => status,
        }
    }

    /// The configuration error the account shows, if any. One ranking, the one
    /// the restore at start decides (R57 steps 2, 3 and 5): every stop of the
    /// account shows ahead of a configuration error, a refusal, a sign-out of any
    /// reason (an unresolved refresh attempt included) and a sign-in stop alike,
    /// because the stop is the condition the owner resolves first and no
    /// configuration change resolves it. The configuration stays beneath the stop
    /// and reappears when it lifts.
    fn visible_configuration_error(&self) -> Option<ConfigurationReason> {
        self.config_error.filter(|_| self.auth.stop.is_none())
    }

    /// The status to write for `status`, given a refusal of another mode that the
    /// start has not yet decided to lift (see `retained_refusal`). A status that
    /// carries a stop is the unit's own change of state (R43): it is written, and
    /// it ends the retention. Any other status leaves the refusal in the row, with
    /// the fuse and the clock-skew flag of `status`, so a change of either is
    /// still persisted (R52a) without lifting a refusal that only the start (or
    /// the owner) may lift.
    ///
    /// While a refusal is retained it is the visible state, by the precedence of
    /// stops: the non-stop conditions of the live mode (refreshing, retrying,
    /// persistence pending, observer waiting, clock invalid, a configuration
    /// error) are not published over it. They stay in the unit's own state, and
    /// show again when the start lifts the refusal.
    fn status_to_write_over_retained_refusal(&mut self, status: AuthStatus) -> AuthStatus {
        let Some(retained) = self.retained_refusal else {
            return status;
        };
        if status.stop().is_some() {
            self.retained_refusal = None;
            return status;
        }
        AuthStatus {
            fuse: status.fuse,
            clock_skew: status.clock_skew,
            ..retained.status
        }
    }

    /// Write the status when it changed (R52a). The status is set in memory
    /// first: a failed write never lets requests continue (R43). The status that is
    /// set is the newest one before any write is tried, and the recorder's own
    /// `set` retries a write that is due with it, so a retry never writes a status
    /// that has been superseded (a fuse or a clock-skew flag that has since
    /// changed) and no row goes out between.
    fn publish_status(&mut self, now: i64) {
        let status = self.current_status(now);
        // The journal follows the unit's own state: a refusal held in the store for
        // the start to decide on does not silence it, and a failed write does not
        // repeat it.
        self.journal_attention(&status);
        let status = self.status_to_write_over_retained_refusal(status);
        let now_ms = now.saturating_mul(1_000);
        // A unit that has lost its sensitive access (the supervisor's fence, or its own
        // failure at shutdown) keeps the status in memory and writes no row (R-P3bF-1):
        // the account's row is a store mutation like any other.
        let refused = self.loss.lost();
        if let Some(recorder) = &mut self.status {
            if refused {
                recorder.hold(status);
            } else {
                recorder.set(status, now_ms);
            }
        }
    }

    /// Journal the credential attention when it changes (C5 R52, B2 clarification
    /// 3): a redacted warning on entry or change, an info line `attention=none`
    /// when it clears. The line says nothing of whether collection has resumed. The
    /// identity is the attention code with the state and reason that gave it. A
    /// unit that has not entered an attention says nothing, and an attention it
    /// restored at start is journaled once by this process.
    fn journal_attention(&mut self, status: &AuthStatus) {
        let row = status.to_row();
        let current = crate::db::auth_attention(&row)
            .map(|code| (code.code(), row.state.clone(), row.reason.clone()));
        if current == self.attention_logged {
            return;
        }
        match &current {
            Some((attention, state, reason)) => tracing::warn!(
                target: "collector.decision",
                vehicle = "account",
                attention = *attention,
                auth_state = state.as_str(),
                auth_reason = reason.as_deref().unwrap_or("none"),
                collecting = row.collecting,
                "credential attention"
            ),
            None => tracing::info!(
                target: "collector.decision",
                vehicle = "account",
                attention = "none",
                auth_state = row.state.as_str(),
                auth_reason = row.reason.as_deref().unwrap_or("none"),
                collecting = row.collecting,
                "credential attention cleared"
            ),
        }
        self.attention_logged = current;
    }

    /// Apply what the detached exchange published, retry a pending write when it
    /// is due, and let time bring on a stop (R15a, R24, R45). Whoever holds the
    /// manager runs this before any token use.
    fn settle(&mut self, now: i64) -> Option<LegacyAuthManagerError> {
        let mut error = None;
        if let Some(result) = self.flight.take_result() {
            error = self.apply_flight_result(*result);
        }
        self.retry_pending_close(now);
        self.retry_pending_successor(now);
        self.auth.settle_stop(now);
        self.publish_status(now);
        error
    }

    /// Apply the result of the detached exchange. A recognised sensitive outcome is
    /// latched here, wherever the result is applied (settle, service and shutdown
    /// alike), before any later mutation, and the first one is kept (B2 fix round, P3b
    /// fix 2, R-P3bF2-2). A wait for persistence stays retryable: it is no sensitive
    /// outcome, and neither is the guard's clean refusal before anything was sent (R17:
    /// the attempt was cancelled, the input is reusable and the unit retries; in a
    /// supervised run the shared write guard has set the run's fence at that very
    /// refusal, so the refusal is irreversible there whatever this unit does).
    fn apply_flight_result(&mut self, result: ExchangeResult) -> Option<LegacyAuthManagerError> {
        let clean_refusal = matches!(
            result.kind,
            ExchangeKind::NotSent {
                reason: RetryReason::SensitiveAccess
            }
        );
        let error = self.apply_flight_result_inner(result);
        if let Some(error) = error
            .as_ref()
            .filter(|error| error.is_sensitive_access_failure() && !clean_refusal)
        {
            self.loss.latch();
            if self.sensitive_outcome.is_none() {
                self.sensitive_outcome = Some(KeptSensitive::of(error));
            }
        }
        error
    }

    /// The first sensitive outcome the unit met, if any: the shutdown returns it and
    /// the supervisor ends the run with it.
    pub(crate) fn sensitive_outcome(&self) -> Option<LegacyAuthManagerError> {
        self.sensitive_outcome.as_ref().map(KeptSensitive::error)
    }

    fn apply_flight_result_inner(
        &mut self,
        result: ExchangeResult,
    ) -> Option<LegacyAuthManagerError> {
        let new_generation = match &result.kind {
            ExchangeKind::Persisted(successor) => Some(successor.output),
            _ => None,
        };
        let spread = self.env.spread_unit();
        match self.auth.apply_result(result, spread) {
            Applied::Done(error) => {
                if let (Some(generation), LegacyAuthPersistence::Hub(persistence)) = (new_generation, &self.persistence) { persistence.set_generation(generation); }
                error.map(LegacyAuthManagerError::Auth)
            }
            Applied::Adopt { external } => self
                .adopt_stored_pair(external)
                .err()
                .map(|error| self.adoption_failed(error)),
        }
    }

    /// A store, key or cipher fault met while the stored pair was loaded (C5-S6).
    /// The pair in use is unchanged and the next attempt is scheduled, so this is
    /// a wait (`AuthUnavailable`), not a loss of the process's sensitive access:
    /// the collector's fence ends the run for that, and a busy database is not
    /// it. The loss of the admission itself is kept as it is.
    fn adoption_failed(&mut self, error: LegacyAuthManagerError) -> LegacyAuthManagerError {
        if matches!(
            error,
            LegacyAuthManagerError::Credential(CredentialError::SensitiveAccessUnavailable)
        ) {
            return error;
        }
        let now = self.env.now();
        let spread = self.env.spread_unit();
        LegacyAuthManagerError::Auth(self.auth.store_unavailable(now, spread))
    }

    /// C5-S1: try the closing of an attempt again when its time has come. The
    /// receipt is `started` until it is done, so no reservation begins before
    /// (the gate of R16 holds it).
    fn retry_pending_close(&mut self, now: i64) {
        if !self.auth.close_retry_due(now) {
            return;
        }
        let Some(pending) = self.auth.pending_close else {
            return;
        };
        let ports = self.ports();
        match ports.finish(&pending) {
            Ok(()) => self.auth.clear_close(),
            Err(PortError) => self.auth.note_close_failure(now),
        }
    }

    /// R24: try the write of a pending successor again when its time has come.
    /// The row is read first: a row that holds the successor's generation means
    /// the earlier write had committed, so nothing is written twice (R62); a
    /// generation that is neither the input nor the successor means an external
    /// writer resolved the attempt (K4), so the successor is discarded and the
    /// stored pair is adopted (R44a).
    fn retry_pending_successor(&mut self, now: i64) {
        if !self.auth.persist_retry_due(now) {
            return;
        }
        let Some(successor) = self.auth.pending_successor.take() else {
            return;
        };
        let ports = self.ports();
        match persist_step(ports.as_ref(), &successor, true) {
            PersistStep::Done => {
                // Hub is the only variant outside test builds.
                #[allow(irrefutable_let_patterns)]
                if let LegacyAuthPersistence::Hub(persistence) = &self.persistence { persistence.set_generation(successor.output); }
                let spread = self.env.spread_unit();
                self.auth.apply_persisted(&successor, now, spread);
            }
            PersistStep::Pending(_) => {
                self.auth.pending_successor = Some(successor);
                self.auth.note_persist_failure(now);
            }
            PersistStep::External => {
                drop(successor);
                let _ = self.adopt_stored_pair(true);
            }
        }
    }

    /// Load the pair the store holds now (R44a, R62).
    ///
    /// `external` is true for a write the unit did not make: the fuse and the
    /// retry fields are cleared and every stop that new credentials lift is
    /// lifted, but a refusal is kept, because new credentials do not lift it
    /// (R44, seam S3). It is false for a refresh another manager of the unit
    /// wrote, which changes nothing but the pair.
    fn adopt_stored_pair(&mut self, external: bool) -> Result<(), LegacyAuthManagerError> {
        let persistence = match &self.persistence {
            LegacyAuthPersistence::Hub(persistence) => Arc::clone(persistence),
            #[cfg(test)] LegacyAuthPersistence::TestCallback(_) => return Ok(()),
        };
        if self.loss.lost() { return Err(CredentialError::SensitiveAccessUnavailable.into()); }
        let plain = persistence.vault.load(CredentialSlot::OwnerApi).map_err(CredentialError::from)?.ok_or(CredentialError::SourceDbTokenStoreMissing)?;
        let generation = plain.generation;
        let stored = (plain.expires_at, plain.next_refresh_at);
        let tokens = OwnerTokens { access_token: plain.access, refresh_token: plain.refresh };
        persistence.set_generation(generation);
        let now = self.env.now();
        match (self.rebuild)(&tokens, &stored) {
            Ok(mut fresh) => {
                fresh.clock_skew = self.auth.clock_skew;
                fresh.last_success_at = self.auth.last_success_at;
                fresh.token_requests_not_before = self.auth.token_requests_not_before;
                if external {
                    // New credentials lift every stop but a refusal (R44), and
                    // reset the fuse (R42): the fuse object follows at its next
                    // synchronisation.
                    if self
                        .auth
                        .stop
                        .is_some_and(|stop| !stop.lifted_by_external_write())
                    {
                        fresh.stop = self.auth.stop;
                    }
                    self.fuse = FuseSnapshot::default();
                    self.fuse_reset_pending = true;
                } else {
                    fresh.stop = self.auth.stop;
                    fresh.hold_until = self.auth.hold_until;
                    fresh.forced_after_hold = self.auth.forced_after_hold;
                }
                self.config_error = None;
                self.auth = fresh;
            }
            Err(error) => {
                // The new pair cannot be used under the present configuration:
                // a configuration error is evaluated again at every start and
                // after every write (R5); a pair that fails the grammar is a
                // stop that lifts when a valid one is stored (R44).
                self.fuse = FuseSnapshot::default();
                self.fuse_reset_pending = true;
                match error.configuration_reason() {
                    Some(reason) => {
                        // The new credentials lift every stop but a refusal (R44),
                        // as they do when the pair can be used; a stop left here
                        // would show ahead of the configuration it no longer
                        // explains.
                        if external
                            && self
                                .auth
                                .stop
                                .is_some_and(AuthStop::lifted_by_external_write)
                        {
                            self.auth.stop = None;
                            self.auth.clear_retry();
                        }
                        self.config_error = Some(reason);
                    }
                    None => {
                        self.config_error = None;
                        self.auth
                            .set_stop(AuthStop::SignedOut(SignOutReason::StoredPairInvalid));
                    }
                }
            }
        }
        self.last_stored_check = now;
        self.adopted = true;
        Ok(())
    }

    /// R44a: compare the stored generation with ours and adopt a pair written
    /// under us. Returns whether the pair was replaced.
    fn adopt_if_stored_pair_changed(&mut self) -> bool {
        let relation = match &self.persistence {
            LegacyAuthPersistence::Hub(persistence) => persistence.relation(),
            #[cfg(test)] LegacyAuthPersistence::TestCallback(_) => return false,
        };
        match relation {
            Ok(StoredPairRelation::External) => self.adopt_stored_pair(true).is_ok(),
            Ok(StoredPairRelation::OwnOutput(_)) => self.adopt_stored_pair(false).is_ok(),
            Ok(StoredPairRelation::Current | StoredPairRelation::Missing) | Err(_) => false,
        }
    }

    pub(crate) async fn refresh_if_due(
        &mut self,
        client: &Client,
        _now: SystemTime,
    ) -> Result<(), LegacyAuthManagerError> {
        self.refresh(client, false).await
    }
    /// A refresh that is not waited for its due time (R41, R49): the gates of R16
    /// still hold.
    pub(crate) async fn refresh_now(
        &mut self,
        client: &Client,
        _now: SystemTime,
    ) -> Result<(), LegacyAuthManagerError> {
        self.refresh(client, true).await
    }

    /// One attempt to refresh, if one is allowed and due (C5 sections 5.1 and
    /// 5.4). `now` of the public methods is not read: the unit reads its own
    /// clock (R37), so one clock serves every decision of the attempt.
    async fn refresh(
        &mut self,
        client: &Client,
        forced: bool,
    ) -> Result<(), LegacyAuthManagerError> {
        let mut now = self.env.now();
        self.adopted = false;
        let _ = self.settle(now);
        // R15: an exchange in flight is waited for, and its result used.
        if self.flight.is_running() {
            self.flight.wait().await;
            now = self.env.now();
            let _ = self.settle(now);
        }
        // A sensitive outcome met while a result was applied is the answer of this call
        // too, whichever settlement met it (R-P3bF2-2).
        if let Some(error) = self.sensitive_outcome() {
            return Err(error);
        }
        if self.adopted {
            // A pair written under the unit was loaded: nothing is sent in this
            // call (R44a). The imported pair is refreshed on its first need.
            return Ok(());
        }
        if self.refresh_policy == LegacyAuthRefreshPolicy::Observer {
            // R28: while it waits, an observer reads the stored generation every
            // 60 s, so the refresh owner's next pair (an external write) ends the
            // wait without a restart.
            self.recheck_waiting_observer(now);
            // R14: an observer never sends a token request.
            return if forced {
                Err(LegacyAuthManagerError::ObserverRefreshDisabled)
            } else {
                Ok(())
            };
        }
        if let Some(reason) = self.config_error {
            return Err(self
                .stop_view(now)
                .unwrap_or_else(|| configuration_error_of(reason))
                .into());
        }
        // R16: the gates. A forced refresh waits out the interval gates that are
        // no longer than the minimum refresh interval (R19, R49).
        let mut waited = false;
        let block = loop {
            match self.auth.gate(now, self.flight.is_running()) {
                Some(Block::Until(until))
                    if forced
                        && !waited
                        && until.saturating_sub(now) <= MIN_REFRESH_INTERVAL_SECONDS =>
                {
                    self.env.sleep(until.saturating_sub(now)).await;
                    now = self.env.now();
                    waited = true;
                }
                other => break other,
            }
        };
        match block {
            Some(Block::Stopped) => {
                // R44a: while stopped the stored generation is compared every
                // 60 s, so a new login lifts a stop without a restart.
                if now.saturating_sub(self.last_stored_check) >= EXTERNAL_WRITE_RECHECK_SECONDS {
                    self.last_stored_check = now;
                    if self.adopt_if_stored_pair_changed() {
                        let _ = self.settle(self.env.now());
                        return Ok(());
                    }
                }
                return match self.auth.effective_stop(now) {
                    Some(stop) if stop.blocks_requests() => Err(stop_error(stop).into()),
                    // C5-S2: `sign_in_required` stops token requests only. A forced
                    // refresh is told that none is possible, which is a wait for
                    // the caller, not the terminal error of a sign-out the state
                    // does not hold (R43, R56).
                    Some(AuthStop::SignInRequired(_)) if forced => {
                        Err(LegacyAuthError::RefreshNotPossible.into())
                    }
                    _ => Ok(()),
                };
            }
            Some(Block::InFlight) => {
                return Err(LegacyAuthError::AuthUnavailable { retry_at: None }.into());
            }
            Some(Block::SuccessorPending) => {
                return if forced {
                    Err(LegacyAuthError::AuthUnavailable {
                        retry_at: self.auth.persist_retry_at,
                    }
                    .into())
                } else {
                    Ok(())
                };
            }
            Some(Block::ClosePending) => {
                return if forced {
                    Err(LegacyAuthError::AuthUnavailable {
                        retry_at: self.auth.close_retry_at,
                    }
                    .into())
                } else {
                    Ok(())
                };
            }
            Some(Block::NoRefreshPath) => {
                return if forced {
                    Err(LegacyAuthError::RefreshNotPossible.into())
                } else {
                    Ok(())
                };
            }
            Some(Block::ClockInvalid) => {
                return if forced {
                    Err(LegacyAuthError::AuthUnavailable {
                        retry_at: Some(now.saturating_add(CLOCK_RECHECK_SECONDS)),
                    }
                    .into())
                } else {
                    Ok(())
                };
            }
            Some(Block::Until(until)) => {
                return if forced {
                    Err(LegacyAuthError::AuthUnavailable {
                        retry_at: Some(until),
                    }
                    .into())
                } else {
                    Ok(())
                };
            }
            None => {}
        }
        // R41: the refresh that follows the confirmation pause is owed once the
        // pause has ended, whether or not the schedule has made one due.
        let owed_after_pause =
            self.auth.forced_after_hold && self.auth.hold_until.is_none_or(|until| now >= until);
        if !forced && !owed_after_pause && !self.auth.is_due(now) {
            return Ok(());
        }
        // R44a: the stored generation is compared before a token request. A pair
        // written under us is loaded and nothing is sent.
        if self.adopt_if_stored_pair_changed() {
            let _ = self.settle(self.env.now());
            return Ok(());
        }
        // R33a: once shutdown has begun no new token request starts.
        if self.shutdown.load(Ordering::Acquire) {
            return Ok(());
        }
        self.assert_sensitive_access()?;
        let Some(endpoint) = self.auth.request_url() else {
            return Err(LegacyAuthError::RefreshNotPossible.into());
        };
        let input = ExchangeInput {
            client: client.clone(),
            endpoint,
            access_token: Zeroizing::new(self.auth.access_token().to_owned()),
            refresh_token: Zeroizing::new(self.auth.refresh_token().to_owned()),
            pinned_issuer: self.auth.pinned_issuer().map(str::to_owned),
            ports: self.ports(),
            env: Arc::clone(&self.env),
            shutdown: Arc::clone(&self.shutdown),
            generation: self.generation(),
            #[cfg(test)]
            injected: self.injected_for_test(),
        };
        if !self.flight.start(input) {
            return Ok(());
        }
        if owed_after_pause {
            // The pause is spent: a restart after this point does not owe the
            // refresh again (R57 step 8).
            self.auth.forced_after_hold = false;
            self.fuse_pause_spent = true;
        }
        self.publish_status(self.env.now());
        // The caller waits for the exchange, bounded by R33. Dropping this future
        // leaves the exchange running (R15a); the next holder applies its result.
        self.flight.wait().await;
        let error = self.settle(self.env.now());
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// R44 (b): the owner's retry lifts the stops whose refresh token is still
    /// valid (a rejected access token, a rejected provider token and a refusal).
    /// A stop that needs a new sign-in is left as it is and reported, so the
    /// interface can say so (C5R-03). Lifting clears the retry fields. The
    /// rejection fuse is reset by its owner.
    pub(crate) fn owner_retry(&mut self) -> Result<(), LegacyAuthError> {
        let now = self.env.now();
        self.auth.settle_stop(now);
        match self.auth.stop {
            Some(stop) if stop.lifted_by_owner_retry() => {
                self.auth.stop = None;
                self.auth.clear_retry();
                self.auth.client_refusals = 0;
                self.auth.hold_until = None;
                self.auth.forced_after_hold = false;
                self.auth.observer_rejected = false;
                self.fuse = FuseSnapshot::default();
                self.fuse_reset_pending = true;
                self.settle_after_lift(now);
                Ok(())
            }
            Some(AuthStop::SignInRequired(reason) | AuthStop::SignedOut(reason)) => {
                Err(LegacyAuthError::RetryNotPossible { reason })
            }
            Some(AuthStop::ApiRefused(_)) | None => Ok(()),
        }
    }

    /// After a stop was lifted: an attempt that is still unresolved stops the
    /// unit again, because the refusal never lifted that condition (R57 step 5
    /// (b)).
    fn settle_after_lift(&mut self, now: i64) {
        if self.has_unresolved_legacy_refresh() {
            self.auth
                .set_stop(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown));
        }
        self.publish_status(now);
    }

    /// R33a: the collector's shutdown. No new token request starts; an exchange
    /// in flight is waited for (at most 60 s) and its outcome handled normally,
    /// including one last write of a successor that is still pending; then the
    /// status is flushed.
    ///
    /// The result is the sensitive outcome of that work (C5 R23, R33a, B2
    /// clarification 3): the first sensitive error the exchange's application or the
    /// adoption of an external pair met, kept as it was and never replaced by a later
    /// success or a later denial, or else the final revalidation of the retained
    /// admission, made at the boundary where the shutdown ends. An ordinary outcome
    /// (a wait, a store fault met while adopting, an unknown outcome the process
    /// already owns) is not returned: it is the unit's state, and the status shows it.
    ///
    /// After a sensitive failure, and after the loss the run observed, the shutdown
    /// keeps the application and the journal in memory and mutates no store (B2 fix
    /// round, P3b fix 1, R-P3bF-2): no owed closing, no write of a pending successor,
    /// no adoption, no status row and no final attempt. The next start re-derives the
    /// stop from the receipt that stays unresolved.
    pub(crate) async fn shutdown(&mut self) -> Result<(), LegacyAuthManagerError> {
        self.shutdown.store(true, Ordering::Release);
        if self.flight.is_running() {
            self.flight.wait().await;
        }
        let now = self.env.now();
        // The admission is judged where the exchange has ended: a denial that stands
        // now stops every write that follows (R-P3bF-1).
        if self.check_sensitive_access().is_err() {
            self.loss.latch();
        }
        // The first sensitive error is kept as it was, including the one an earlier
        // settlement or service call met when it applied the exchange's result
        // (R-P3bF2-2).
        let mut sensitive: Option<LegacyAuthManagerError> = self.sensitive_outcome();
        // The first sensitive error is kept as it was.
        fn keep(slot: &mut Option<LegacyAuthManagerError>, error: Option<LegacyAuthManagerError>) {
            if slot.is_none()
                && let Some(error) =
                    error.filter(LegacyAuthManagerError::is_sensitive_access_failure)
            {
                *slot = Some(error);
            }
        }
        if let Some(result) = self.flight.take_result() {
            keep(&mut sensitive, self.apply_flight_result(*result));
        }
        // A sensitive failure is a loss for good: what follows writes nothing, even if
        // the admission passes again by then.
        if sensitive.is_some() {
            self.loss.latch();
        }
        // C5-S1: one last try at a closing that is still owed.
        if !self.loss.lost()
            && let Some(pending) = self.auth.pending_close
            && self.ports().finish(&pending).is_ok()
        {
            self.auth.clear_close();
        }
        if !self.loss.lost()
            && let Some(successor) = self.auth.pending_successor.take()
        {
            let ports = self.ports();
            match persist_step(ports.as_ref(), &successor, true) {
                PersistStep::Done => {
                    // Hub is the only variant outside test builds.
                #[allow(irrefutable_let_patterns)]
                if let LegacyAuthPersistence::Hub(persistence) = &self.persistence { persistence.set_generation(successor.output); }
                    let spread = self.env.spread_unit();
                    self.auth.apply_persisted(&successor, now, spread);
                }
                PersistStep::Pending(_) => self.auth.pending_successor = Some(successor),
                PersistStep::External => {
                    drop(successor);
                    keep(
                        &mut sensitive,
                        self.adopt_stored_pair(true)
                            .err()
                            .map(|error| self.adoption_failed(error)),
                    );
                }
            }
        }
        self.auth.settle_stop(now);
        self.publish_status(now);
        // The final write (R33a): a refusal the start has not lifted is the status
        // the call above recorded. A unit that has lost its sensitive access writes
        // none.
        if !self.loss.lost()
            && let Some(recorder) = &mut self.status
        {
            recorder.attempt(now.saturating_mul(1_000));
        }
        match sensitive {
            Some(error) => Err(error),
            None => self.assert_sensitive_access(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        auth: LegacyAuth,
        persist: Arc<dyn Fn(&str, &str) -> Result<(), CredentialError> + Send + Sync>,
    ) -> Self {
        Self {
            auth,
            persistence: LegacyAuthPersistence::TestCallback(persist),
            refresh_policy: LegacyAuthRefreshPolicy::Managed,
            sensitive_access: permitted_sensitive_access(),
            loss: Arc::new(LossGate::default()),
            flight: Flight::default(),
            env: Arc::new(SystemEnv),
            shutdown: Arc::new(AtomicBool::new(false)),
            mode: AuthMode::Tesla,
            status: None,
            fuse: FuseSnapshot::default(),
            config_error: None,
            rebuild: Arc::new(|_, _| Err(LegacyAuthError::InvalidPersistedSchedule)),
            last_stored_check: 0,
            adopted: false,
            fuse_handoff: None,
            fuse_reset_pending: false,
            fuse_pause_spent: false,
            owed_refresh_not_before: 0,
            retained_refusal: None,
            attention_logged: None,
            sensitive_outcome: None,
            status_attempts: Arc::new(Mutex::new(Vec::new())),
            status_fail_writes: Arc::new(AtomicBool::new(false)),
            write_log: Arc::new(Mutex::new(Vec::new())),
            injected_exchange: Mutex::new(None),
            injected_status: Mutex::new(None),
            injected_times: Mutex::new(Vec::new()),
        }
    }
    /// Test only: what the next send is said to have had (see `injected_exchange`
    /// and `injected_status`).
    #[cfg(test)]
    fn injected_for_test(&self) -> Option<Exchange> {
        let one_shot = self
            .injected_exchange
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        one_shot.or_else(|| {
            let status = (*self
                .injected_status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner))?;
            self.injected_times
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(tokio::time::Instant::now());
            Some(Exchange::Answer(crate::legacy_auth::RawAnswer {
                status: reqwest::StatusCode::from_u16(status).ok()?,
                headers: reqwest::header::HeaderMap::new(),
                body: crate::legacy_auth::AnswerBody::Complete(Zeroizing::new(Vec::new())),
            }))
        })
    }

    /// Test only: answer every token request with `status` and no body, in place
    /// of the network.
    #[cfg(test)]
    pub(crate) fn answer_token_requests_with(&self, status: u16) {
        *self
            .injected_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status);
    }

    /// Test only: when each token request answered by
    /// [`answer_token_requests_with`](Self::answer_token_requests_with) was made.
    #[cfg(test)]
    pub(crate) fn injected_request_times(&self) -> Vec<tokio::time::Instant> {
        self.injected_times
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Test only: every status row a write was attempted with, in order, failed
    /// attempts included.
    #[cfg(test)]
    pub(crate) fn status_write_attempts(&self) -> Vec<crate::db::CollectorAuthRow> {
        self.status_attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Test only: make every status write fail (or succeed again).
    #[cfg(test)]
    pub(crate) fn fail_status_writes(&self, fail: bool) {
        self.status_fail_writes.store(fail, Ordering::SeqCst);
    }

    /// Test only: the unit's own admission check is denied once, at the first check
    /// after the returned switch is set, and passes at every other (the admission is
    /// back), so a scenario can meet a denial at a chosen moment of a detached exchange.
    #[cfg(test)]
    pub(crate) fn deny_sensitive_access_once_for_test(&mut self) -> Arc<AtomicBool> {
        let denial = Arc::new(AtomicBool::new(false));
        let switch = Arc::clone(&denial);
        let inner = Arc::clone(&self.sensitive_access);
        self.sensitive_access = Arc::new(move || {
            if switch.swap(false, Ordering::SeqCst) {
                Err(CredentialError::SensitiveAccessUnavailable)
            } else {
                inner()
            }
        });
        denial
    }

    /// Test only: the pair held in memory, for a scenario that has to put the unit
    /// in a state no sequence of requests reaches quickly.
    #[cfg(test)]
    pub(crate) fn auth_for_test(&mut self) -> &mut LegacyAuth {
        &mut self.auth
    }

    /// The in-memory manager with an environment the test controls: its clock,
    /// its waits and its random source (C5 R37).
    #[cfg(test)]
    pub(crate) fn for_test_with_env(
        auth: LegacyAuth,
        persist: Arc<dyn Fn(&str, &str) -> Result<(), CredentialError> + Send + Sync>,
        env: Arc<dyn AuthEnv>,
    ) -> Self {
        let mut manager = Self::for_test(auth, persist);
        manager.env = env;
        manager
    }
    /// A manager that holds a configuration error (C5 R5, R7): the state an
    /// external credential write that contradicts the configuration puts the
    /// unit in, which every request and stream admission then meets.
    #[cfg(test)]
    pub(crate) fn for_test_with_configuration_error(
        auth: LegacyAuth,
        reason: ConfigurationReason,
    ) -> Self {
        let mut manager = Self::for_test(auth, Arc::new(|_, _| Ok(())));
        manager.config_error = Some(reason);
        manager
    }
    /// An observer over a pair held in memory (C5 R14, R28): it never sends a
    /// token request, and a counted rejection makes it wait.
    #[cfg(test)]
    pub(crate) fn for_test_observer(auth: LegacyAuth) -> Self {
        let mut manager = Self::for_test(auth, Arc::new(|_, _| Ok(())));
        manager.refresh_policy = LegacyAuthRefreshPolicy::Observer;
        manager
    }
    #[cfg(test)]
    pub(crate) fn for_test_with_sensitive_access(
        auth: LegacyAuth,
        persist: Arc<dyn Fn(&str, &str) -> Result<(), CredentialError> + Send + Sync>,
        sensitive_access: Arc<dyn Fn() -> Result<(), CredentialError> + Send + Sync>,
    ) -> Self {
        let mut manager = Self::for_test(auth, persist);
        manager.sensitive_access = sensitive_access;
        manager
    }
    #[cfg(test)]
    pub(crate) fn for_test_with_active_pair(
        auth: LegacyAuth,
    ) -> Result<Self, LegacyAuthManagerError> {
        Ok(Self::for_test(auth, Arc::new(|_, _| Ok(()))))
    }
    /// Test only: whether the pair held in memory is this one.
    #[cfg(test)]
    pub(crate) fn test_pair_matches(
        &self,
        access: &str,
        refresh: &str,
    ) -> Result<bool, LegacyAuthManagerError> {
        Ok(self.auth.access_token() == access && self.auth.refresh_token() == refresh)
    }
}

/// The rebuild of the `_with_issuer` test constructors (section 3.4): a stored
/// row of the refreshed form, loaded with the issuer's endpoint.
#[cfg(test)]
fn issuer_rebuild(issuer: url::Url) -> Rebuild {
    Arc::new(move |tokens, stored| {
        if stored.0 <= 0
            || stored.1 <= 0
            || stored.1 >= stored.0
        {
            return Err(LegacyAuthError::InvalidPersistedSchedule);
        }
        Ok(LegacyAuth::for_test(
            issuer.clone(),
            tokens.access_token(),
            tokens.refresh_token(),
        )
        .with_test_schedule(stored.0, stored.1))
    })
}

#[derive(Clone, PartialEq, Eq)]
pub struct SourceDbPassword(Zeroizing<String>);
impl SourceDbPassword {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CredentialError> {
        let bytes = bytes
            .strip_suffix(b"\r\n")
            .or_else(|| bytes.strip_suffix(b"\n"))
            .unwrap_or(bytes);
        if bytes.is_empty() {
            return Err(CredentialError::EmptyPostgresPassword);
        }
        if bytes.len() > MAX_POSTGRES_PASSWORD_BYTES {
            return Err(CredentialError::PostgresPasswordTooLarge);
        }
        if bytes.contains(&0) || bytes.contains(&b'\r') || bytes.contains(&b'\n') {
            return Err(CredentialError::InvalidPostgresPasswordBytes);
        }
        String::from_utf8(bytes.to_vec())
            .map(Zeroizing::new)
            .map(Self)
            .map_err(|_| CredentialError::InvalidPostgresPasswordEncoding)
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl std::fmt::Debug for SourceDbPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SourceDbPassword([redacted])")
    }
}

fn validate_token_component(value: &str) -> Result<(), CredentialError> {
    if value.is_empty() {
        return Err(CredentialError::EmptyToken);
    }
    if value.len() > MAX_TOKEN_BYTES {
        return Err(CredentialError::TokenTooLarge);
    }
    if value
        .bytes()
        .any(|b| b == 0 || b == b'\r' || b == b'\n' || b.is_ascii_control())
    {
        return Err(CredentialError::InvalidTokenBytes);
    }
    Ok(())
}
#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("owner token is empty")]
    EmptyToken,
    #[error("owner token exceeds the size limit")]
    TokenTooLarge,
    #[error("owner token contains unsupported bytes")]
    InvalidTokenBytes,
    #[error("Source database PostgreSQL password is empty")]
    EmptyPostgresPassword,
    #[error("Source database PostgreSQL password exceeds the size limit")]
    PostgresPasswordTooLarge,
    #[error("Source database PostgreSQL password contains unsupported bytes")]
    InvalidPostgresPasswordBytes,
    #[error("Source database PostgreSQL password is not UTF-8")]
    InvalidPostgresPasswordEncoding,
    #[error("Hub legacy owner token pair is missing")]
    SourceDbTokenStoreMissing,
    #[error("Hub legacy owner token store failed: {0}")]
    LegacyOwnerTokenStore(#[source] crate::db::StoreError),
    #[error("Hub legacy owner token key file failed: {0}")]
    SourceDbCredentialFile(#[source] crate::legacy_owner_credentials::SourceDbCredentialError),
    #[error("Legacy owner token operation failed: {0}")]
    SourceDbTokenCipher(#[source] crate::source_token_envelope::SourceDbTokenError),
    #[error("Hub runtime admission is unavailable")]
    SensitiveAccessUnavailable,
    /// Source credentials are disabled: the stored or offered pair is not the
    /// Hub's own login, so it is never loaded, used or refreshed.
    #[error("source_credential_disabled: {}", crate::auth::credential_sign_in::SOURCE_CREDENTIAL_DISABLED_TEXT)]
    SourceCredentialDisabled,
    #[cfg(test)]
    #[error("test sensitive access check failed")]
    MacKeychainHelperInvalid,
    #[cfg(test)]
    #[error("test persistence failed")]
    LegacyTokenStateWrite,
}

impl From<CredentialStoreError> for CredentialError {
    fn from(error: CredentialStoreError) -> Self {
        match error {
            CredentialStoreError::RefreshTokenSpent => Self::LegacyOwnerTokenStore(crate::db::StoreError::RefreshOutcomeUnknown { reason: crate::db::BeginRefusal::TokenSpent }),
            CredentialStoreError::Store(error) => Self::LegacyOwnerTokenStore(error),
            CredentialStoreError::SensitiveAccessUnavailable | CredentialStoreError::LockPoisoned => Self::SensitiveAccessUnavailable,
            CredentialStoreError::SourceCredentialDisabled(_) | CredentialStoreError::UnknownOrigin(_) => Self::SourceCredentialDisabled,
            _ => Self::LegacyOwnerTokenStore(crate::db::StoreError::WriteRefused("credential_vault_unavailable")),
        }
    }
}

mod rejection_support;
pub(crate) use rejection_support::{AuthWait, CollectorStartView, RefusalRefresh};

#[cfg(test)]
#[path = "credentials/tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) async fn assert_vault_retry_barrier_contract() {
    tests::refresh_tests::assert_vault_retry_barrier_contract().await;
}
#[cfg(test)]
pub(crate) async fn assert_vault_read_interleaving_contract() {
    tests::refresh_tests::assert_vault_read_interleaving_contract().await;
}
#[cfg(test)]
pub(crate) async fn assert_vault_fenced_successor_contract() {
    tests::refresh_tests::assert_vault_fenced_successor_contract().await;
}
#[cfg(test)]
pub(crate) async fn assert_external_refresh_owner_contract() {
    tests::refresh_tests::assert_external_refresh_owner_contract().await;
}

#[cfg(test)]
mod n1_src_auth_credentials_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for LegacyAuthManagerError {
        fn error_example() -> Self {
            LegacyAuthManagerError::ObserverRefreshDisabled
        }
    }

    impl ErrorExample for CredentialError {
        fn error_example() -> Self {
            CredentialError::EmptyToken
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_legacy_auth_manager_error(error: LegacyAuthManagerError) {
            match &error {
                LegacyAuthManagerError::Credential(..) => (),
                LegacyAuthManagerError::Auth(..) => (),
                LegacyAuthManagerError::ObserverRefreshDisabled => (),
            }
            assert_name_free(&error);
        }
        check_legacy_auth_manager_error(LegacyAuthManagerError::Credential(example()));
        check_legacy_auth_manager_error(LegacyAuthManagerError::Auth(example()));
        check_legacy_auth_manager_error(LegacyAuthManagerError::ObserverRefreshDisabled);
        fn check_credential_error(error: CredentialError) {
            match &error {
                CredentialError::EmptyToken => (),
                CredentialError::TokenTooLarge => (),
                CredentialError::InvalidTokenBytes => (),
                CredentialError::EmptyPostgresPassword => (),
                CredentialError::PostgresPasswordTooLarge => (),
                CredentialError::InvalidPostgresPasswordBytes => (),
                CredentialError::InvalidPostgresPasswordEncoding => (),
                CredentialError::SourceDbTokenStoreMissing => (),
                CredentialError::LegacyOwnerTokenStore(..) => (),
                CredentialError::SourceDbCredentialFile(..) => (),
                CredentialError::SourceDbTokenCipher(..) => (),
                CredentialError::SensitiveAccessUnavailable => (),
                CredentialError::SourceCredentialDisabled => (),
                #[cfg(test)]
                CredentialError::MacKeychainHelperInvalid => (),
                #[cfg(test)]
                CredentialError::LegacyTokenStateWrite => (),
            }
            assert_name_free(&error);
        }
        check_credential_error(CredentialError::EmptyToken);
        check_credential_error(CredentialError::TokenTooLarge);
        check_credential_error(CredentialError::InvalidTokenBytes);
        check_credential_error(CredentialError::EmptyPostgresPassword);
        check_credential_error(CredentialError::PostgresPasswordTooLarge);
        check_credential_error(CredentialError::InvalidPostgresPasswordBytes);
        check_credential_error(CredentialError::InvalidPostgresPasswordEncoding);
        check_credential_error(CredentialError::SourceDbTokenStoreMissing);
        check_credential_error(CredentialError::LegacyOwnerTokenStore(example()));
        check_credential_error(CredentialError::SourceDbCredentialFile(example()));
        check_credential_error(CredentialError::SourceDbTokenCipher(example()));
        check_credential_error(CredentialError::SensitiveAccessUnavailable);
        check_credential_error(CredentialError::SourceCredentialDisabled);
        #[cfg(test)]
        check_credential_error(CredentialError::MacKeychainHelperInvalid);
        #[cfg(test)]
        check_credential_error(CredentialError::LegacyTokenStateWrite);
    }
}
