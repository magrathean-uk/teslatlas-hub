//! Small local credential helpers. Hub owns one encrypted TeslaMate pair.

use crate::legacy_auth::{LegacyAuth, LegacyAuthError};
use crate::teslamate_token::MAX_LEGACY_TOKEN_PLAINTEXT_BYTES;
use reqwest::Client;
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::SystemTime,
};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const MAX_TOKEN_BYTES: usize = MAX_LEGACY_TOKEN_PLAINTEXT_BYTES;
const MAX_POSTGRES_PASSWORD_BYTES: usize = 4 * 1024;

type SensitiveAccessGuard = Arc<dyn Fn() -> Result<(), CredentialError> + Send + Sync>;

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
        fn decode(bytes: &Zeroizing<Vec<u8>>) -> Result<String, CredentialError> {
            let bytes = bytes
                .strip_suffix(b"\r\n")
                .or_else(|| bytes.strip_suffix(b"\n"))
                .unwrap_or(bytes);
            std::str::from_utf8(bytes)
                .map(str::to_owned)
                .map_err(|_| CredentialError::InvalidTokenBytes)
        }

        Self::from_secret_parts(decode(&access_token)?, decode(&refresh_token)?)
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
    pub(crate) fn is_sensitive_access_failure(&self) -> bool {
        matches!(self, Self::Credential(_))
            || matches!(
                self,
                Self::Auth(
                    LegacyAuthError::Persistence
                        | LegacyAuthError::SensitivePersistenceUnavailable
                        | LegacyAuthError::SensitiveAccessUnavailable
                        | LegacyAuthError::SensitiveRotationOutcomeUnknown
                )
            )
    }
}

#[derive(Clone)]
enum LegacyAuthPersistence {
    HubTeslaMate(Arc<HubTeslaMatePersistence>),
    #[cfg(test)]
    TestCallback(Arc<dyn Fn(&str, &str) -> Result<(), CredentialError> + Send + Sync>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LegacyAuthRefreshPolicy {
    Managed,
    Observer,
}
struct HubTeslaMatePersistence {
    store: crate::db::HubStore,
    encryption_key: Arc<crate::teslamate_credentials::TeslaMateEncryptionKey>,
    credential_generation: Mutex<uuid::Uuid>,
}
impl HubTeslaMatePersistence {
    fn begin_refresh(
        &self,
    ) -> Result<(crate::db::OutboundRequestReceiptId, uuid::Uuid), CredentialError> {
        let input_generation = *self.credential_generation.lock().map_err(|_| {
            CredentialError::TeslaMateTokenStore(crate::db::StoreError::LegacyRefreshOutcomeUnknown)
        })?;
        let receipt_id = self
            .store
            .begin_legacy_refresh(input_generation)
            .map_err(CredentialError::TeslaMateTokenStore)?;
        Ok((receipt_id, input_generation))
    }

    fn cancel_refresh(
        &self,
        receipt_id: crate::db::OutboundRequestReceiptId,
        input_generation: uuid::Uuid,
    ) -> Result<(), CredentialError> {
        self.store
            .cancel_unsent_legacy_refresh(receipt_id, input_generation)
            .map_err(CredentialError::TeslaMateTokenStore)
    }

    fn persist_refreshed(
        &self,
        receipt_id: crate::db::OutboundRequestReceiptId,
        input_generation: uuid::Uuid,
        access: &str,
        refresh: &str,
        expires_at: i64,
        next_refresh_at: i64,
    ) -> Result<(), CredentialError> {
        let tokens = OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned())?;
        let (access, refresh) = crate::teslamate_token::encrypt_legacy_owner_tokens(
            self.encryption_key.as_bytes(),
            &tokens,
        )
        .map_err(CredentialError::TeslaMateTokenCipher)?;
        let stored = crate::db::TeslaMateLegacyTokenStore::refreshed(
            access,
            refresh,
            expires_at,
            next_refresh_at,
        )
        .map_err(CredentialError::TeslaMateTokenStore)?;
        let output_generation =
            crate::teslamate_token::legacy_refresh_credential_generation(&tokens);
        let stored = stored
            .with_credential_generation(output_generation)
            .map_err(CredentialError::TeslaMateTokenStore)?;
        self.store
            .complete_legacy_refresh(receipt_id, input_generation, output_generation, &stored)
            .map_err(CredentialError::TeslaMateTokenStore)?;
        *self.credential_generation.lock().map_err(|_| {
            CredentialError::TeslaMateTokenStore(crate::db::StoreError::LegacyRefreshOutcomeUnknown)
        })? = output_generation;
        Ok(())
    }
}

pub(crate) struct LegacyAuthManager {
    auth: LegacyAuth,
    persistence: LegacyAuthPersistence,
    refresh_policy: LegacyAuthRefreshPolicy,
    sensitive_access: SensitiveAccessGuard,
    refresh_terminal: bool,
}
impl std::fmt::Debug for LegacyAuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.auth.fmt(f)
    }
}
impl LegacyAuthManager {
    pub(crate) fn from_hub_teslamate_store(
        store: crate::db::HubStore,
        data_dir: &Path,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_teslamate_store_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Managed,
            |tokens, stored| {
                LegacyAuth::from_persisted_state(
                    tokens.access_token(),
                    tokens.refresh_token(),
                    stored.expires_at(),
                    stored.next_refresh_at(),
                )
            },
        )
    }

    /// Load the same local token pair for a bounded observer. This manager can
    /// use its current access token, but will never submit its refresh token.
    pub(crate) fn from_hub_teslamate_store_observer(
        store: crate::db::HubStore,
        data_dir: &Path,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_teslamate_store_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Observer,
            |tokens, stored| {
                LegacyAuth::from_persisted_state(
                    tokens.access_token(),
                    tokens.refresh_token(),
                    stored.expires_at(),
                    stored.next_refresh_at(),
                )
            },
        )
    }

    #[cfg(unix)]
    pub(crate) fn from_hub_teslamate_store_for_admitted_user(
        store: crate::db::HubStore,
        data_dir: &Path,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_teslamate_store(store, data_dir)
            .map(|manager| manager.with_runtime_admission(admission))
    }

    #[cfg(unix)]
    pub(crate) fn from_hub_teslamate_store_observer_for_admitted_user(
        store: crate::db::HubStore,
        data_dir: &Path,
        admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_teslamate_store_observer(store, data_dir)
            .map(|manager| manager.with_runtime_admission(admission))
    }

    fn from_hub_teslamate_store_with_policy(
        store: crate::db::HubStore,
        data_dir: &Path,
        refresh_policy: LegacyAuthRefreshPolicy,
        build_auth: impl FnOnce(
            &OwnerTokens,
            &crate::db::TeslaMateLegacyTokenStore,
        ) -> Result<LegacyAuth, LegacyAuthError>,
    ) -> Result<Self, LegacyAuthManagerError> {
        let stored = store
            .load_teslamate_legacy_tokens()
            .map_err(CredentialError::TeslaMateTokenStore)?
            .ok_or(CredentialError::TeslaMateTokenStoreMissing)?;
        if store
            .has_unresolved_legacy_refresh()
            .map_err(CredentialError::TeslaMateTokenStore)?
        {
            return Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown,
            ));
        }
        let encryption_key = Arc::new(
            crate::teslamate_credentials::load_key_for_tokens(data_dir, &stored)
                .map_err(CredentialError::TeslaMateCredentialFile)?,
        );
        let tokens = crate::teslamate_token::decrypt_legacy_owner_tokens(
            encryption_key.as_bytes(),
            stored.access(),
            stored.refresh(),
        )
        .map_err(CredentialError::TeslaMateTokenCipher)?;
        let credential_generation =
            crate::teslamate_token::legacy_refresh_credential_generation(&tokens);
        let auth = build_auth(&tokens, &stored)?;
        store
            .bind_teslamate_legacy_credential_generation(&stored, credential_generation)
            .map_err(CredentialError::TeslaMateTokenStore)?;
        Ok(Self {
            auth,
            persistence: LegacyAuthPersistence::HubTeslaMate(Arc::new(HubTeslaMatePersistence {
                store,
                encryption_key,
                credential_generation: Mutex::new(credential_generation),
            })),
            refresh_policy,
            sensitive_access: permitted_sensitive_access(),
            refresh_terminal: false,
        })
    }

    #[cfg(unix)]
    fn with_runtime_admission(
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
    pub(crate) fn from_hub_teslamate_store_with_issuer(
        store: crate::db::HubStore,
        data_dir: &Path,
        issuer: url::Url,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_teslamate_store_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Managed,
            move |tokens, stored| {
                if stored.expires_at() <= 0
                    || stored.next_refresh_at() <= 0
                    || stored.next_refresh_at() >= stored.expires_at()
                {
                    return Err(LegacyAuthError::InvalidPersistedSchedule);
                }
                Ok(
                    LegacyAuth::for_test(issuer, tokens.access_token(), tokens.refresh_token())
                        .with_test_schedule(stored.expires_at(), stored.next_refresh_at()),
                )
            },
        )
    }
    #[cfg(test)]
    pub(crate) fn from_hub_teslamate_store_observer_with_issuer(
        store: crate::db::HubStore,
        data_dir: &Path,
        issuer: url::Url,
    ) -> Result<Self, LegacyAuthManagerError> {
        Self::from_hub_teslamate_store_with_policy(
            store,
            data_dir,
            LegacyAuthRefreshPolicy::Observer,
            move |tokens, stored| {
                if stored.expires_at() <= 0
                    || stored.next_refresh_at() <= 0
                    || stored.next_refresh_at() >= stored.expires_at()
                {
                    return Err(LegacyAuthError::InvalidPersistedSchedule);
                }
                Ok(
                    LegacyAuth::for_test(issuer, tokens.access_token(), tokens.refresh_token())
                        .with_test_schedule(stored.expires_at(), stored.next_refresh_at()),
                )
            },
        )
    }
    #[cfg(test)]
    pub(crate) fn access_token(&self) -> &str {
        self.auth.access_token()
    }
    pub(crate) fn assert_sensitive_access(&self) -> Result<(), LegacyAuthManagerError> {
        (self.sensitive_access)().map_err(Into::into)
    }
    pub(crate) fn access_token_for_sensitive_use(&self) -> Result<&str, LegacyAuthManagerError> {
        self.assert_sensitive_access()?;
        Ok(self.auth.access_token())
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
    pub(crate) async fn refresh_if_due(
        &mut self,
        client: &Client,
        now: SystemTime,
    ) -> Result<(), LegacyAuthManagerError> {
        self.refresh(client, now, false).await
    }
    pub(crate) async fn refresh_now(
        &mut self,
        client: &Client,
        now: SystemTime,
    ) -> Result<(), LegacyAuthManagerError> {
        self.refresh(client, now, true).await
    }
    async fn refresh(
        &mut self,
        client: &Client,
        now: SystemTime,
        force: bool,
    ) -> Result<(), LegacyAuthManagerError> {
        if self.refresh_terminal {
            return Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown,
            ));
        }
        if self.refresh_policy == LegacyAuthRefreshPolicy::Observer {
            return if force {
                Err(LegacyAuthManagerError::ObserverRefreshDisabled)
            } else {
                Ok(())
            };
        }
        self.assert_sensitive_access()?;
        #[cfg(test)]
        if let LegacyAuthPersistence::TestCallback(persist) = &self.persistence {
            let persist = Arc::clone(persist);
            let sensitive_access = Arc::clone(&self.sensitive_access);
            return self
                .auth
                .refresh_persisted_with_bound_audit_guarded(
                    client,
                    now,
                    force,
                    move || {
                        sensitive_access().map_err(|_| LegacyAuthError::SensitiveAccessUnavailable)
                    },
                    move |a, r, _, _| persist(a, r).map_err(|_| LegacyAuthError::Persistence),
                )
                .await
                .map_err(Into::into);
        }
        let persistence = match &self.persistence {
            LegacyAuthPersistence::HubTeslaMate(persistence) => Arc::clone(persistence),
            #[cfg(test)]
            LegacyAuthPersistence::TestCallback(_) => unreachable!("handled above"),
        };
        let sensitive_access = Arc::clone(&self.sensitive_access);
        let attempt = Arc::new(Mutex::new(None));
        let begin_attempt = Arc::clone(&attempt);
        let persist_attempt = Arc::clone(&attempt);
        let begin_persistence = Arc::clone(&persistence);
        let persist_persistence = Arc::clone(&persistence);
        let result = self
            .auth
            .refresh_persisted_with_bound_audit_guarded(
                client,
                now,
                force,
                move || {
                    sensitive_access().map_err(|_| LegacyAuthError::SensitiveAccessUnavailable)?;
                    let (receipt_id, input_generation) = begin_persistence
                        .begin_refresh()
                        .map_err(|error| match error {
                            CredentialError::TeslaMateTokenStore(
                                crate::db::StoreError::LegacyRefreshOutcomeUnknown,
                            ) => LegacyAuthError::SensitiveRotationOutcomeUnknown,
                            _ => LegacyAuthError::Persistence,
                        })?;
                    if sensitive_access().is_err() {
                        if begin_persistence
                            .cancel_refresh(receipt_id, input_generation)
                            .is_err()
                        {
                            *begin_attempt.lock().expect("refresh attempt mutex") =
                                Some((receipt_id, input_generation));
                            return Err(LegacyAuthError::SensitiveRotationOutcomeUnknown);
                        }
                        return Err(LegacyAuthError::SensitiveAccessUnavailable);
                    }
                    *begin_attempt.lock().expect("refresh attempt mutex") =
                        Some((receipt_id, input_generation));
                    Ok(())
                },
                move |a, r, expires, next| {
                    let (receipt_id, input_generation) = persist_attempt
                        .lock()
                        .expect("refresh attempt mutex")
                        .as_ref()
                        .copied()
                        .ok_or(LegacyAuthError::Persistence)?;
                    persist_persistence
                        .persist_refreshed(receipt_id, input_generation, a, r, expires, next)
                        .map_err(|_| LegacyAuthError::Persistence)
                },
            )
            .await;
        let attempt_started = attempt.lock().expect("refresh attempt mutex").is_some();
        if attempt_started && result.is_err() {
            self.refresh_terminal = true;
            return Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown,
            ));
        }
        result.map_err(Into::into)
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
            refresh_terminal: false,
        }
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
    #[cfg(test)]
    pub(crate) fn test_pair_matches(
        &self,
        access: &str,
        refresh: &str,
    ) -> Result<bool, LegacyAuthManagerError> {
        Ok(self.auth.access_token() == access && self.auth.refresh_token() == refresh)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TeslaMatePostgresPassword(Zeroizing<String>);
impl TeslaMatePostgresPassword {
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
impl std::fmt::Debug for TeslaMatePostgresPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TeslaMatePostgresPassword([redacted])")
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
    #[error("TeslaMate PostgreSQL password is empty")]
    EmptyPostgresPassword,
    #[error("TeslaMate PostgreSQL password exceeds the size limit")]
    PostgresPasswordTooLarge,
    #[error("TeslaMate PostgreSQL password contains unsupported bytes")]
    InvalidPostgresPasswordBytes,
    #[error("TeslaMate PostgreSQL password is not UTF-8")]
    InvalidPostgresPasswordEncoding,
    #[error("Hub TeslaMate token pair is missing")]
    TeslaMateTokenStoreMissing,
    #[error("Hub TeslaMate token store failed: {0}")]
    TeslaMateTokenStore(#[source] crate::db::StoreError),
    #[error("Hub TeslaMate key file failed: {0}")]
    TeslaMateCredentialFile(#[source] crate::teslamate_credentials::TeslaMateCredentialError),
    #[error("TeslaMate token operation failed: {0}")]
    TeslaMateTokenCipher(#[source] crate::teslamate_token::TeslaMateTokenError),
    #[error("Hub runtime admission is unavailable")]
    SensitiveAccessUnavailable,
    #[cfg(test)]
    #[error("test sensitive access check failed")]
    MacKeychainHelperInvalid,
    #[cfg(test)]
    #[error("test persistence failed")]
    LegacyTokenStateWrite,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_tokens_are_redacted_and_zeroizable_on_drop() {
        fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

        let access = "owner-access-secret";
        let refresh = "owner-refresh-secret";
        let mut tokens =
            OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned()).unwrap();
        let debug = format!("{tokens:?}");
        assert!(!debug.contains(access));
        assert!(!debug.contains(refresh));
        assert_zeroize_on_drop::<OwnerTokens>();

        tokens.zeroize();
        assert!(tokens.access_token().bytes().all(|byte| byte == 0));
        assert!(tokens.refresh_token().bytes().all(|byte| byte == 0));
    }

    #[test]
    fn owner_token_files_accept_one_line_ending_and_enforce_semantic_bounds() {
        let access = [vec![b'a'; MAX_TOKEN_BYTES], b"\r\n".to_vec()].concat();
        let refresh = [b"refresh".as_slice(), b"\n".as_slice()].concat();
        let tokens = OwnerTokens::from_file_bytes(Zeroizing::new(access), Zeroizing::new(refresh))
            .expect("bounded token files");
        assert_eq!(tokens.access_token().len(), MAX_TOKEN_BYTES);
        assert_eq!(tokens.refresh_token(), "refresh");

        assert!(matches!(
            OwnerTokens::from_file_bytes(
                Zeroizing::new(vec![b'a'; MAX_TOKEN_BYTES + 1]),
                Zeroizing::new(b"refresh".to_vec()),
            ),
            Err(CredentialError::TokenTooLarge)
        ));
        assert!(matches!(
            OwnerTokens::from_file_bytes(
                Zeroizing::new(vec![0xff]),
                Zeroizing::new(b"refresh".to_vec()),
            ),
            Err(CredentialError::InvalidTokenBytes)
        ));
    }

    #[tokio::test]
    async fn observer_never_posts_a_refresh_token() {
        let data = crate::private_tempdir().expect("data directory");
        let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
        crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key")
            .expect("private key");
        let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
        let tokens = OwnerTokens::from_secret_parts(
            "observer-access".to_owned(),
            "observer-refresh".to_owned(),
        )
        .expect("observer tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
                .expect("encrypt observer tokens");
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    access,
                    refresh,
                    2_000_000_000,
                    1_900_000_000,
                )
                .expect("schedule"),
            )
            .expect("store observer tokens");

        let fake = crate::fake_tesla::FakeTeslaSource::spawn_canonical(
            crate::fake_tesla::AdvanceMode::Manual,
        )
        .await
        .expect("fake Tesla");
        let mut manager = LegacyAuthManager::from_hub_teslamate_store_observer_with_issuer(
            store,
            data.path(),
            fake.oauth_issuer_url(),
        )
        .expect("load observer");

        crate::crypto::install_default_provider();
        manager
            .refresh_if_due(&Client::new(), SystemTime::now())
            .await
            .expect("observer skips scheduled refresh");
        assert!(matches!(
            manager.refresh_now(&Client::new(), SystemTime::now()).await,
            Err(LegacyAuthManagerError::ObserverRefreshDisabled)
        ));
        assert_eq!(fake.token_refresh_request_count(), 0);
        fake.shutdown().await;
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn replaced_runtime_admission_blocks_refresh_before_token_transport() {
        let root = crate::private_tempdir().expect("test root");
        let data_dir = root.path().join("data");
        std::fs::create_dir(&data_dir).expect("data directory");
        std::fs::set_permissions(
            &data_dir,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("private data directory");
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(&data_dir)
            .expect("admit data directory");
        let fake = crate::fake_tesla::FakeTeslaSource::spawn_canonical(
            crate::fake_tesla::AdvanceMode::Manual,
        )
        .await
        .expect("fake Tesla");
        let auth = LegacyAuth::for_test(
            fake.oauth_issuer_url(),
            "admitted-access",
            "admitted-refresh",
        );
        let mut manager = LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())))
            .with_runtime_admission(admission);

        std::fs::rename(&data_dir, root.path().join("replaced-data"))
            .expect("replace admitted directory");
        std::fs::create_dir(&data_dir).expect("replacement directory");

        crate::crypto::install_default_provider();
        assert!(matches!(
            manager.refresh_now(&Client::new(), SystemTime::now()).await,
            Err(LegacyAuthManagerError::Credential(
                CredentialError::SensitiveAccessUnavailable
            ))
        ));
        assert_eq!(fake.token_refresh_request_count(), 0);
        fake.shutdown().await;
    }

    #[tokio::test]
    async fn hub_teslamate_store_refreshes_and_reopens_with_the_successor() {
        let data = crate::private_tempdir().expect("data directory");
        let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
        crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key")
            .expect("private key");
        let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
        let initial = OwnerTokens::from_secret_parts(
            "initial-access".to_owned(),
            "initial-refresh".to_owned(),
        )
        .expect("initial tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
                .expect("encrypt initial tokens");
        let stored = crate::db::TeslaMateLegacyTokenStore::refreshed(
            access,
            refresh,
            2_000_000_000,
            1_900_000_000,
        )
        .expect("initial schedule");
        store
            .replace_teslamate_legacy_tokens(&stored)
            .expect("store initial pair");

        let fake = crate::fake_tesla::FakeTeslaSource::spawn_canonical(
            crate::fake_tesla::AdvanceMode::Manual,
        )
        .await
        .expect("fake Tesla");
        let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store.clone(),
            data.path(),
            fake.oauth_issuer_url(),
        )
        .expect("load initial pair");
        let mut stale_manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store.clone(),
            data.path(),
            fake.oauth_issuer_url(),
        )
        .expect("load second initial authority");
        crate::crypto::install_default_provider();
        manager
            .refresh_now(&Client::new(), SystemTime::now())
            .await
            .expect("refresh and persist successor");
        assert_eq!(fake.token_refresh_request_count(), 1);
        let expected_expires = manager.auth.expires_at();
        let expected_next_refresh = manager.next_refresh_at();

        let stored = store
            .load_teslamate_legacy_tokens()
            .expect("load stored pair")
            .expect("stored pair");
        let decrypted = crate::teslamate_token::decrypt_legacy_owner_tokens(
            key.as_bytes(),
            stored.access(),
            stored.refresh(),
        )
        .expect("decrypt successor");
        assert_eq!(
            decrypted.access_token(),
            crate::fake_tesla::FAKE_REFRESHED_ACCESS_TOKEN
        );
        assert_eq!(
            decrypted.refresh_token(),
            crate::fake_tesla::FAKE_REFRESHED_REFRESH_TOKEN
        );
        assert_eq!(stored.expires_at(), expected_expires);
        assert_eq!(stored.next_refresh_at(), expected_next_refresh);

        assert!(matches!(
            stale_manager
                .refresh_now(&Client::new(), SystemTime::now())
                .await,
            Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            ))
        ));
        assert_eq!(fake.token_refresh_request_count(), 1);
        assert!(
            !store
                .has_unresolved_legacy_refresh()
                .expect("stale authority creates no receipt")
        );
        assert_eq!(
            store
                .load_teslamate_legacy_tokens()
                .expect("load successor after stale attempt")
                .expect("successor remains")
                .credential_generation(),
            stored.credential_generation()
        );

        let reopened = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store,
            data.path(),
            fake.oauth_issuer_url(),
        )
        .expect("reopen successor");
        assert_eq!(
            reopened.access_token(),
            crate::fake_tesla::FAKE_REFRESHED_ACCESS_TOKEN
        );
        assert_eq!(
            reopened.refresh_token(),
            crate::fake_tesla::FAKE_REFRESHED_REFRESH_TOKEN
        );
        assert_eq!(reopened.next_refresh_at(), expected_next_refresh);
    }

    #[tokio::test]
    async fn runtime_replacement_after_refresh_receipt_begin_cancels_before_token_post() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let data = crate::private_tempdir().expect("data directory");
        let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
        crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key")
            .expect("private key");
        let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
        let initial = OwnerTokens::from_secret_parts(
            "initial-access".to_owned(),
            "initial-refresh".to_owned(),
        )
        .expect("initial tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
                .expect("encrypt initial tokens");
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    access,
                    refresh,
                    2_000_000_000,
                    1_900_000_000,
                )
                .expect("initial schedule"),
            )
            .expect("store initial pair");

        let fake = crate::fake_tesla::FakeTeslaSource::spawn_canonical(
            crate::fake_tesla::AdvanceMode::Manual,
        )
        .await
        .expect("fake Tesla");
        let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store.clone(),
            data.path(),
            fake.oauth_issuer_url(),
        )
        .expect("load initial pair");
        let checks = Arc::new(AtomicUsize::new(0));
        let guarded_checks = Arc::clone(&checks);
        manager.sensitive_access = Arc::new(move || {
            if guarded_checks.fetch_add(1, Ordering::SeqCst) < 2 {
                Ok(())
            } else {
                Err(CredentialError::SensitiveAccessUnavailable)
            }
        });

        crate::crypto::install_default_provider();
        assert!(matches!(
            manager.refresh_now(&Client::new(), SystemTime::now()).await,
            Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveAccessUnavailable
            ))
        ));
        assert_eq!(checks.load(Ordering::SeqCst), 3);
        assert_eq!(fake.token_refresh_request_count(), 0);
        assert!(
            !store
                .has_unresolved_legacy_refresh()
                .expect("cancelled receipt is terminal")
        );
        let generation = store
            .load_teslamate_legacy_tokens()
            .expect("tokens load")
            .expect("tokens remain")
            .credential_generation()
            .expect("generation remains");
        let receipt = store
            .begin_legacy_refresh(generation)
            .expect("input remains refreshable");
        store
            .cancel_unsent_legacy_refresh(receipt, generation)
            .expect("test cleanup");
        fake.shutdown().await;
    }

    #[tokio::test]
    async fn post_send_refresh_failure_is_terminal_until_explicit_new_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let data = crate::private_tempdir().expect("data directory");
        let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
        crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key")
            .expect("private key");
        let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
        let initial = OwnerTokens::from_secret_parts(
            "ambiguous-access".to_owned(),
            "ambiguous-refresh".to_owned(),
        )
        .expect("initial tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
                .expect("encrypt initial tokens");
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    access,
                    refresh,
                    2_000_000_000,
                    1_900_000_000,
                )
                .expect("stored initial pair"),
            )
            .expect("persist initial pair");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback refresh listener");
        let address = listener.local_addr().expect("loopback address");
        let response_task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("refresh request");
            let mut request = vec![0_u8; 16 * 1024];
            let read = socket
                .read(&mut request)
                .await
                .expect("read refresh request");
            assert!(read > 0);
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await
                .expect("malformed success response");
        });
        let issuer = url::Url::parse(&format!("http://{address}/")).expect("loopback issuer");
        let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store.clone(),
            data.path(),
            issuer.clone(),
        )
        .expect("load initial pair");
        crate::crypto::install_default_provider();
        assert!(matches!(
            manager.refresh_now(&Client::new(), SystemTime::now()).await,
            Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            ))
        ));
        response_task.await.expect("response task");
        assert!(matches!(
            manager.refresh_now(&Client::new(), SystemTime::now()).await,
            Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            ))
        ));
        assert!(matches!(
            LegacyAuthManager::from_hub_teslamate_store_with_issuer(
                store.clone(),
                data.path(),
                issuer.clone(),
            ),
            Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            ))
        ));

        let replacement =
            OwnerTokens::from_secret_parts("fresh-access".to_owned(), "fresh-refresh".to_owned())
                .expect("explicit replacement tokens");
        let replacement_generation =
            crate::teslamate_token::legacy_refresh_credential_generation(&replacement);
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &replacement)
                .expect("encrypt replacement tokens");
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    access,
                    refresh,
                    2_100_000_000,
                    2_000_000_000,
                )
                .expect("stored replacement pair")
                .with_credential_generation(replacement_generation)
                .expect("replacement generation"),
            )
            .expect("explicit replacement supersedes ambiguity");
        let recovered =
            LegacyAuthManager::from_hub_teslamate_store_with_issuer(store, data.path(), issuer)
                .expect("new credential authority starts");
        assert_eq!(recovered.access_token(), "fresh-access");
        assert_eq!(recovered.refresh_token(), "fresh-refresh");
    }

    #[test]
    fn postgres_password_parsing() {
        assert_eq!(
            TeslaMatePostgresPassword::from_bytes(b"postgres")
                .unwrap()
                .as_str(),
            "postgres"
        );
        assert_eq!(
            TeslaMatePostgresPassword::from_bytes(b"postgres\n")
                .unwrap()
                .as_str(),
            "postgres"
        );
        assert!(TeslaMatePostgresPassword::from_bytes(b"bad\n\n").is_err());
    }

    #[test]
    fn refreshed_pair_persistence_failure_is_terminal() {
        for error in [
            LegacyAuthManagerError::Auth(LegacyAuthError::Persistence),
            LegacyAuthManagerError::Auth(LegacyAuthError::SensitivePersistenceUnavailable),
        ] {
            assert!(error.is_sensitive_access_failure());
        }
    }
}
