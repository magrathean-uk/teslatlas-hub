// SPDX-License-Identifier: AGPL-3.0-only
use crate::auth::credential_envelope::{
    CredentialColumn, CredentialOrigin, CredentialSlot, EnvelopeError, EnvelopeKeys, EnvelopePlace,
    KeyId, NonceSource, RefreshOwner,
};
use crate::auth::credential_keys::KeyringHandle;
pub(crate) const MAX_CREDENTIAL_REFRESH_FENCES: usize = MAX_OUTBOUND_REQUEST_RECEIPTS as usize;
const CREDENTIAL_PAIR_COLUMNS: &str = "account_id, slot, origin, refresh_owner, generation,
    access_envelope, refresh_envelope, refresh_present, refresh_fingerprint, expires_at,
    next_refresh_at, client_id, region, migrated, written_at_ms";
/// Ciphertext custody deliberately provides neither Clone nor Display.
pub struct SealedCredentialPair {
    pub(crate) account_id: u32,
    pub(crate) slot: CredentialSlot,
    pub(crate) origin: Option<CredentialOrigin>,
    pub(crate) refresh_owner: RefreshOwner,
    pub(crate) generation: Uuid,
    pub(crate) access_envelope: Vec<u8>,
    pub(crate) refresh_envelope: Vec<u8>,
    pub(crate) refresh_present: bool,
    pub(crate) refresh_fingerprint: Option<[u8; 32]>,
    pub(crate) expires_at: i64,
    pub(crate) next_refresh_at: i64,
    pub(crate) client_id: Option<String>,
    pub(crate) region: Option<String>,
    pub(crate) migrated: bool,
    pub(crate) written_at_ms: i64,
}
impl std::fmt::Debug for SealedCredentialPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedCredentialPair")
            .field("slot", &self.slot)
            .field("origin", &self.origin)
            .field("refresh_owner", &self.refresh_owner)
            .field("refresh_present", &self.refresh_present)
            .field("expires_at", &self.expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .field("access_len", &self.access_envelope.len())
            .field("refresh_len", &self.refresh_envelope.len())
            .finish()
    }
}
#[allow(
    dead_code,
    reason = "Part S handoff API; remaining consumers land in M and V1."
)]
impl SealedCredentialPair {
    fn place(&self, column: CredentialColumn) -> EnvelopePlace {
        EnvelopePlace {
            table: 1,
            slot: self.slot,
            column,
            origin: self.origin,
            owner: self.refresh_owner,
            account_id: self.account_id,
            generation: self.generation,
        }
    }
    /// The caller reserves two invocations durably before entering this function.
    pub(crate) fn seal(
        &mut self,
        keys: &KeyringHandle,
        access: &[u8],
        refresh: &[u8],
        nonces: &mut dyn NonceSource,
    ) -> Result<(), StoreError> {
        use crate::auth::credential_envelope::{
            MAX_TOKEN_PLAINTEXT_BYTES, refresh_fingerprint, seal_value,
        };
        // Reject both lengths before either seal can draw a nonce.
        if access.len() > MAX_TOKEN_PLAINTEXT_BYTES || refresh.len() > MAX_TOKEN_PLAINTEXT_BYTES {
            return Err(StoreError::CredentialEnvelope(EnvelopeError::TokenTooLarge));
        }
        if access.is_empty()
            || self.refresh_present == refresh.is_empty()
            || (!self.refresh_present && self.origin != Some(CredentialOrigin::ProviderEntry))
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        let id = keys.keyring().current.id;
        let key = keys
            .envelope_key(id)
            .ok_or(StoreError::CredentialEnvelope(EnvelopeError::UnknownKey))?;
        let access_envelope = seal_value(
            key,
            id,
            &self.place(CredentialColumn::Access),
            access,
            nonces,
        )
        .map_err(StoreError::CredentialEnvelope)?;
        let refresh_envelope = seal_value(
            key,
            id,
            &self.place(CredentialColumn::Refresh),
            refresh,
            nonces,
        )
        .map_err(StoreError::CredentialEnvelope)?;
        self.access_envelope = access_envelope;
        self.refresh_envelope = refresh_envelope;
        self.refresh_fingerprint = self.refresh_present.then(|| refresh_fingerprint(refresh));
        Ok(())
    }
    pub(crate) fn open(
        &self,
        keys: &dyn EnvelopeKeys,
    ) -> Result<(Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>), EnvelopeError> {
        use crate::auth::credential_envelope::open_value;
        let access = open_value(
            keys,
            &self.place(CredentialColumn::Access),
            &self.access_envelope,
        )?;
        let refresh = open_value(
            keys,
            &self.place(CredentialColumn::Refresh),
            &self.refresh_envelope,
        )?;
        // R56 belongs to the vault: it validates plaintexts and compares the
        // stored refresh fingerprint in constant time with its distinct error.
        // This helper only authenticates/decrypts each envelope at its place.
        Ok((access, refresh))
    }
    fn validate(&self) -> Result<(), StoreError> {
        let fleet = self.slot == CredentialSlot::FleetApi;
        if self.account_id != 1
            || self.generation.is_nil()
            || self.written_at_ms < 0
            || fleet != (self.origin == Some(CredentialOrigin::FleetOauth))
            || (self.origin.is_none() && (!self.migrated || fleet))
            || fleet != self.client_id.is_some()
            || fleet != self.region.is_some()
            || self
                .client_id
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 255)
            || self
                .region
                .as_ref()
                .is_some_and(|v| !matches!(v.as_str(), "na" | "eu" | "cn"))
            || (!self.refresh_present && self.origin != Some(CredentialOrigin::ProviderEntry))
            || self.refresh_present != self.refresh_fingerprint.is_some()
            || !(self.slot == CredentialSlot::OwnerApi
                && self.expires_at == 0
                && self.next_refresh_at == 0
                || self.expires_at > self.next_refresh_at && self.next_refresh_at > 0)
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        credential_check_lengths(self.access_envelope.len(), self.refresh_envelope.len())?;
        if self.access_envelope.len() < 68
            || self.refresh_present != (self.refresh_envelope.len() > 67)
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        credential_check_header(
            &self.access_envelope[..39],
            &self.place(CredentialColumn::Access),
        )
        .map_err(StoreError::CredentialEnvelope)?;
        credential_check_header(
            &self.refresh_envelope[..39],
            &self.place(CredentialColumn::Refresh),
        )
        .map_err(StoreError::CredentialEnvelope)
    }
}
/// Candidate UUIDs only; no token material survives construction.
pub(crate) struct LegacyCandidates(pub Vec<Uuid>);
#[allow(
    dead_code,
    reason = "Part S handoff API; entry constructors land in V1."
)]
impl LegacyCandidates {
    pub(crate) fn for_secret_parts(access: &str, refresh: &str) -> Result<Self, StoreError> {
        Self::for_received_with_old_g(
            access.as_bytes(),
            refresh.as_bytes(),
            &mut credential_legacy_secret_generation,
        )
    }
    pub(crate) fn for_file_bytes(access: &[u8], refresh: &[u8]) -> Result<Self, StoreError> {
        use crate::auth::credential_envelope::canonical_token;
        credential_canonical_utf8(access)?;
        let canonical_refresh = credential_canonical_utf8(refresh)?;
        let mut result = Self(Vec::new());
        if canonical_refresh.is_empty() {
            return Ok(result);
        }
        if let Ok(tokens) = crate::credentials::OwnerTokens::from_file_bytes(
            Zeroizing::new(access.to_vec()),
            Zeroizing::new(refresh.to_vec()),
        ) {
            result.push_unique(
                crate::source_token_envelope::legacy_refresh_credential_generation(&tokens),
            );
        }
        result.add_canonical_forms_with_old_g(
            canonical_token(access),
            canonical_token(refresh),
            &mut credential_legacy_secret_generation,
        )?;
        Ok(result)
    }
    fn for_received_with_old_g(
        access: &[u8],
        refresh: &[u8],
        old_g: &mut dyn FnMut(&[u8], &[u8]) -> Option<Uuid>,
    ) -> Result<Self, StoreError> {
        credential_canonical_utf8(access)?;
        if credential_canonical_utf8(refresh)?.is_empty() {
            // R74: a provider entry without a refresh token has no candidate.
            return Ok(Self(Vec::new()));
        }
        let mut result = Self(Vec::new());
        if let Some(generation) = old_g(access, refresh) {
            result.push_unique(generation);
        }
        result.add_canonical_forms_with_old_g(access, refresh, old_g)?;
        Ok(result)
    }
    fn add_canonical_forms_with_old_g(
        &mut self,
        access: &[u8],
        refresh: &[u8],
        old_g: &mut dyn FnMut(&[u8], &[u8]) -> Option<Uuid>,
    ) -> Result<(), StoreError> {
        let access = credential_canonical_utf8(access)?;
        let refresh = credential_canonical_utf8(refresh)?;
        for suffix in ["", "\n", "\r\n"] {
            let candidate = Zeroizing::new(format!("{refresh}{suffix}"));
            // R74 and M2P-19: each refused kept-constructor form is skipped.
            // The test seam replaces both constructor admission and opaque OLD-G.
            if let Some(generation) = old_g(access.as_bytes(), candidate.as_bytes()) {
                self.push_unique(generation);
            }
        }
        Ok(())
    }
    fn push_unique(&mut self, generation: Uuid) {
        if !self.0.contains(&generation) {
            self.0.push(generation);
        }
    }
    pub(crate) fn for_successor(access: &str, refresh: &str) -> Result<Self, StoreError> {
        Self::for_secret_parts(access, refresh)
    }
    #[cfg(test)]
    pub(crate) fn for_secret_parts_with_old_g(
        access: &str,
        refresh: &str,
        mut old_g: impl FnMut(&[u8], &[u8]) -> Option<Uuid>,
    ) -> Result<Self, StoreError> {
        Self::for_received_with_old_g(access.as_bytes(), refresh.as_bytes(), &mut old_g)
    }
    #[cfg(test)]
    pub(crate) fn for_file_bytes_with_old_g(
        access: &[u8],
        refresh: &[u8],
        mut old_g: impl FnMut(&[u8], &[u8]) -> Option<Uuid>,
    ) -> Result<Self, StoreError> {
        Self::for_received_with_old_g(access, refresh, &mut old_g)
    }
    #[cfg(test)]
    pub(crate) fn for_successor_with_old_g(
        access: &str,
        refresh: &str,
        old_g: impl FnMut(&[u8], &[u8]) -> Option<Uuid>,
    ) -> Result<Self, StoreError> {
        Self::for_secret_parts_with_old_g(access, refresh, old_g)
    }
    fn validate(&self) -> Result<(), StoreError> {
        if self.0.len() > 8 || self.0.iter().any(Uuid::is_nil) {
            Err(StoreError::CredentialPairInvalid)
        } else {
            Ok(())
        }
    }
}
fn credential_canonical_utf8(bytes: &[u8]) -> Result<&str, StoreError> {
    std::str::from_utf8(crate::auth::credential_envelope::canonical_token(bytes))
        .map_err(|_| StoreError::CredentialPairInvalid)
}
fn credential_legacy_secret_generation(access: &[u8], refresh: &[u8]) -> Option<Uuid> {
    // The kept interface takes owned Strings, so ownership must cross that
    // opaque boundary. Preparatory copies are zeroising until that transfer;
    // from_file_bytes above accepts Zeroizing<Vec<u8>> directly.
    let mut access = Zeroizing::new(std::str::from_utf8(access).ok()?.to_owned());
    let mut refresh = Zeroizing::new(std::str::from_utf8(refresh).ok()?.to_owned());
    let tokens = crate::credentials::OwnerTokens::from_secret_parts(
        std::mem::take(&mut *access),
        std::mem::take(&mut *refresh),
    )
    .ok()?;
    Some(crate::source_token_envelope::legacy_refresh_credential_generation(&tokens))
}
#[allow(
    dead_code,
    reason = "Part S fence identity contract; conversion lands in M."
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LegacyFenceIdentity {
    Anchored,
    Opaque,
}
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part S handoff API; consumed by later parts.")
)]
pub(crate) enum EntryProvenance<'a> {
    Supplied,
    HubGrant {
        sign_in_id: Uuid,
        grant_secret: &'a Zeroizing<[u8; 32]>,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "Part S handoff API; consumed by later parts.")
)]
pub(crate) enum ReplacePolicy {
    OnlyIfEmpty,
    ReplaceAny,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RefreshCompletion {
    Completed,
    AlreadyComplete,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeginRefusal {
    RowChanged,
    AttemptInFlight,
    TokenSpent,
    FenceFull,
    NoRefreshToken,
    NoRow,
}
pub struct CredentialPairPresence {
    pub(crate) slot: CredentialSlot,
    pub(crate) origin: Option<CredentialOrigin>,
    pub(crate) refresh_owner: RefreshOwner,
    pub(crate) refresh_present: bool,
    pub(crate) expires_at: i64,
    pub(crate) next_refresh_at: i64,
    pub(crate) written_at_ms: i64,
    pub(crate) header_check: Result<(), EnvelopeError>,
}
impl std::fmt::Debug for CredentialPairPresence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialPairPresence")
            .field("slot", &self.slot)
            .field("origin", &self.origin)
            .field("refresh_owner", &self.refresh_owner)
            .field("refresh_present", &self.refresh_present)
            .field("expires_at", &self.expires_at)
            .field("next_refresh_at", &self.next_refresh_at)
            .field("written_at_ms", &self.written_at_ms)
            .field("header_check", &self.header_check)
            .finish()
    }
}
pub struct CredentialConnection {
    connection: Connection,
    database_path: PathBuf,
    barrier_directory: File,
    migration_state: CredentialMigrationRuntime,
    #[cfg(test)]
    now_ms: Option<i64>,
    #[cfg(test)]
    fence_capacity: usize,
    #[cfg(test)]
    blob_bytes_read: usize,
    #[cfg(test)]
    barrier_failure: bool,
    #[cfg(test)]
    barrier_failure_at: Option<usize>,
    #[cfg(test)]
    barrier_calls: usize,
    #[cfg(test)]
    rollback_completion: bool,
    #[cfg(test)]
    events: Vec<&'static str>,
    // Fields drop in declaration order: close SQLite before releasing the take.
    _lease: CredentialConnectionLease,
}
struct CredentialConnectionLease {
    #[cfg(test)]
    taken: std::sync::Arc<std::sync::atomic::AtomicBool>,
    admission: Option<crate::hub_user_process::CredentialVaultAdmission>,
    #[cfg(test)]
    fixture: std::sync::Arc<FixtureVault>,
}
impl Drop for CredentialConnectionLease {
    fn drop(&mut self) {
        // SQLite has already closed. Release admission before waking a reopen.
        drop(self.admission.take());
        #[cfg(test)]
        let _guard = self.fixture.release.lock().unwrap_or_else(|error| error.into_inner());
        #[cfg(test)]
        self.taken.store(false, std::sync::atomic::Ordering::Release);
        #[cfg(test)]
        self.fixture.released.notify_all();
    }
}
impl HubStore {
    /// R68's slot-only fact contains neither an identity nor a generation.
    pub(crate) fn unresolved_fleet_refresh_status(&self) -> Result<bool, StoreError> {
        self.open_read_only_connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a JOIN outbound_request_receipts r ON r.id=a.receipt_id WHERE a.account_id=1 AND a.slot='fleet_api' AND r.outcome='started')",
            [], |r| r.get(0)
        ).map_err(StoreError::CredentialRefreshReceipt)
    }
    pub fn take_credential_connection(&self) -> Result<CredentialConnection, StoreError> {
        use std::sync::atomic::Ordering;
        self.credential_connection_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| StoreError::CredentialConnectionTaken)?;
        let opened = (|| {
            let data_dir = self
                .database_path
                .parent()
                .ok_or(StoreError::InvalidCataloguePath)?;
            // R72 needs a held descriptor, not K's repairing directory opener.
            let descriptor = open(
                data_dir,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| {
                if matches!(error, Errno::LOOP | Errno::NOTDIR) {
                    StoreError::UnsafeDataDir(data_dir.to_path_buf())
                } else {
                    StoreError::InspectCatalogue(error.into())
                }
            })?;
            let facts =
                fstat(&descriptor).map_err(|error| StoreError::InspectCatalogue(error.into()))?;
            if FileType::from_raw_mode(facts.st_mode) != FileType::Directory
                || facts.st_uid != rustix::process::geteuid().as_raw()
                || facts.st_mode & 0o7777 != 0o700
            {
                return Err(StoreError::UnsafeDataDir(data_dir.to_path_buf()));
            }
            let barrier_directory = File::from(descriptor);
            let connection = self.open_for_credential_commit()?;
            connection
                .execute_batch("PRAGMA synchronous = FULL; PRAGMA secure_delete = ON;")
                .map_err(StoreError::Configure)?;
            #[cfg(target_os = "macos")]
            connection
                .execute_batch("PRAGMA fullfsync = ON; PRAGMA checkpoint_fullfsync = ON;")
                .map_err(StoreError::Configure)?;
            Ok(CredentialConnection {
                _lease: CredentialConnectionLease {
                    #[cfg(test)]
                    taken: self.credential_connection_taken.clone(),
                    admission: None,
                    #[cfg(test)]
                    fixture: self.fixture_vault.clone(),
                },
                connection,
                database_path: self.database_path.clone(),
                barrier_directory,
                migration_state: CredentialMigrationRuntime::default(),
                #[cfg(test)]
                now_ms: None,
                #[cfg(test)]
                fence_capacity: MAX_CREDENTIAL_REFRESH_FENCES,
                #[cfg(test)]
                blob_bytes_read: 0,
                #[cfg(test)]
                barrier_failure: false,
                #[cfg(test)]
                barrier_failure_at: None,
                #[cfg(test)]
                barrier_calls: 0,
                #[cfg(test)]
                rollback_completion: false,
                #[cfg(test)]
                events: Vec::new(),
            })
        })();
        if opened.is_err() {
            self.credential_connection_taken
                .store(false, Ordering::Release);
        }
        opened
    }
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Part S handoff API; consumed by later parts.")
    )]
    pub(crate) fn credential_pair_presence(
        &self,
        slot: CredentialSlot,
    ) -> Result<Option<CredentialPairPresence>, StoreError> {
        let mut connection = self.open_read_only_connection()?;
        if credential_schema_version(&connection)? < CREDENTIAL_FORMAT_SCHEMA_VERSION {
            return Ok(None);
        }
        let transaction = connection.transaction().map_err(StoreError::Begin)?;
        let metadata = transaction
            .query_row(
                "SELECT origin, refresh_owner, generation, refresh_present,
            expires_at, next_refresh_at, written_at_ms,
            length(access_envelope), length(refresh_envelope), account_id
            FROM credential_pairs WHERE account_id=1 AND slot=?1",
                [slot.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, u32>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        let Some((
            origin,
            owner,
            generation,
            refresh_present,
            expires_at,
            next_refresh_at,
            written_at_ms,
            access_len,
            refresh_len,
            account_id,
        )) = metadata
        else {
            return Ok(None);
        };
        let access_len = credential_sql_length(access_len)?;
        let refresh_len = credential_sql_length(refresh_len)?;
        let origin =
            credential_origin(origin.as_deref()).ok_or(StoreError::CredentialPairInvalid)?;
        let owner = credential_owner(&owner).ok_or(StoreError::CredentialPairInvalid)?;
        let generation =
            Uuid::parse_str(&generation).map_err(|_| StoreError::CredentialPairInvalid)?;
        use crate::auth::credential_envelope::check_envelope_length;
        let lengths =
            check_envelope_length(access_len).and_then(|()| check_envelope_length(refresh_len));
        let header_check = if let Err(error) = lengths {
            Err(error)
        } else {
            let (access, refresh): (Vec<u8>, Vec<u8>) = transaction
                .query_row(
                    "SELECT substr(access_envelope, 1, 39),
            substr(refresh_envelope, 1, 39) FROM credential_pairs WHERE
            account_id = 1 AND slot = ?1",
                    [slot.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(StoreError::CredentialPairStore)?;
            // This counter observes these instrumented reads, not arbitrary
            // queries added elsewhere; it is a deliberately bounded test seam.
            #[cfg(test)]
            self.credential_blob_bytes_read.fetch_add(
                access.len() + refresh.len(),
                std::sync::atomic::Ordering::Relaxed,
            );
            let mut place = EnvelopePlace {
                table: 1,
                slot,
                column: CredentialColumn::Access,
                origin,
                owner,
                account_id,
                generation,
            };
            credential_check_header(&access, &place).and_then(|()| {
                place.column = CredentialColumn::Refresh;
                credential_check_header(&refresh, &place)
            })
        };
        Ok(Some(CredentialPairPresence {
            slot,
            origin,
            refresh_owner: owner,
            refresh_present,
            expires_at,
            next_refresh_at,
            written_at_ms,
            header_check,
        }))
    }
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "Part S handoff API; consumed by later parts.")
    )]
    pub(crate) fn credential_refresh_requires_sign_in(&self) -> Result<bool, StoreError> {
        let connection = self.open_read_only_connection()?;
        if credential_schema_version(&connection)? < CREDENTIAL_FORMAT_SCHEMA_VERSION {
            return Ok(false);
        }
        credential_has_opaque_fence(&connection).map_err(StoreError::CredentialPairStore)
    }
    #[cfg(test)]
    pub(crate) fn test_credential_blob_bytes_read(&self) -> usize {
        self.credential_blob_bytes_read
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(test)]
    pub(crate) fn reset_test_credential_blob_bytes_read(&self) {
        self.credential_blob_bytes_read
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }
}
#[allow(
    dead_code,
    reason = "Part S handoff API; remaining callers land in M and V1."
)]
impl CredentialConnection {
    pub(crate) fn bind_vault_admission(&mut self, admission: crate::hub_user_process::CredentialVaultAdmission) {
        self._lease.admission = Some(admission);
    }
    /// R64 eligibility before any key publication, reservation or seal.
    pub(crate) fn credential_clear_needed(&mut self, slot: CredentialSlot) -> Result<bool, StoreError> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_pairs WHERE account_id=1 AND slot=?1) OR EXISTS(SELECT 1 FROM credential_refresh_attempts a JOIN outbound_request_receipts r ON r.id=a.receipt_id WHERE a.account_id=1 AND a.slot=?1 AND r.outcome='started') OR EXISTS(SELECT 1 FROM credential_format_state WHERE singleton_id=1 AND CASE ?1 WHEN 'owner_api' THEN owner_api_state='pending' AND owner_api_blocked IS NOT NULL ELSE fleet_api_state='pending' AND fleet_api_blocked IS NOT NULL END)",
            [slot.as_str()], |r| r.get(0)
        ).map_err(StoreError::CredentialPairStore)
    }
    /// E-A9: identity and every link of its chain share one read snapshot.
    pub(crate) fn guarded_collector_credential_identity(&mut self, fleet: bool) -> Result<String, StoreError> {
        let tx = self.connection.transaction().map_err(StoreError::Begin)?;
        if fleet {
            let fields = tx.query_row("SELECT client_id, region FROM credential_pairs WHERE account_id=1 AND slot='fleet_api'", [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .optional().map_err(StoreError::CredentialPairStore)?;
            return Ok(fields.map_or_else(String::new, |(client, region)| format!("{client}|{region}")));
        }
        let current = tx.query_row("SELECT generation FROM credential_pairs WHERE account_id=1 AND slot='owner_api'", [], |r| r.get::<_, String>(0))
            .optional().map_err(StoreError::CredentialPairStore)?;
        let Some(mut current) = current else { return Ok(String::new()); };
        let mut visited = std::collections::HashSet::new();
        loop {
            let generation = Uuid::parse_str(&current).map_err(|_| StoreError::CredentialPairInvalid)?;
            if !visited.insert(generation) { return Err(StoreError::CredentialPairInvalid); }
            let prior = tx.query_row("SELECT a.input_generation FROM credential_refresh_attempts a JOIN outbound_request_receipts r ON r.id=a.receipt_id WHERE a.account_id=1 AND a.slot='owner_api' AND a.output_generation=?1 AND r.outcome='success'", [&current], |r| r.get::<_, String>(0))
                .optional().map_err(StoreError::CredentialRefreshReceipt)?;
            match prior { Some(input) => current = input, None => return Ok(generation.to_string()) }
        }
    }
    pub(crate) fn inspect_fleet_refresh_receipt(&mut self) -> Result<bool, StoreError> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_pairs p JOIN credential_refresh_attempts a ON a.account_id=p.account_id AND a.slot=p.slot AND a.input_generation=p.generation JOIN outbound_request_receipts r ON r.id=a.receipt_id WHERE p.account_id=1 AND p.slot='fleet_api' AND r.outcome='started' AND (EXISTS(SELECT 1 FROM credential_refresh_fences f WHERE f.fence_id=a.fence_id AND f.kind='attempt' AND f.fingerprint=p.refresh_fingerprint) OR (a.migrated=1 AND EXISTS(SELECT 1 FROM credential_legacy_fences f WHERE f.legacy_generation=p.generation))))",
            [], |r| r.get(0)
        ).map_err(StoreError::CredentialRefreshReceipt)
    }
    pub(crate) fn fill_credential_rejected_access_digest(&mut self, generation: Uuid, digest: Uuid, fuse_generation: Option<Uuid>, consecutive: u8) -> Result<(), StoreError> {
        let changed=self.connection.execute(
            "UPDATE collector_auth_state SET fuse_rejected_access_digest=?2 WHERE singleton_id=1 AND fuse_rejected_access_digest IS NULL AND fuse_consecutive=?4 AND fuse_consecutive>=1 AND fuse_generation IS ?3 AND EXISTS(SELECT 1 FROM credential_pairs WHERE account_id=1 AND slot='owner_api' AND generation=?1)",
            params![generation.to_string(), digest.to_string(),fuse_generation.map(|g|g.to_string()),consecutive]
        ).map_err(StoreError::Query)?;
        if changed==1 {Ok(())}else{Err(StoreError::WriteRefused("credential_digest_snapshot_changed"))}
    }
    pub(crate) fn validate_credential_grant(&mut self, slot: CredentialSlot, id: Uuid, secret: &Zeroizing<[u8; 32]>) -> Result<(), StoreError> {
        let now = self.now_ms()?;
        let binding = self.connection.query_row("SELECT slot, binding, expires_at_ms FROM credential_sign_ins WHERE sign_in_id=?1", [id.to_string()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, i64>(2)?))).optional().map_err(StoreError::CredentialPairStore)?;
        let Some((stored_slot, binding, expiry)) = binding else { return Err(StoreError::SignInBindingInvalid); };
        if stored_slot != slot.as_str() || !crate::protocol::constant_time_eq(&binding, &credential_sign_in_binding(secret)) { return Err(StoreError::SignInBindingInvalid); }
        if expiry <= now { return Err(StoreError::SignInExpired); }
        Ok(())
    }
    pub(crate) fn check_supplied_credential_entry(&mut self, slot: CredentialSlot, refresh: &[u8], legacy: &LegacyCandidates, policy: ReplacePolicy) -> Result<(), StoreError> {
        let tx = self.connection.transaction().map_err(StoreError::Begin)?;
        if policy == ReplacePolicy::OnlyIfEmpty && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_pairs WHERE account_id=1 AND slot=?1)", [slot.as_str()], |r| r.get::<_, bool>(0)
        ).map_err(StoreError::CredentialPairStore)? { return Err(StoreError::SlotOccupied); }
        let fingerprint = (!refresh.is_empty()).then(|| crate::auth::credential_envelope::refresh_fingerprint(refresh));
        if credential_token_spent(&tx, fingerprint.as_ref(), legacy)? { return Err(StoreError::RefreshTokenSpent); }
        if !refresh.is_empty() && credential_has_opaque_fence(&tx).map_err(StoreError::CredentialPairStore)? { return Err(StoreError::FreshSignInRequired); }
        Ok(())
    }
    pub(crate) fn credential_reserved_invocations(&mut self, key: KeyId) -> Result<u64, StoreError> {
        let used = self.connection.query_row("SELECT reserved_invocations FROM credential_key_usage WHERE key_id=?1", [key.0.as_slice()], |r| r.get::<_, i64>(0))
            .optional().map_err(StoreError::CredentialPairStore)?.unwrap_or(0);
        u64::try_from(used).map_err(|_| StoreError::InvalidStoredCount)
    }
    pub(crate) fn credential_refresh_is_complete(&mut self, slot: CredentialSlot, receipt: OutboundRequestReceiptId, input: Uuid, output: Uuid) -> Result<bool, StoreError> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a JOIN outbound_request_receipts r ON r.id=a.receipt_id JOIN credential_pairs p ON p.account_id=a.account_id AND p.slot=a.slot WHERE a.account_id=1 AND a.slot=?1 AND a.receipt_id=?2 AND a.input_generation=?3 AND a.output_generation=?4 AND p.generation=?4 AND r.outcome='success')",
            params![slot.as_str(), receipt.0, input.to_string(), output.to_string()], |r| r.get(0)
        ).map_err(StoreError::CredentialRefreshReceipt)
    }
    fn now_ms(&self) -> Result<i64, StoreError> {
        #[cfg(test)]
        if let Some(now) = self.now_ms {
            return Ok(now);
        }
        outbound_request_clock_ms()
    }
    fn capacity(&self) -> usize {
        #[cfg(test)]
        {
            self.fence_capacity
        }
        #[cfg(not(test))]
        {
            MAX_CREDENTIAL_REFRESH_FENCES
        }
    }
    pub(crate) fn replace_credential_pair(
        &mut self,
        pair: &SealedCredentialPair,
        provenance: &EntryProvenance<'_>,
        legacy: &LegacyCandidates,
        policy: ReplacePolicy,
    ) -> Result<(), StoreError> {
        self.replace_credential_pair_with_commit(pair, provenance, legacy, policy, &mut || {})
    }

    /// TRANSITIONAL: R55 accounting is supplied by the vault at the cut point.
    pub(crate) fn replace_credential_pair_with_commit(
        &mut self,
        pair: &SealedCredentialPair,
        provenance: &EntryProvenance<'_>,
        legacy: &LegacyCandidates,
        policy: ReplacePolicy,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        if self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        pair.validate()?;
        legacy.validate()?;
        let now = self.now_ms()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if credential_migration_check_slot_write(&tx, pair.slot)?
            && !self.migration_state.conversion_barrier_passed
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        if let EntryProvenance::HubGrant {
            sign_in_id,
            grant_secret,
        } = provenance
        {
            let binding = tx
                .query_row(
                    "SELECT slot, binding, expires_at_ms FROM credential_sign_ins
            WHERE sign_in_id=?1",
                    [sign_in_id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Vec<u8>>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(StoreError::CredentialPairStore)?;
            let Some((slot, stored, expiry)) = binding else {
                return Err(StoreError::SignInBindingInvalid);
            };
            let expected = credential_sign_in_binding(grant_secret);
            let expected_origin = match pair.slot {
                CredentialSlot::OwnerApi => CredentialOrigin::HubLogin,
                CredentialSlot::FleetApi => CredentialOrigin::FleetOauth,
            };
            if slot != pair.slot.as_str()
                || !crate::protocol::constant_time_eq(&stored, &expected)
                || pair.origin != Some(expected_origin)
            {
                return Err(StoreError::SignInBindingInvalid);
            }
            if expiry <= now {
                return Err(StoreError::SignInExpired);
            }
        } else if pair.origin == Some(CredentialOrigin::HubLogin) {
            return Err(StoreError::InvalidOrigin);
        }
        if policy == ReplacePolicy::OnlyIfEmpty
            && tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM credential_pairs WHERE
            account_id=1 AND slot=?1)",
                    [pair.slot.as_str()],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(StoreError::CredentialPairStore)?
        {
            return Err(StoreError::SlotOccupied);
        }
        if credential_token_spent(&tx, pair.refresh_fingerprint.as_ref(), legacy)? {
            return Err(StoreError::RefreshTokenSpent);
        }
        if matches!(provenance, EntryProvenance::Supplied)
            && pair.refresh_present
            && credential_has_opaque_fence(&tx).map_err(StoreError::CredentialPairStore)?
        {
            return Err(StoreError::FreshSignInRequired);
        }
        credential_cancel_slot(&tx, pair.slot, now)?;
        credential_write_pair(&tx, pair, now)?;
        credential_migration_discard_blocked_slot(&tx, pair.slot, now)?;
        if let EntryProvenance::HubGrant { sign_in_id, .. } = provenance {
            tx.execute(
                "DELETE FROM credential_sign_ins WHERE sign_in_id=?1",
                [sign_in_id.to_string()],
            )
            .map_err(StoreError::CredentialPairStore)?;
        }
        before_commit();
        tx.commit().map_err(StoreError::CredentialCommit)?;
        self.migration_state.vacuum_succeeded = false;
        Ok(())
    }
    pub(crate) fn load_credential_pair(
        &mut self,
        slot: CredentialSlot,
    ) -> Result<Option<SealedCredentialPair>, StoreError> {
        let tx = self.connection.transaction().map_err(StoreError::Begin)?;
        let lengths = tx
            .query_row(
                "SELECT length(access_envelope), length(refresh_envelope) FROM
            credential_pairs WHERE account_id=1 AND slot=?1",
                [slot.as_str()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        let Some((access_len, refresh_len)) = lengths else {
            return Ok(None);
        };
        let access_len = credential_sql_length(access_len)?;
        let refresh_len = credential_sql_length(refresh_len)?;
        credential_check_lengths(access_len, refresh_len)?;
        let query = format!(
            "SELECT {CREDENTIAL_PAIR_COLUMNS} FROM credential_pairs WHERE
            account_id=1 AND slot=?1"
        );
        let pair = tx
            .query_row(&query, [slot.as_str()], credential_pair_from_row)
            .map_err(StoreError::CredentialPairStore)?;
        // Counts this blob fetch after the length gate; it does not observe
        // arbitrary uninstrumented SQLite queries (S-OR-12's seam limit).
        #[cfg(test)]
        {
            self.blob_bytes_read += access_len + refresh_len;
        }
        pair.validate()?;
        Ok(Some(pair))
    }
    pub(crate) fn begin_credential_refresh(
        &mut self,
        slot: CredentialSlot,
        input_generation: Uuid,
    ) -> Result<OutboundRequestReceiptId, StoreError> {
        self.begin_credential_refresh_with_commit(slot, input_generation, &mut || {})
    }

    /// R30: the hook observes only a COMMIT that is about to be issued.
    pub(crate) fn begin_credential_refresh_with_commit(
        &mut self,
        slot: CredentialSlot,
        input_generation: Uuid,
        before_commit: &mut dyn FnMut(),
    ) -> Result<OutboundRequestReceiptId, StoreError> {
        let now = self.now_ms()?;
        let capacity = self.capacity();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let row = tx
            .query_row(
                "SELECT generation, refresh_present, refresh_fingerprint FROM
            credential_pairs WHERE account_id=1 AND slot=?1",
                [slot.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        let Some((generation, present, fingerprint)) = row else {
            return Err(credential_begin_refusal(BeginRefusal::NoRow));
        };
        if generation != input_generation.to_string() {
            return Err(credential_begin_refusal(BeginRefusal::RowChanged));
        }
        if !present {
            return Err(credential_begin_refusal(BeginRefusal::NoRefreshToken));
        }
        if credential_unresolved(&tx, slot).map_err(StoreError::CredentialRefreshReceipt)? {
            return Err(credential_begin_refusal(BeginRefusal::AttemptInFlight));
        }
        let fingerprint = fingerprint.ok_or(StoreError::CredentialPairInvalid)?;
        let spent = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credential_refresh_fences WHERE
            fingerprint=?1) OR EXISTS(SELECT 1 FROM
            credential_legacy_fences WHERE legacy_generation=?2 COLLATE
            NOCASE)",
                params![fingerprint, generation],
                |row| row.get::<_, bool>(0),
            )
            .map_err(StoreError::CredentialRefreshReceipt)?;
        if spent {
            return Err(credential_begin_refusal(BeginRefusal::TokenSpent));
        }
        let count: i64 = tx
            .query_row(
                "SELECT (SELECT count(*) FROM
            credential_refresh_fences)+(SELECT count(*) FROM
            credential_legacy_fences)",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::CredentialRefreshReceipt)?;
        let count = usize::try_from(count).map_err(|_| StoreError::InvalidStoredCount)?;
        if count >= capacity {
            return Err(credential_begin_refusal(BeginRefusal::FenceFull));
        }
        ensure_outbound_request_capacity(&tx)?;
        let attempt_id = Uuid::new_v4();
        tx.execute(
            "INSERT INTO outbound_request_receipts(correlation_id,
            started_at_ms, vehicle_tesla_id, transport, operation,
            safety_class, precondition, outcome) VALUES(?1, ?2, NULL, ?3,
            'token_refresh', 'non_wake_endpoint', 'not_required',
            'started')",
            params![
                attempt_id.to_string(),
                now,
                if slot == CredentialSlot::OwnerApi {
                    "legacy_auth"
                } else {
                    "fleet_api"
                }
            ],
        )
        .map_err(StoreError::CredentialRefreshReceipt)?;
        let receipt = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO credential_refresh_fences(fingerprint, kind,
            fenced_at_ms) VALUES(?1, 'attempt', ?2)",
            params![fingerprint, now],
        )
        .map_err(StoreError::CredentialRefreshReceipt)?;
        let fence = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO credential_refresh_attempts(receipt_id,
            attempt_id, account_id, slot, input_generation, fence_id,
            output_generation, migrated) VALUES(?1, ?2, 1, ?3, ?4, ?5,
            NULL, 0)",
            params![
                receipt,
                attempt_id.to_string(),
                slot.as_str(),
                generation,
                fence
            ],
        )
        .map_err(StoreError::CredentialRefreshReceipt)?;
        #[cfg(test)]
        record_credential_commit_pragmas(&tx)?;
        before_commit();
        tx.commit().map_err(StoreError::CredentialCommit)?;
        Ok(OutboundRequestReceiptId(receipt))
    }
    pub(crate) fn complete_credential_refresh(
        &mut self,
        receipt: OutboundRequestReceiptId,
        input_generation: Uuid,
        successor: &SealedCredentialPair,
        legacy: &LegacyCandidates,
    ) -> Result<RefreshCompletion, StoreError> {
        self.complete_credential_refresh_with_commit(
            receipt, input_generation, successor, legacy, &mut || {},
        )
    }

    /// R31: refusals and AlreadyComplete do not issue a counted COMMIT.
    pub(crate) fn complete_credential_refresh_with_commit(
        &mut self,
        receipt: OutboundRequestReceiptId,
        input_generation: Uuid,
        successor: &SealedCredentialPair,
        legacy: &LegacyCandidates,
        before_commit: &mut dyn FnMut(),
    ) -> Result<RefreshCompletion, StoreError> {
        successor.validate()?;
        legacy.validate()?;
        let now = self.now_ms()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let attempt = credential_attempt(&tx, receipt)?;
        let Some(attempt) = attempt else {
            return Err(credential_begin_refusal(BeginRefusal::RowChanged));
        };
        let current = tx
            .query_row(
                "SELECT generation, origin, refresh_owner, client_id, region,
            migrated FROM credential_pairs WHERE account_id=1 AND slot=?1",
                [attempt.slot.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        let Some((current_generation, origin, owner, client, region, migrated)) = current else {
            return Err(credential_begin_refusal(BeginRefusal::RowChanged));
        };
        if attempt.outcome == "success"
            && attempt.input == input_generation.to_string()
            && attempt.slot == successor.slot
            && attempt.output.as_deref() == Some(successor.generation.to_string().as_str())
            && current_generation == successor.generation.to_string()
        {
            return Ok(RefreshCompletion::AlreadyComplete);
        }
        let output_used: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts WHERE
            slot=?1 AND output_generation=?2)",
                params![attempt.slot.as_str(), successor.generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::CredentialRefreshReceipt)?;
        if attempt.outcome != "started"
            || attempt.migrated
            || attempt.input != input_generation.to_string()
            || current_generation != input_generation.to_string()
            || successor.slot != attempt.slot
            || successor.generation == input_generation
            || output_used
        {
            return Err(credential_begin_refusal(BeginRefusal::RowChanged));
        }
        if successor.origin.map(CredentialOrigin::as_str) != origin.as_deref()
            || successor.refresh_owner.as_str() != owner
            || successor.client_id != client
            || successor.region != region
            || successor.migrated != migrated
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        let input_fingerprint = attempt
            .fence_id
            .map(|fence| {
                tx.query_row(
                    "SELECT fingerprint FROM credential_refresh_fences WHERE
            fence_id=?1 AND kind='attempt'",
                    [fence],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
            })
            .transpose()
            .map_err(StoreError::CredentialRefreshReceipt)?
            .flatten();
        let unrotated = successor
            .refresh_fingerprint
            .as_ref()
            .is_some_and(|f| input_fingerprint.as_deref() == Some(f.as_slice()));
        if unrotated {
            credential_release_attempt_fence(&tx, receipt, &attempt)?;
        } else if credential_token_spent(&tx, successor.refresh_fingerprint.as_ref(), legacy)? {
            return Err(StoreError::RefreshTokenSpent);
        }
        credential_write_pair(&tx, successor, now)?;
        tx.execute(
            "UPDATE credential_refresh_attempts SET output_generation=?2
            WHERE receipt_id=?1",
            params![receipt.0, successor.generation.to_string()],
        )
        .map_err(StoreError::CredentialRefreshReceipt)?;
        credential_close_receipt(
            &tx,
            receipt,
            now,
            OutboundRequestOutcome::Success,
            Some(200),
            None,
        )?;
        #[cfg(test)]
        if self.rollback_completion {
            return Err(StoreError::WriteRefused("credential_completion_rollback"));
        }
        #[cfg(test)]
        record_credential_commit_pragmas(&tx)?;
        before_commit();
        tx.commit().map_err(StoreError::CredentialCommit)?;
        Ok(RefreshCompletion::Completed)
    }
    pub(crate) fn cancel_unsent_credential_refresh(
        &mut self,
        receipt: OutboundRequestReceiptId,
        input_generation: Uuid,
    ) -> Result<(), StoreError> {
        self.cancel_unsent_credential_refresh_with_commit(receipt, input_generation, &mut || {})
    }

    pub(crate) fn cancel_unsent_credential_refresh_with_commit(
        &mut self,
        receipt: OutboundRequestReceiptId,
        input_generation: Uuid,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        self.close_credential_refresh_attempt_inner(
            receipt,
            input_generation,
            OutboundRequestOutcome::Cancelled,
            None,
            None,
            true,
            before_commit,
        )
    }
    pub(crate) fn close_credential_refresh_attempt(
        &mut self,
        receipt: OutboundRequestReceiptId,
        input_generation: Uuid,
        outcome: OutboundRequestOutcome,
        http_status: Option<u16>,
        retry_after_seconds: Option<u64>,
        release_input: bool,
    ) -> Result<(), StoreError> {
        self.close_credential_refresh_attempt_with_commit(
            receipt, input_generation, outcome, http_status, retry_after_seconds,
            release_input, &mut || {},
        )
    }

    pub(crate) fn close_credential_refresh_attempt_with_commit(
        &mut self,
        receipt: OutboundRequestReceiptId,
        input_generation: Uuid,
        outcome: OutboundRequestOutcome,
        http_status: Option<u16>,
        retry_after_seconds: Option<u64>,
        release_input: bool,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        if !matches!(
            outcome,
            OutboundRequestOutcome::Success
                | OutboundRequestOutcome::HttpError
                | OutboundRequestOutcome::AuthenticationRejected
                | OutboundRequestOutcome::TransportError
                | OutboundRequestOutcome::Timeout
        ) {
            return Err(StoreError::CredentialPairInvalid);
        }
        self.close_credential_refresh_attempt_inner(
            receipt,
            input_generation,
            outcome,
            http_status,
            retry_after_seconds,
            release_input,
            before_commit,
        )
    }
    fn close_credential_refresh_attempt_inner(
        &mut self,
        receipt: OutboundRequestReceiptId,
        input_generation: Uuid,
        outcome: OutboundRequestOutcome,
        http_status: Option<u16>,
        retry_after_seconds: Option<u64>,
        release_input: bool,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        let completion = OutboundRequestCompletion {
            outcome,
            http_status,
            retry_after_seconds,
        };
        completion.validate()?;
        let now = self.now_ms()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let attempt = credential_attempt(&tx, receipt)?
            .ok_or_else(|| credential_begin_refusal(BeginRefusal::RowChanged))?;
        if attempt.outcome != "started" || attempt.input != input_generation.to_string() {
            return Err(credential_begin_refusal(BeginRefusal::RowChanged));
        }
        if release_input && !attempt.migrated {
            let releasable = attempt
                .fence_id
                .map(|id| {
                    tx.query_row(
                        "SELECT kind='attempt' FROM credential_refresh_fences WHERE
            fence_id=?1",
                        [id],
                        |row| row.get::<_, bool>(0),
                    )
                    .optional()
                })
                .transpose()
                .map_err(StoreError::CredentialRefreshReceipt)?
                .flatten()
                .unwrap_or(false);
            if releasable {
                tx.execute(
                    "DELETE FROM credential_refresh_attempts WHERE receipt_id=?1",
                    [receipt.0],
                )
                .map_err(StoreError::CredentialRefreshReceipt)?;
                if let Some(fence) = attempt.fence_id {
                    tx.execute(
                        "DELETE FROM credential_refresh_fences WHERE fence_id=?1 AND
            kind='attempt'",
                        [fence],
                    )
                    .map_err(StoreError::CredentialRefreshReceipt)?;
                }
            }
        }
        credential_close_receipt(&tx, receipt, now, outcome, http_status, retry_after_seconds)?;
        before_commit();
        tx.commit().map_err(StoreError::CredentialCommit)
    }
    pub(crate) fn has_unresolved_credential_refresh(
        &mut self,
        slot: CredentialSlot,
    ) -> Result<bool, StoreError> {
        credential_unresolved(&self.connection, slot).map_err(StoreError::CredentialRefreshReceipt)
    }
    pub(crate) fn clear_credential_pair(&mut self, slot: CredentialSlot) -> Result<(), StoreError> {
        self.clear_credential_pair_with_commit(slot, &mut || {})
    }

    pub(crate) fn clear_credential_pair_with_commit(
        &mut self,
        slot: CredentialSlot,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        if self.migration_state.barrier_failed {
            return Err(StoreError::CredentialPairInvalid);
        }
        let now = self.now_ms()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if credential_migration_check_slot_write(&tx, slot)?
            && !self.migration_state.conversion_barrier_passed
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        tx.execute(
            "DELETE FROM credential_pairs WHERE account_id=1 AND slot=?1",
            [slot.as_str()],
        )
        .map_err(StoreError::CredentialPairStore)?;
        credential_cancel_slot(&tx, slot, now)?;
        credential_migration_discard_blocked_slot(&tx, slot, now)?;
        before_commit();
        tx.commit().map_err(StoreError::CredentialCommit)?;
        self.migration_state.vacuum_succeeded = false;
        Ok(())
    }
    pub(crate) fn set_credential_refresh_owner(
        &mut self,
        pair: &SealedCredentialPair,
        expected_generation: Uuid,
    ) -> Result<(), StoreError> {
        self.set_credential_refresh_owner_with_commit(pair, expected_generation, &mut || {})
    }

    /// TRANSITIONAL: R36's counted COMMIT is observed by the vault.
    pub(crate) fn set_credential_refresh_owner_with_commit(
        &mut self,
        pair: &SealedCredentialPair,
        expected_generation: Uuid,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        pair.validate()?;
        if pair.generation != expected_generation {
            return Err(credential_begin_refusal(BeginRefusal::RowChanged));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let query = format!(
            "SELECT {CREDENTIAL_PAIR_COLUMNS} FROM credential_pairs WHERE
            account_id=1 AND slot=?1"
        );
        let current = tx
            .query_row(&query, [pair.slot.as_str()], credential_pair_from_row)
            .optional()
            .map_err(StoreError::CredentialPairStore)?
            .ok_or_else(|| credential_begin_refusal(BeginRefusal::RowChanged))?;
        if current.generation != expected_generation {
            return Err(credential_begin_refusal(BeginRefusal::RowChanged));
        }
        if current.origin != pair.origin
            || current.refresh_present != pair.refresh_present
            || current.refresh_fingerprint != pair.refresh_fingerprint
            || current.expires_at != pair.expires_at
            || current.next_refresh_at != pair.next_refresh_at
            || current.client_id != pair.client_id
            || current.region != pair.region
            || current.migrated != pair.migrated
        {
            return Err(StoreError::CredentialPairInvalid);
        }
        tx.execute(
            "UPDATE credential_pairs SET refresh_owner=?2,
            access_envelope=?3, refresh_envelope=?4 WHERE account_id=1 AND
            slot=?1 AND generation=?5",
            params![
                pair.slot.as_str(),
                pair.refresh_owner.as_str(),
                pair.access_envelope,
                pair.refresh_envelope,
                expected_generation.to_string()
            ],
        )
        .map_err(StoreError::CredentialPairStore)?;
        before_commit();
        tx.commit().map_err(StoreError::CredentialCommit)
    }
    pub(crate) fn reserve_credential_invocations(
        &mut self,
        key_id: KeyId,
        count: u64,
    ) -> Result<(), StoreError> {
        self.reserve_credential_invocations_with_commit(key_id, count, &mut || {})
    }

    /// TRANSITIONAL: R22's counted COMMIT is observed before its barrier.
    pub(crate) fn reserve_credential_invocations_with_commit(
        &mut self,
        key_id: KeyId,
        count: u64,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        let now = self.now_ms()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let used = tx
            .query_row(
                "SELECT reserved_invocations FROM credential_key_usage WHERE key_id=?1",
                [key_id.0.as_slice()],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?
            .unwrap_or(0);
        let used = u64::try_from(used).map_err(|_| StoreError::InvalidStoredCount)?;
        let total = used
            .checked_add(count)
            .filter(|total| *total <= crate::auth::credential_keys::FORCED_ROTATION_INVOCATIONS)
            .ok_or(StoreError::WriteRefused("credential_invocation_limit"))?;
        let total = i64::try_from(total).map_err(|_| StoreError::InvalidStoredCount)?;
        tx.execute(
            "INSERT INTO credential_key_usage(key_id, reserved_invocations,
            first_reserved_at_ms) VALUES(?1, ?2, ?3) ON CONFLICT(key_id)
            DO UPDATE SET
            reserved_invocations=excluded.reserved_invocations",
            params![key_id.0.as_slice(), total, now],
        )
        .map_err(StoreError::CredentialPairStore)?;
        before_commit();
        tx.commit().map_err(StoreError::CredentialCommit)?;
        #[cfg(test)]
        self.events.push("reservation_commit");
        self.credential_durability_barrier()?;
        #[cfg(test)]
        self.events.push("reservation_barrier");
        Ok(())
    }
    pub(crate) fn credential_durability_barrier(&mut self) -> Result<(), StoreError> {
        #[cfg(test)]
        {
            self.barrier_calls += 1;
        }
        #[cfg(test)]
        if self.barrier_failure || self.barrier_failure_at == Some(self.barrier_calls) {
            return Err(StoreError::CatalogueDurability(
                std::io::Error::from_raw_os_error(5),
            ));
        }
        use std::os::fd::AsFd;
        crate::platform::private_directory::durable_sync_fd(self.barrier_directory.as_fd()).map_err(
            |error| StoreError::CatalogueDurability(std::io::Error::from_raw_os_error(error.errno)),
        )?;
        #[cfg(test)] crate::auth::credential_vault::fixtures::power_event("database_barrier", None);
        Ok(())
    }
    pub(crate) fn insert_sign_in_binding(
        &mut self,
        slot: CredentialSlot,
        binding: [u8; 32],
        now_ms: i64,
    ) -> Result<Uuid, StoreError> {
        let expires = now_ms
            .checked_add(600_000)
            .filter(|_| now_ms >= 0)
            .ok_or(StoreError::CredentialPairInvalid)?;
        let id = Uuid::new_v4();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        tx.execute(
            "DELETE FROM credential_sign_ins WHERE expires_at_ms <= ?1",
            [now_ms],
        )
        .map_err(StoreError::CredentialPairStore)?;
        tx.execute(
            "INSERT INTO credential_sign_ins (sign_in_id, slot, binding,
            started_at_ms, expires_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                slot.as_str(),
                binding.as_slice(),
                now_ms,
                expires
            ],
        )
        .map_err(StoreError::CredentialPairStore)?;
        tx.commit().map_err(StoreError::CredentialPairStore)?;
        Ok(id)
    }
    pub(crate) fn scrub_credential_wal(&mut self) -> Result<bool, StoreError> {
        let mut wal = self.database_path.as_os_str().to_os_string();
        wal.push("-wal");
        for attempt in 0..5 {
            #[cfg(test)] crate::auth::credential_vault::fixtures::power_event("wal_scrub_attempt", None);
            let (busy, _, _): (i64, i64, i64) = self
                .connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(StoreError::CatalogueCheckpoint)?;
            let length = match fs::metadata(Path::new(&wal)) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => return Err(StoreError::InspectCatalogue(error)),
            };
            if busy == 0 && length == 0 {
                #[cfg(test)] crate::auth::credential_vault::fixtures::power_event("wal_scrubbed", None);
                return Ok(true);
            }
            if attempt < 4 {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        Ok(false)
    }
    #[cfg(test)]
    pub(crate) fn set_test_now_ms(&mut self, now: i64) {
        self.now_ms = Some(now);
    }
    #[cfg(test)]
    pub(crate) fn set_test_fence_capacity(&mut self, n: usize) {
        self.fence_capacity = n;
    }
    #[cfg(test)]
    pub(crate) fn test_connection(&self) -> &Connection {
        &self.connection
    }
    #[cfg(test)]
    pub(crate) fn test_blob_bytes_read(&self) -> usize {
        self.blob_bytes_read
    }
    #[cfg(test)]
    pub(crate) fn reset_test_blob_bytes_read(&mut self) {
        self.blob_bytes_read = 0;
    }
    #[cfg(test)]
    pub(crate) fn set_test_barrier_failure(&mut self, fail: bool) {
        self.barrier_failure = fail;
    }
    #[cfg(test)]
    pub(crate) fn set_test_rollback_completion(&mut self, fail: bool) {
        self.rollback_completion = fail;
    }
    #[cfg(test)]
    pub(crate) fn test_events(&self) -> Vec<&'static str> {
        self.events.clone()
    }
}
fn credential_schema_version(connection: &Connection) -> Result<i32, StoreError> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(StoreError::Query)
}
fn credential_begin_refusal(reason: BeginRefusal) -> StoreError {
    StoreError::RefreshOutcomeUnknown { reason }
}
fn credential_origin(value: Option<&str>) -> Option<Option<CredentialOrigin>> {
    Some(match value {
        None => None,
        Some("hub_login") => Some(CredentialOrigin::HubLogin),
        Some("direct_entry") => Some(CredentialOrigin::DirectEntry),
        Some("provider_entry") => Some(CredentialOrigin::ProviderEntry),
        Some("fleet_oauth") => Some(CredentialOrigin::FleetOauth),
        Some("source_token") => Some(CredentialOrigin::SourceToken),
        _ => return None,
    })
}
fn credential_slot(value: &str) -> Option<CredentialSlot> {
    match value {
        "owner_api" => Some(CredentialSlot::OwnerApi),
        "fleet_api" => Some(CredentialSlot::FleetApi),
        _ => None,
    }
}
fn credential_owner(value: &str) -> Option<RefreshOwner> {
    match value {
        "hub" => Some(RefreshOwner::Hub),
        "external" => Some(RefreshOwner::External),
        _ => None,
    }
}
fn credential_check_lengths(access: usize, refresh: usize) -> Result<(), StoreError> {
    use crate::auth::credential_envelope::check_envelope_length;
    check_envelope_length(access).map_err(StoreError::CredentialEnvelope)?;
    check_envelope_length(refresh).map_err(StoreError::CredentialEnvelope)
}
fn credential_check_header(bytes: &[u8], place: &EnvelopePlace) -> Result<(), EnvelopeError> {
    use crate::auth::credential_envelope::{check_place, parse_header};
    let header: &[u8; 39] = bytes.try_into().map_err(|_| EnvelopeError::Truncated)?;
    check_place(&parse_header(header)?, place)
}
fn credential_sign_in_binding(secret: &Zeroizing<[u8; 32]>) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub/sign-in-binding/v1\0");
    hash.update(secret.as_slice());
    // Reset the SHA-256 state after consuming the grant secret.
    hash.finalize_reset().into()
}
fn credential_pair_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SealedCredentialPair> {
    let slot: String = row.get(1)?;
    let origin: Option<String> = row.get(2)?;
    let owner: String = row.get(3)?;
    let generation: String = row.get(4)?;
    let fingerprint: Option<Vec<u8>> = row.get(8)?;
    Ok(SealedCredentialPair {
        account_id: row.get(0)?,
        slot: credential_slot(&slot).ok_or(rusqlite::Error::InvalidQuery)?,
        origin: credential_origin(origin.as_deref()).ok_or(rusqlite::Error::InvalidQuery)?,
        refresh_owner: credential_owner(&owner).ok_or(rusqlite::Error::InvalidQuery)?,
        generation: Uuid::parse_str(&generation).map_err(|_| rusqlite::Error::InvalidQuery)?,
        access_envelope: row.get(5)?,
        refresh_envelope: row.get(6)?,
        refresh_present: row.get(7)?,
        refresh_fingerprint: fingerprint
            .map(|f| f.try_into().map_err(|_| rusqlite::Error::InvalidQuery))
            .transpose()?,
        expires_at: row.get(9)?,
        next_refresh_at: row.get(10)?,
        client_id: row.get(11)?,
        region: row.get(12)?,
        migrated: row.get(13)?,
        written_at_ms: row.get(14)?,
    })
}
fn credential_write_pair(
    tx: &Transaction<'_>,
    pair: &SealedCredentialPair,
    now: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO credential_pairs(account_id, slot, origin,
            refresh_owner, generation, access_envelope, refresh_envelope,
            refresh_present, refresh_fingerprint, expires_at,
            next_refresh_at, client_id, region, migrated, written_at_ms)
            VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            ?14, ?15) ON CONFLICT(account_id, slot) DO UPDATE SET
            origin=excluded.origin, refresh_owner=excluded.refresh_owner,
            generation=excluded.generation,
            access_envelope=excluded.access_envelope,
            refresh_envelope=excluded.refresh_envelope,
            refresh_present=excluded.refresh_present,
            refresh_fingerprint=excluded.refresh_fingerprint,
            expires_at=excluded.expires_at,
            next_refresh_at=excluded.next_refresh_at,
            client_id=excluded.client_id, region=excluded.region,
            migrated=excluded.migrated,
            written_at_ms=excluded.written_at_ms",
        params![
            pair.account_id,
            pair.slot.as_str(),
            pair.origin.map(CredentialOrigin::as_str),
            pair.refresh_owner.as_str(),
            pair.generation.to_string(),
            pair.access_envelope,
            pair.refresh_envelope,
            pair.refresh_present,
            pair.refresh_fingerprint.as_ref().map(|f| f.as_slice()),
            pair.expires_at,
            pair.next_refresh_at,
            pair.client_id,
            pair.region,
            pair.migrated,
            now
        ],
    )
    .map_err(StoreError::CredentialPairStore)?;
    Ok(())
}
fn credential_has_opaque_fence(connection: &Connection) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM credential_legacy_fences WHERE
            identity='opaque')",
        [],
        |row| row.get(0),
    )
}
fn credential_unresolved(connection: &Connection, slot: CredentialSlot) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a JOIN
            outbound_request_receipts r ON r.id=a.receipt_id WHERE
            a.account_id=1 AND a.slot=?1 AND r.outcome='started')",
        [slot.as_str()],
        |row| row.get(0),
    )
}
fn credential_token_spent(
    connection: &Connection,
    fingerprint: Option<&[u8; 32]>,
    legacy: &LegacyCandidates,
) -> Result<bool, StoreError> {
    let fingerprint_spent = if let Some(f) = fingerprint {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credential_refresh_fences WHERE
            fingerprint=?1)",
                [f.as_slice()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(StoreError::CredentialRefreshReceipt)?
    } else {
        false
    };
    if fingerprint_spent {
        return Ok(true);
    }
    for generation in &legacy.0 {
        if connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credential_legacy_fences WHERE
            legacy_generation=?1 COLLATE NOCASE)",
                [generation.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .map_err(StoreError::CredentialRefreshReceipt)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn credential_cancel_slot(
    tx: &Transaction<'_>,
    slot: CredentialSlot,
    now: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE outbound_request_receipts SET outcome='cancelled',
            completed_at_ms=max(started_at_ms, ?2), duration_ms=max(0,
            ?2-started_at_ms), http_status=NULL, retry_after_seconds=NULL
            WHERE outcome='started' AND id IN(SELECT receipt_id FROM
            credential_refresh_attempts WHERE account_id=1 AND slot=?1)",
        params![slot.as_str(), now],
    )
    .map_err(StoreError::CredentialRefreshReceipt)?;
    Ok(())
}
fn credential_close_receipt(
    tx: &Transaction<'_>,
    receipt: OutboundRequestReceiptId,
    now: i64,
    outcome: OutboundRequestOutcome,
    status: Option<u16>,
    retry: Option<u64>,
) -> Result<(), StoreError> {
    let retry = retry
        .map(i64::try_from)
        .transpose()
        .map_err(|_| StoreError::InvalidOutboundRequestRetryAfter)?;
    let count = tx
        .execute(
            "UPDATE outbound_request_receipts SET outcome=?2,
            completed_at_ms=max(started_at_ms, ?3), duration_ms=max(0,
            ?3-started_at_ms), http_status=?4, retry_after_seconds=?5
            WHERE id=?1 AND outcome='started'",
            params![receipt.0, outcome.as_str(), now, status, retry],
        )
        .map_err(StoreError::CredentialRefreshReceipt)?;
    if count != 1 {
        return Err(credential_begin_refusal(BeginRefusal::RowChanged));
    }
    Ok(())
}
struct CredentialAttempt {
    slot: CredentialSlot,
    input: String,
    output: Option<String>,
    fence_id: Option<i64>,
    migrated: bool,
    outcome: String,
}
fn credential_attempt(
    connection: &Connection,
    receipt: OutboundRequestReceiptId,
) -> Result<Option<CredentialAttempt>, StoreError> {
    connection
        .query_row(
            "SELECT a.slot, a.input_generation, a.output_generation,
            a.fence_id, a.migrated, r.outcome FROM
            credential_refresh_attempts a JOIN outbound_request_receipts r
            ON r.id=a.receipt_id WHERE a.receipt_id=?1",
            [receipt.0],
            |row| {
                let slot: String = row.get(0)?;
                Ok(CredentialAttempt {
                    slot: credential_slot(&slot).ok_or(rusqlite::Error::InvalidQuery)?,
                    input: row.get(1)?,
                    output: row.get(2)?,
                    fence_id: row.get(3)?,
                    migrated: row.get(4)?,
                    outcome: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(StoreError::CredentialRefreshReceipt)
}
fn credential_release_attempt_fence(
    tx: &Transaction<'_>,
    receipt: OutboundRequestReceiptId,
    attempt: &CredentialAttempt,
) -> Result<(), StoreError> {
    if let Some(fence) = attempt.fence_id {
        tx.execute(
            "UPDATE credential_refresh_attempts SET fence_id=NULL WHERE
            receipt_id=?1",
            [receipt.0],
        )
        .map_err(StoreError::CredentialRefreshReceipt)?;
        tx.execute(
            "DELETE FROM credential_refresh_fences WHERE fence_id=?1 AND
            kind='attempt'",
            [fence],
        )
        .map_err(StoreError::CredentialRefreshReceipt)?;
    }
    Ok(())
}
#[allow(dead_code, reason = "Part S guarded-generation API; consumed by V1.")]
impl CredentialConnection {
    pub(crate) fn stored_generation(
        &mut self,
        slot: CredentialSlot,
    ) -> Result<Option<Uuid>, StoreError> {
        let generation = self
            .connection
            .query_row(
                "SELECT generation FROM credential_pairs WHERE account_id=1 AND
            slot=?1",
                [slot.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::CredentialPairStore)?;
        generation
            .map(|generation| {
                Uuid::parse_str(&generation).map_err(|_| StoreError::CredentialPairInvalid)
            })
            .transpose()
    }
    pub(crate) fn generation_is_refresh_output(
        &mut self,
        slot: CredentialSlot,
        generation: Uuid,
    ) -> Result<bool, StoreError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts a JOIN
            outbound_request_receipts r ON r.id=a.receipt_id WHERE
            a.account_id=1 AND a.slot=?1 AND a.output_generation=?2 AND
            r.outcome='success')",
                params![slot.as_str(), generation.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::CredentialRefreshReceipt)
    }
}
fn credential_inventory_count_sql(
    connection: &Connection,
    slot: CredentialSlot,
) -> Result<&'static str, StoreError> {
    if credential_schema_version(connection)? < CREDENTIAL_FORMAT_SCHEMA_VERSION {
        return Ok("SELECT 0");
    }
    Ok(match slot {
        CredentialSlot::OwnerApi => {
            "SELECT COUNT(*) FROM credential_pairs WHERE account_id=1 AND
            slot='owner_api'"
        }
        CredentialSlot::FleetApi => {
            "SELECT COUNT(*) FROM credential_pairs WHERE account_id=1 AND
            slot='fleet_api'"
        }
    })
}
#[allow(
    dead_code,
    reason = "Part S durability primitive; admitted V1 vault owns lock and read-only decision."
)]
impl CredentialConnection {
    /// Called only for an admitted writable vault, under its vault lock.
    /// Read-only/status/doctor/immutable opens never call this primitive.
    pub(crate) fn probe_credential_durability(&mut self) -> Result<(), StoreError> {
        self.probe_credential_durability_inner(#[cfg(test)] &mut || {})
    }
    fn probe_credential_durability_inner(
        &mut self,
        #[cfg(test)] before_barrier: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        let (_, log_frames, checkpointed_frames): (i64, i64, i64) = self
            .connection
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(StoreError::CatalogueCheckpoint)?;
        #[cfg(test)] crate::auth::credential_vault::fixtures::probe_event("after_probe_checkpoint");
        if log_frames != checkpointed_frames {
            // FULL synchronous commit syncs the complete WAL, including frames
            // readable from a predecessor process whose own sync did not run.
            let now = self.now_ms()?;
            let tx = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(StoreError::Begin)?;
            let changed = tx
                .execute(
                    "UPDATE credential_format_state SET durability_probed_at_ms=?1
            WHERE singleton_id=1",
                    [now],
                )
                .map_err(StoreError::CredentialPairStore)?;
            if changed != 1 {
                return Err(StoreError::CredentialPairInvalid);
            }
            tx.commit().map_err(StoreError::CredentialPairStore)?;
            #[cfg(test)]
            self.events.push("probe_stamp_commit");
        }
        #[cfg(test)] before_barrier();
        if let Err(error) = self.credential_durability_barrier() {
            self.record_unavailable_after_barrier_failure();
            return Err(error);
        }
        #[cfg(test)]
        self.events.push("probe_barrier");
        #[cfg(test)] crate::auth::credential_vault::fixtures::probe_event("after_probe_barrier");
        let prior: Option<String> = self
            .connection
            .query_row(
                "SELECT durability FROM credential_format_state WHERE singleton_id=1",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::CredentialPairStore)?;
        if prior.as_deref() != Some("ok") {
            self.record_credential_durability("ok")?;
            #[cfg(test)]
            self.events.push("probe_ok_commit");
            if let Err(error) = self.credential_durability_barrier() {
                self.record_unavailable_after_barrier_failure();
                return Err(error);
            }
            #[cfg(test)]
            self.events.push("probe_ok_barrier");
        }
        Ok(())
    }
    fn record_unavailable_after_barrier_failure(&mut self) {
        if let Err(error) = self.record_credential_durability("unavailable") {
            // The diagnostic must not replace the failed barrier's primary error.
            tracing::warn!(
                error = %error,
                "cannot record unavailable credential durability after failed barrier"
            );
        }
    }
    fn record_credential_durability(&mut self, value: &'static str) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let changed = tx
            .execute(
                "UPDATE credential_format_state SET durability=?1 WHERE singleton_id=1",
                [value],
            )
            .map_err(StoreError::CredentialPairStore)?;
        if changed != 1 {
            return Err(StoreError::CredentialPairInvalid);
        }
        tx.commit().map_err(StoreError::CredentialPairStore)
    }
}
#[cfg(test)]
impl CredentialConnection {
    /// Canonical vector fixture only, on a previous-process connection.
    /// TRANSITIONAL: CT19, CT20 and CT22 consume this fixture wrapper.
    pub(crate) fn test_insert_credential_vector(
        &mut self,
        pair: &SealedCredentialPair,
    ) -> Result<(), StoreError> {
        pair.validate()?;
        let tx = self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        credential_write_pair(&tx, pair, pair.written_at_ms)?;
        tx.commit().map_err(StoreError::CredentialPairStore)
    }

    /// Returns SQLite's effective limit, which cannot be below its page count.
    /// CT52 uses SQLite's allocation limit during conversion.
    pub(crate) fn set_test_max_page_count(&mut self, pages: u32) -> Result<u32, StoreError> {
        self.connection.pragma_update(None, "max_page_count", pages)
            .map_err(StoreError::Configure)?;
        self.connection.pragma_query_value(None, "max_page_count", |row| row.get(0))
            .map_err(StoreError::Query)
    }

    /// CT21's connection-local SQLite refusal, lifted before recovery.
    pub(crate) fn set_test_query_only(&mut self, enabled: bool) -> Result<(), StoreError> {
        self.connection.pragma_update(None, "query_only", enabled)
            .map_err(StoreError::Configure)
    }

    /// Accepted R73 probe with a cut point immediately before its first barrier.
    /// TRANSITIONAL: CT103 consumes this cut point at the admitted open.
    pub(crate) fn probe_credential_durability_with_cut_point(
        &mut self, before_barrier: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        self.probe_credential_durability_inner(before_barrier)
    }

    pub(crate) fn set_test_barrier_failure_at(&mut self, call: Option<usize>) {
        self.barrier_failure_at = call;
        self.barrier_calls = 0;
    }
    pub(crate) fn test_barrier_calls(&self) -> usize {
        self.barrier_calls
    }
}
impl CredentialConnection {
    /// R21 changes only the envelopes of the complete occupied slot set.
    /// TRANSITIONAL: the vault supplies its counted-COMMIT callback.
    pub(crate) fn reseal_credential_pairs(
        &mut self,
        pairs: &[SealedCredentialPair],
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        if pairs.len() > 2 {
            return Err(StoreError::CredentialPairInvalid);
        }
        for (index, pair) in pairs.iter().enumerate() {
            pair.validate()?;
            if pairs[..index].iter().any(|prior| prior.slot == pair.slot) {
                return Err(StoreError::CredentialPairInvalid);
            }
        }
        let tx = self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let query = format!("SELECT {CREDENTIAL_PAIR_COLUMNS} FROM credential_pairs ORDER BY slot");
        let current = {
            let mut statement = tx.prepare(&query).map_err(StoreError::CredentialPairStore)?;
            statement.query_map([], credential_pair_from_row)
                .map_err(StoreError::CredentialPairStore)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::CredentialPairStore)?
        };
        if current.len() != pairs.len() {
            return Err(StoreError::CredentialPairInvalid);
        }
        if current.is_empty() {
            return Ok(());
        }
        for row in &current {
            let pair = pairs.iter().find(|pair| pair.slot == row.slot)
                .ok_or(StoreError::CredentialPairInvalid)?;
            if row.generation != pair.generation {
                return Err(credential_begin_refusal(BeginRefusal::RowChanged));
            }
            if row.account_id != pair.account_id || row.origin != pair.origin
                || row.refresh_owner != pair.refresh_owner
                || row.refresh_present != pair.refresh_present
                || row.refresh_fingerprint != pair.refresh_fingerprint
                || row.expires_at != pair.expires_at
                || row.next_refresh_at != pair.next_refresh_at
                || row.client_id != pair.client_id || row.region != pair.region
                || row.migrated != pair.migrated || row.written_at_ms != pair.written_at_ms
            {
                return Err(StoreError::CredentialPairInvalid);
            }
        }
        for pair in pairs {
            let changed = tx.execute(
                "UPDATE credential_pairs SET access_envelope=?3, refresh_envelope=?4
                 WHERE account_id=?1 AND slot=?2 AND generation=?5",
                params![pair.account_id, pair.slot.as_str(), pair.access_envelope,
                    pair.refresh_envelope, pair.generation.to_string()],
            ).map_err(StoreError::CredentialPairStore)?;
            if changed != 1 {
                return Err(credential_begin_refusal(BeginRefusal::RowChanged));
            }
        }
        before_commit();
        tx.commit().map_err(StoreError::CredentialCommit)
    }
}

#[allow(
    dead_code,
    reason = "Part S reservation/sealing utility; consumed by V1."
)]
impl CredentialConnection {
    /// A failed reservation/barrier cannot reach either nonce draw. Rotation
    /// at the reservation ceiling is the vault's duty, before retrying here.
    pub(crate) fn reserve_and_seal_credential_pair(
        &mut self,
        pair: &mut SealedCredentialPair,
        keys: &KeyringHandle,
        access: &[u8],
        refresh: &[u8],
        nonces: &mut dyn NonceSource,
    ) -> Result<(), StoreError> {
        self.reserve_and_seal_credential_pair_with_commit(
            pair, keys, access, refresh, nonces, &mut || {},
        )
    }

    pub(crate) fn reserve_and_seal_credential_pair_with_commit(
        &mut self,
        pair: &mut SealedCredentialPair,
        keys: &KeyringHandle,
        access: &[u8],
        refresh: &[u8],
        nonces: &mut dyn NonceSource,
        before_commit: &mut dyn FnMut(),
    ) -> Result<(), StoreError> {
        self.reserve_credential_invocations_with_commit(keys.keyring().current.id, 2, before_commit)?;
        #[cfg(test)]
        self.events.push("seal_start");
        pair.seal(keys, access, refresh, nonces)
    }
}
pub const CREDENTIAL_FORMAT_SCHEMA_VERSION: i32 = 79;
const _: () = assert!(
    CREDENTIAL_FORMAT_SCHEMA_VERSION == MIN_STORE_VERSION + 1
        && CREDENTIAL_FORMAT_SCHEMA_VERSION <= SCHEMA_VERSION
);
/// Phase A needs no key and is reached only after the ordinary writable-store
/// admission and floor creation. Phase B owns decryption, barriers and scrubbing.
fn credential_schema_rung(connection: &Connection, fresh: bool) -> Result<(), StoreError> {
    let version = schema_version(connection)?;
    if version == CREDENTIAL_FORMAT_SCHEMA_VERSION {
        return credential_format_state_keyring_exposure_column(connection);
    }
    if version != MIN_STORE_VERSION {
        return Err(StoreError::UnsupportedSchema(version));
    }
    let prior_secure_delete: i64 = connection
        .pragma_query_value(None, "secure_delete", |row| row.get(0))
        .map_err(StoreError::Migrate)?;
    connection
        .pragma_update(None, "secure_delete", true)
        .map_err(StoreError::Migrate)?;
    let result = credential_schema_rung_transaction(connection, fresh);
    let restored = connection
        .pragma_update(None, "secure_delete", prior_secure_delete)
        .map_err(StoreError::Migrate);
    match result {
        Ok(()) => restored,
        Err(error) => {
            if let Err(restore_error) = restored {
                tracing::warn!(error = %restore_error,
 "cannot restore schema-rung secure_delete");
            }
            Err(error)
        }
    }
}
fn credential_schema_rung_transaction(
    connection: &Connection,
    fresh: bool,
) -> Result<(), StoreError> {
    let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(StoreError::Migrate)?;
    let version = schema_version(&transaction)?;
    if version == CREDENTIAL_FORMAT_SCHEMA_VERSION {
        return Ok(());
    }
    if version != MIN_STORE_VERSION {
        return Err(StoreError::UnsupportedSchema(version));
    }
    credential_pre_upgrade_presence(&transaction)?;
    let (_, suffix) = include_str!("schema_floor.sql")
        .split_once("-- C10 credential schema")
        .expect("C10 schema marker belongs to the checked-in floor schema");
    transaction
        .execute_batch(suffix)
        .map_err(StoreError::Migrate)?;
    let occupied = |table: &str| -> Result<bool, StoreError> {
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table'
            AND name=?1)",
                [table],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)?;
        if !exists {
            return Ok(false);
        }
        transaction
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table})"),
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Migrate)
    };
    let owner_pending = occupied("legacy_owner_tokens")?;
    let fleet_pending = occupied("fleet_tokens")?;
    let fences_pending = occupied("legacy_refresh_receipt_bindings")?
        || occupied("legacy_refresh_input_fences")?
        || occupied("fleet_refresh_receipt_bindings")?
        || occupied("fleet_refresh_input_fences")?;
    let converted = !owner_pending && !fleet_pending && !fences_pending;
    let now = migration_clock_ms()?;
    if converted {
        transaction
            .execute_batch(
                "DROP TABLE IF EXISTS legacy_refresh_receipt_bindings;
                 DROP TABLE IF EXISTS legacy_refresh_input_fences;
                 DROP TABLE IF EXISTS fleet_refresh_receipt_bindings;
                 DROP TABLE IF EXISTS fleet_refresh_input_fences;
                 DROP TABLE IF EXISTS legacy_owner_tokens;
                 DROP TABLE IF EXISTS fleet_tokens;",
            )
            .map_err(StoreError::Migrate)?;
    }
    transaction
        .execute(
            "INSERT INTO credential_format_state (singleton_id,
            format_version, owner_api_state, fleet_api_state,
            fences_pending, old_tables_dropped, purge_required,
            wal_scrubbed, old_files_removed, converted_at_ms,
            completed_at_ms) VALUES(1, 1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9)",
            params![
                if owner_pending { "pending" } else { "none" },
                if fleet_pending { "pending" } else { "none" },
                fences_pending,
                converted,
                !fresh,
                fresh,
                fresh,
                converted.then_some(now),
                (fresh && converted).then_some(now),
            ],
        )
        .map_err(StoreError::Migrate)?;
    transaction
        .pragma_update(None, "user_version", CREDENTIAL_FORMAT_SCHEMA_VERSION)
        .map_err(StoreError::Migrate)?;
    transaction.commit().map_err(StoreError::Migrate)
}
fn credential_sql_length(value: i64) -> Result<usize, StoreError> {
    usize::try_from(value).map_err(|_| StoreError::CredentialPairInvalid)
}
// These synthetic fixtures call the real S API on its once-taken connection.
// They are compiled only for the retained store tests, never for a vault.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct STestCredentialConnection(Mutex<Option<CredentialConnection>>);
#[cfg(test)]
impl std::fmt::Debug for STestCredentialConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("STestCredentialConnection").finish_non_exhaustive()
    }
}
#[cfg(test)]
fn s_test_with_credential_connection<T>(
    store: &HubStore,
    callback: impl FnOnce(&mut CredentialConnection) -> T,
) -> T {
    let mut retained = store.test_retained_credential_connection.0.lock().expect("S fixture connection");
    let connection = retained.get_or_insert_with(|| store.take_credential_connection()
        .expect("once-taken connection for synthetic retained store fixture"));
    callback(connection)
}
#[cfg(test)]
impl HubStore {
    pub(crate) fn test_retained_refresh_output(&self, generation: Uuid) -> Option<Result<bool, StoreError>> {
        let mut retained = self.test_retained_credential_connection.0.lock().expect("S fixture connection");
        retained.as_mut().map(|connection| connection.generation_is_refresh_output(CredentialSlot::OwnerApi, generation))
    }
}
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "Synthetic retained-fixture fields are explicit."
)]
fn s_test_synthetic_pair(
    connection: &mut CredentialConnection,
    slot: CredentialSlot,
    generation: Uuid,
    access: &[u8],
    refresh: &[u8],
    expires_at: i64,
    next_refresh_at: i64,
) -> Result<SealedCredentialPair, StoreError> {
    use crate::auth::credential_envelope::OsNonces;
    use crate::auth::credential_keys::{KeyMaterial, Keyring};
    let keys = KeyringHandle::new(Keyring {
        current: KeyMaterial {
            id: KeyId([0x53; 8]),
            created_at_s: 1_800_000_000,
            key: Zeroizing::new([0x53; 32]),
        },
        previous: None,
    });
    let fleet = slot == CredentialSlot::FleetApi;
    let mut pair = SealedCredentialPair {
        account_id: 1,
        slot,
        origin: Some(if fleet {
            CredentialOrigin::FleetOauth
        } else if refresh.is_empty() {
            CredentialOrigin::ProviderEntry
        } else {
            CredentialOrigin::DirectEntry
        }),
        refresh_owner: RefreshOwner::Hub,
        generation,
        access_envelope: Vec::new(),
        refresh_envelope: Vec::new(),
        refresh_present: !refresh.is_empty(),
        refresh_fingerprint: None,
        expires_at,
        next_refresh_at,
        client_id: fleet.then(|| "client-fixture".to_string()),
        region: fleet.then(|| "eu".to_string()),
        migrated: false,
        written_at_ms: connection.now_ms()?,
    };
    connection.reserve_and_seal_credential_pair(
        &mut pair,
        &keys,
        access,
        refresh,
        &mut OsNonces,
    )?;
    Ok(pair)
}
