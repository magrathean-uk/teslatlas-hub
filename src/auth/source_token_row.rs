// SPDX-License-Identifier: AGPL-3.0-only

//! Separate read-only source credential snapshot for the take-over. It lives
//! outside the history import so the import's credential guard
//! (`source_credential_disabled_import_never_names_source_credential_relations`)
//! covers every import file without exception. History import and its
//! private-schema witness do not call this module; it only borrows the
//! import's read-only connection and snapshot primitives.

use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    credentials::{OwnerTokens, SourceDbPassword},
    import::source_db::{
        reader::{
            SourceDbReadLimits, SourceDbSnapshotSession, SourceReadBudget, connect_source,
            prepare_read_only_snapshot, query_bounded,
        },
        source::ReadOnlySource,
    },
    legacy_auth::{AuthEndpointConfig, AuthMode, LegacyAuth},
    source_token_envelope::{
        MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES, SourceDbTokenError, decrypt_legacy_owner_tokens,
    },
};

const OTHER_CLIENTS_SQL: &str = "SELECT count(*) AS other_clients FROM pg_catalog.pg_stat_activity \
    WHERE datname = current_database() AND pid <> pg_backend_pid() \
    AND backend_type = 'client backend'";

// The length projection refuses oversized fields before the driver buffers
// their bytes. LIMIT 2 supplies just enough lookahead to refuse ambiguity.
const TOKEN_ROW_SQL: &str = "SELECT octet_length(access) AS access_bytes, \
    octet_length(refresh) AS refresh_bytes, \
    CASE WHEN octet_length(access) <= $1 THEN access ELSE NULL END AS access, \
    CASE WHEN octet_length(refresh) <= $1 THEN refresh ELSE NULL END AS refresh \
    FROM private.tokens LIMIT 2";

/// Read and validate a source pair without storing it or making a Tesla call.
/// The caller supplies secrets in zeroising buffers and receives a redacted,
/// zeroising pair. This always opens its own source connection; it never uses
/// a history import connection, exported snapshot or source witness.
pub async fn read_source_token_row(
    source: &ReadOnlySource,
    password: &SourceDbPassword,
    encryption_key: &Zeroizing<Vec<u8>>,
    limits: SourceDbReadLimits,
) -> Result<OwnerTokens, SourceTokenRowError> {
    limits
        .validate()
        .map_err(|_| SourceTokenRowError::ReadFailed)?;
    let (client, task) = connect_source(source, password, limits)
        .await
        .map_err(|_| SourceTokenRowError::ReadFailed)?;
    read_snapshot(
        PostgresSnapshot {
            session: SourceDbSnapshotSession::new(client, task),
            source,
            limits,
        },
        encryption_key,
    )
    .await
}

/// Fixed errors deliberately discard database, schema and decrypt diagnostics:
/// neither Display, Debug nor an error source can expose source values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SourceTokenRowError {
    #[error("source_still_running")]
    SourceStillRunning,
    #[error("source_token_row_missing")]
    RowMissing,
    #[error("source_token_rows_ambiguous")]
    RowsAmbiguous,
    #[error("source_token_too_large")]
    TooLarge,
    #[error("source_token_key_mismatch")]
    KeyMismatch,
    #[error("source_token_invalid")]
    Invalid,
    #[error("source_token_read_failed")]
    ReadFailed,
}

impl SourceTokenRowError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SourceStillRunning => "source_still_running",
            Self::RowMissing => "source_token_row_missing",
            Self::RowsAmbiguous => "source_token_rows_ambiguous",
            Self::TooLarge => "source_token_too_large",
            Self::KeyMismatch => "source_token_key_mismatch",
            Self::Invalid => "source_token_invalid",
            Self::ReadFailed => "source_token_read_failed",
        }
    }
}

struct EncryptedRow {
    access_bytes: Option<i32>,
    refresh_bytes: Option<i32>,
    access: Option<Zeroizing<Vec<u8>>>,
    refresh: Option<Zeroizing<Vec<u8>>>,
}

// Only the database boundary is replaced in tests. The sequencing, cardinality,
// size checks, real decryption and endpoint validation remain the production path.
trait TokenSnapshot {
    async fn prepare(&mut self) -> Result<(), SourceTokenRowError>;
    async fn other_clients(&mut self, sql: &str) -> Result<i64, SourceTokenRowError>;
    async fn rows(&mut self, sql: &str, cap: i32)
    -> Result<Vec<EncryptedRow>, SourceTokenRowError>;
    async fn finish(self) -> Result<(), SourceTokenRowError>;
}

struct PostgresSnapshot<'a> {
    session: SourceDbSnapshotSession,
    source: &'a ReadOnlySource,
    limits: SourceDbReadLimits,
}

impl TokenSnapshot for PostgresSnapshot<'_> {
    async fn prepare(&mut self) -> Result<(), SourceTokenRowError> {
        prepare_read_only_snapshot(self.session.client(), self.source, self.limits)
            .await
            .map(|_| ())
            .map_err(|_| SourceTokenRowError::ReadFailed)
    }

    async fn other_clients(&mut self, sql: &str) -> Result<i64, SourceTokenRowError> {
        let row = self
            .session
            .client()
            .query_one(sql, &[])
            .await
            .map_err(|_| SourceTokenRowError::ReadFailed)?;
        row.try_get("other_clients")
            .map_err(|_| SourceTokenRowError::ReadFailed)
    }

    async fn rows(
        &mut self,
        sql: &str,
        cap: i32,
    ) -> Result<Vec<EncryptedRow>, SourceTokenRowError> {
        let rows = query_bounded(
            self.session.client(),
            sql,
            &[&cap],
            "private.tokens",
            SourceReadBudget::fixed(2),
        )
        .await
        .map_err(|_| SourceTokenRowError::ReadFailed)?;
        rows.into_iter()
            .map(|row| {
                let decode_error = |_| SourceTokenRowError::ReadFailed;
                Ok(EncryptedRow {
                    access_bytes: row.try_get("access_bytes").map_err(decode_error)?,
                    refresh_bytes: row.try_get("refresh_bytes").map_err(decode_error)?,
                    access: row
                        .try_get::<_, Option<&[u8]>>("access")
                        .map_err(decode_error)?
                        .map(|bytes| Zeroizing::new(bytes.to_vec())),
                    refresh: row
                        .try_get::<_, Option<&[u8]>>("refresh")
                        .map_err(decode_error)?
                        .map(|bytes| Zeroizing::new(bytes.to_vec())),
                })
            })
            .collect()
    }

    async fn finish(self) -> Result<(), SourceTokenRowError> {
        self.session
            .finish()
            .await
            .map_err(|_| SourceTokenRowError::ReadFailed)
    }
}

async fn read_snapshot(
    mut snapshot: impl TokenSnapshot,
    encryption_key: &Zeroizing<Vec<u8>>,
) -> Result<OwnerTokens, SourceTokenRowError> {
    let result = async {
        snapshot.prepare().await?;
        match snapshot.other_clients(OTHER_CLIENTS_SQL).await? {
            0 => (),
            count if count > 0 => return Err(SourceTokenRowError::SourceStillRunning),
            _ => return Err(SourceTokenRowError::ReadFailed),
        }
        let cap = i32::try_from(MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES)
            .expect("source ciphertext cap fits PostgreSQL int4");
        let mut rows = snapshot.rows(TOKEN_ROW_SQL, cap).await?;
        match rows.len() {
            0 => return Err(SourceTokenRowError::RowMissing),
            1 => (),
            _ => return Err(SourceTokenRowError::RowsAmbiguous),
        }
        let row = rows.pop().expect("one source row");
        let access = checked_value(row.access_bytes, row.access)?;
        let refresh = checked_value(row.refresh_bytes, row.refresh)?;
        let tokens = decrypt_legacy_owner_tokens(encryption_key, &access, &refresh)
            .map_err(map_decrypt_error)?;
        // Use exactly the runtime grammar and issuer pin, with no provider
        // endpoint or region fallback that could admit a foreign/opaque token.
        LegacyAuth::from_access_token(
            tokens.access_token(),
            tokens.refresh_token(),
            &AuthEndpointConfig {
                mode: AuthMode::Tesla,
                region: None,
                token_url: None,
                allow_loopback_http: false,
            },
        )
        .map_err(|_| SourceTokenRowError::Invalid)?;
        Ok(tokens)
    }
    .await;
    let finish = snapshot.finish().await;
    match (result, finish) {
        (Err(error), _) => Err(error),
        (Ok(tokens), Ok(())) => Ok(tokens),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn checked_value(
    length: Option<i32>,
    value: Option<Zeroizing<Vec<u8>>>,
) -> Result<Zeroizing<Vec<u8>>, SourceTokenRowError> {
    let length = length
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(SourceTokenRowError::Invalid)?;
    if length > MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES {
        return Err(SourceTokenRowError::TooLarge);
    }
    let value = value.ok_or(SourceTokenRowError::Invalid)?;
    if value.len() > MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES {
        return Err(SourceTokenRowError::TooLarge);
    }
    if value.len() != length {
        return Err(SourceTokenRowError::Invalid);
    }
    Ok(value)
}

fn map_decrypt_error(error: SourceDbTokenError) -> SourceTokenRowError {
    match error {
        SourceDbTokenError::AuthenticationFailed
        | SourceDbTokenError::EmptyEncryptionKey
        | SourceDbTokenError::InvalidEncryptionKey => SourceTokenRowError::KeyMismatch,
        _ => SourceTokenRowError::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use aes_gcm::{
        Aes256Gcm, Nonce,
        aead::{Aead, KeyInit, Payload},
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    use super::*;

    const SYNTHETIC_KEY: &[u8] = b"synthetic-source-row-key-not-a-real-key";
    const SYNTHETIC_REFRESH: &[u8] = b"synthetic-source-row-refresh-not-a-real-token";
    type Events = Rc<RefCell<Vec<&'static str>>>;

    struct FakeSnapshot {
        events: Events,
        prepare: Result<(), SourceTokenRowError>,
        sessions: Result<i64, SourceTokenRowError>,
        rows: Result<Vec<EncryptedRow>, SourceTokenRowError>,
        finish: Result<(), SourceTokenRowError>,
    }

    impl FakeSnapshot {
        fn new(rows: Vec<EncryptedRow>) -> Self {
            Self {
                events: Rc::default(),
                prepare: Ok(()),
                sessions: Ok(0),
                rows: Ok(rows),
                finish: Ok(()),
            }
        }
    }

    impl TokenSnapshot for FakeSnapshot {
        async fn prepare(&mut self) -> Result<(), SourceTokenRowError> {
            self.events.borrow_mut().push("prepare");
            self.prepare
        }

        async fn other_clients(&mut self, sql: &str) -> Result<i64, SourceTokenRowError> {
            assert_eq!(sql, OTHER_CLIENTS_SQL);
            self.events.borrow_mut().push("sessions");
            self.sessions
        }

        async fn rows(
            &mut self,
            sql: &str,
            cap: i32,
        ) -> Result<Vec<EncryptedRow>, SourceTokenRowError> {
            assert_eq!(sql, TOKEN_ROW_SQL);
            assert_eq!(
                usize::try_from(cap).unwrap(),
                MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES
            );
            self.events.borrow_mut().push("rows");
            std::mem::replace(&mut self.rows, Ok(Vec::new()))
        }

        async fn finish(self) -> Result<(), SourceTokenRowError> {
            self.events.borrow_mut().push("rollback");
            self.finish
        }
    }

    fn synthetic_access(issuer: &str) -> Zeroizing<String> {
        let claims = Zeroizing::new(
            serde_json::json!({
                "iss": issuer,
                "exp": 1_900_000_000,
                "synthetic": true,
            })
            .to_string(),
        );
        Zeroizing::new(format!(
            "e30.{}.synthetic-signature",
            URL_SAFE_NO_PAD.encode(claims.as_bytes())
        ))
    }

    // Synthetic fixtures use the public Cloak V1 layout understood by the
    // existing decrypt module, not any source application's implementation.
    fn seal(plaintext: &[u8], nonce_byte: u8) -> Zeroizing<Vec<u8>> {
        let derived = Zeroizing::new(Sha256::digest(SYNTHETIC_KEY).to_vec());
        let cipher = Aes256Gcm::new_from_slice(&derived).unwrap();
        let nonce = Nonce::from([nonce_byte; 12]);
        let sealed = Zeroizing::new(
            cipher
                .encrypt(
                    &nonce,
                    Payload {
                        msg: plaintext,
                        aad: b"AES256GCM",
                    },
                )
                .unwrap(),
        );
        let (ciphertext, tag) = sealed.split_at(sealed.len() - 16);
        let mut envelope = Zeroizing::new(vec![1, 10]);
        envelope.extend_from_slice(b"AES.GCM.V1");
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(tag);
        envelope.extend_from_slice(ciphertext);
        envelope
    }

    fn encrypted_row(access: &[u8], refresh: &[u8]) -> EncryptedRow {
        let access = seal(access, 1);
        let refresh = seal(refresh, 2);
        EncryptedRow {
            access_bytes: Some(i32::try_from(access.len()).unwrap()),
            refresh_bytes: Some(i32::try_from(refresh.len()).unwrap()),
            access: Some(access),
            refresh: Some(refresh),
        }
    }

    fn valid_row() -> EncryptedRow {
        encrypted_row(
            synthetic_access("https://auth.tesla.com/oauth2/v3").as_bytes(),
            SYNTHETIC_REFRESH,
        )
    }

    async fn run(fake: FakeSnapshot) -> (Result<OwnerTokens, SourceTokenRowError>, Events) {
        let events = Rc::clone(&fake.events);
        let key = Zeroizing::new(SYNTHETIC_KEY.to_vec());
        (read_snapshot(fake, &key).await, events)
    }

    #[test]
    fn source_token_row_sql_is_read_only_bounded_and_checks_other_database_clients() {
        assert_eq!(
            OTHER_CLIENTS_SQL,
            "SELECT count(*) AS other_clients FROM pg_catalog.pg_stat_activity \
            WHERE datname = current_database() AND pid <> pg_backend_pid() \
            AND backend_type = 'client backend'"
        );
        assert_eq!(
            TOKEN_ROW_SQL,
            "SELECT octet_length(access) AS access_bytes, \
            octet_length(refresh) AS refresh_bytes, \
            CASE WHEN octet_length(access) <= $1 THEN access ELSE NULL END AS access, \
            CASE WHEN octet_length(refresh) <= $1 THEN refresh ELSE NULL END AS refresh \
            FROM private.tokens LIMIT 2"
        );
        let setup = crate::source_db_schema::READ_ONLY_SESSION_SQL;
        assert!(setup[0].contains("TRANSACTION READ ONLY"));
        assert!(setup[1].contains("REPEATABLE READ, READ ONLY"));
    }

    #[tokio::test]
    async fn source_token_row_reads_single_pair_and_rolls_back_before_returning() {
        for issuer in [
            "https://auth.tesla.com/oauth2/v3",
            "https://auth.tesla.cn/oauth2/v3",
        ] {
            let access = synthetic_access(issuer);
            let (result, events) = run(FakeSnapshot::new(vec![encrypted_row(
                access.as_bytes(),
                SYNTHETIC_REFRESH,
            )]))
            .await;
            let tokens = result.expect("synthetic pair");
            assert!(tokens.access_token() == access.as_str());
            assert!(tokens.refresh_token().as_bytes() == SYNTHETIC_REFRESH);
            assert_eq!(
                *events.borrow(),
                ["prepare", "sessions", "rows", "rollback"]
            );
        }
    }

    #[tokio::test]
    async fn source_token_row_refuses_a_login_issued_to_another_client() {
        let claims = Zeroizing::new(serde_json::json!({
            "iss": "https://auth.tesla.com/oauth2/v3",
            "exp": 1_900_000_000,
            "azp": "synthetic-other-client",
            "synthetic": true,
        }).to_string());
        let access = Zeroizing::new(format!(
            "e30.{}.synthetic-signature",
            URL_SAFE_NO_PAD.encode(claims.as_bytes()),
        ));
        let (result, events) = run(FakeSnapshot::new(vec![encrypted_row(
            access.as_bytes(), SYNTHETIC_REFRESH,
        )])).await;
        assert_eq!(result.unwrap_err(), SourceTokenRowError::Invalid);
        assert_eq!(*events.borrow(), ["prepare", "sessions", "rows", "rollback"]);
    }

    #[tokio::test]
    async fn source_token_row_other_clients_refused_before_row_read() {
        for count in [1, 2, i64::MAX] {
            let mut fake = FakeSnapshot::new(vec![valid_row()]);
            fake.sessions = Ok(count);
            let (result, events) = run(fake).await;
            assert_eq!(result.unwrap_err(), SourceTokenRowError::SourceStillRunning);
            assert_eq!(*events.borrow(), ["prepare", "sessions", "rollback"]);
        }
    }

    #[tokio::test]
    async fn source_token_row_negative_session_count_fails_closed() {
        let mut fake = FakeSnapshot::new(vec![valid_row()]);
        fake.sessions = Ok(-1);
        let (result, events) = run(fake).await;
        assert_eq!(result.unwrap_err(), SourceTokenRowError::ReadFailed);
        assert_eq!(*events.borrow(), ["prepare", "sessions", "rollback"]);
    }

    #[tokio::test]
    async fn source_token_row_missing_and_extra_rows_refused_before_decryption() {
        for count in [0, 2, 3] {
            let rows = (0..count)
                .map(|_| encrypted_row(b"invalid plaintext", b"invalid plaintext"))
                .collect();
            let (result, events) = run(FakeSnapshot::new(rows)).await;
            assert_eq!(
                result.unwrap_err(),
                if count == 0 {
                    SourceTokenRowError::RowMissing
                } else {
                    SourceTokenRowError::RowsAmbiguous
                }
            );
            assert_eq!(
                *events.borrow(),
                ["prepare", "sessions", "rows", "rollback"]
            );
        }
    }

    #[tokio::test]
    async fn source_token_row_caps_both_ciphertexts_before_decryption() {
        for access_side in [true, false] {
            for projected_null in [true, false] {
                let mut row = valid_row();
                let (length, value) = if access_side {
                    (&mut row.access_bytes, &mut row.access)
                } else {
                    (&mut row.refresh_bytes, &mut row.refresh)
                };
                *length = Some(i32::try_from(MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES + 1).unwrap());
                *value = if projected_null {
                    None
                } else {
                    Some(Zeroizing::new(vec![
                        0;
                        MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES + 1
                    ]))
                };
                let (result, _) = run(FakeSnapshot::new(vec![row])).await;
                assert_eq!(result.unwrap_err(), SourceTokenRowError::TooLarge);
            }
        }
    }

    #[tokio::test]
    async fn source_token_row_exact_ciphertext_cap_is_accepted() {
        let mut access = synthetic_access("https://auth.tesla.com/oauth2/v3");
        let max_plaintext = crate::source_token_envelope::MAX_TOKEN_PLAINTEXT_BYTES;
        let padding = max_plaintext - access.len();
        access.extend(std::iter::repeat_n('x', padding));
        let refresh = Zeroizing::new(vec![b'r'; max_plaintext]);
        let row = encrypted_row(access.as_bytes(), &refresh);
        assert_eq!(
            row.access_bytes,
            Some(i32::try_from(MAX_SOURCE_TOKEN_CIPHERTEXT_BYTES).unwrap())
        );
        assert_eq!(row.refresh_bytes, row.access_bytes);
        let (result, _) = run(FakeSnapshot::new(vec![row])).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn source_token_row_null_negative_and_inconsistent_fields_are_invalid() {
        for access_side in [true, false] {
            for case in 0..5 {
                let mut row = valid_row();
                let (length, value) = if access_side {
                    (&mut row.access_bytes, &mut row.access)
                } else {
                    (&mut row.refresh_bytes, &mut row.refresh)
                };
                match case {
                    0 => *length = None,
                    1 => *length = Some(-1),
                    2 => *value = None,
                    3 => *length = Some(0),
                    _ => {
                        *length = Some(0);
                        *value = Some(Zeroizing::new(Vec::new()));
                    }
                }
                let (result, _) = run(FakeSnapshot::new(vec![row])).await;
                assert_eq!(result.unwrap_err(), SourceTokenRowError::Invalid);
            }
        }
    }

    #[tokio::test]
    async fn source_token_row_wrong_key_and_tampered_tags_map_to_key_mismatch() {
        let fake = FakeSnapshot::new(vec![valid_row()]);
        let events = Rc::clone(&fake.events);
        let key = Zeroizing::new(b"synthetic-wrong-key".to_vec());
        let result = read_snapshot(fake, &key).await;
        assert_eq!(result.unwrap_err(), SourceTokenRowError::KeyMismatch);
        assert_eq!(events.borrow().last(), Some(&"rollback"));
        for access_side in [true, false] {
            let mut row = valid_row();
            let value = if access_side {
                &mut row.access
            } else {
                &mut row.refresh
            };
            value.as_mut().unwrap()[24] ^= 1;
            let (result, _) = run(FakeSnapshot::new(vec![row])).await;
            assert_eq!(result.unwrap_err(), SourceTokenRowError::KeyMismatch);
        }
    }

    #[tokio::test]
    async fn source_token_row_empty_key_is_redacted_key_mismatch() {
        let result = read_snapshot(
            FakeSnapshot::new(vec![valid_row()]),
            &Zeroizing::new(Vec::new()),
        )
        .await;
        assert_eq!(result.unwrap_err(), SourceTokenRowError::KeyMismatch);
    }

    #[tokio::test]
    async fn source_token_row_invalid_envelope_plaintext_grammar_and_issuer_refused() {
        let access = synthetic_access("https://auth.tesla.com/oauth2/v3");
        let foreign = synthetic_access("https://synthetic-issuer.invalid/oauth2/v3");
        for (access, refresh) in [
            (&b"bad access grammar!"[..], SYNTHETIC_REFRESH),
            (access.as_bytes(), &b"synthetic-non-ascii-\xff"[..]),
            (&b"\xff"[..], SYNTHETIC_REFRESH),
            (&b""[..], SYNTHETIC_REFRESH),
            (access.as_bytes(), &b""[..]),
            (access.as_bytes(), &b"synthetic\nrefresh"[..]),
            (foreign.as_bytes(), SYNTHETIC_REFRESH),
            (&b"synthetic-opaque-without-issuer"[..], SYNTHETIC_REFRESH),
        ] {
            let (result, _) = run(FakeSnapshot::new(vec![encrypted_row(access, refresh)])).await;
            assert_eq!(result.unwrap_err(), SourceTokenRowError::Invalid);
        }
        for access_side in [true, false] {
            let mut row = valid_row();
            let value = if access_side {
                &mut row.access
            } else {
                &mut row.refresh
            };
            value.as_mut().unwrap()[0] = 0;
            let (result, _) = run(FakeSnapshot::new(vec![row])).await;
            assert_eq!(result.unwrap_err(), SourceTokenRowError::Invalid);
        }
    }

    #[tokio::test]
    async fn source_token_row_read_and_cleanup_failures_release_snapshot_and_fail_closed() {
        for stage in ["prepare", "sessions", "rows", "rollback"] {
            let mut fake = FakeSnapshot::new(vec![valid_row()]);
            match stage {
                "prepare" => fake.prepare = Err(SourceTokenRowError::ReadFailed),
                "sessions" => fake.sessions = Err(SourceTokenRowError::ReadFailed),
                "rows" => fake.rows = Err(SourceTokenRowError::ReadFailed),
                _ => fake.finish = Err(SourceTokenRowError::ReadFailed),
            }
            let (result, events) = run(fake).await;
            assert_eq!(result.unwrap_err(), SourceTokenRowError::ReadFailed);
            let expected = match stage {
                "prepare" => vec!["prepare", "rollback"],
                "sessions" => vec!["prepare", "sessions", "rollback"],
                _ => vec!["prepare", "sessions", "rows", "rollback"],
            };
            assert_eq!(*events.borrow(), expected);
        }
        let mut fake = FakeSnapshot::new(Vec::new());
        fake.finish = Err(SourceTokenRowError::ReadFailed);
        let (result, _) = run(fake).await;
        assert_eq!(result.unwrap_err(), SourceTokenRowError::RowMissing);
    }

    #[tokio::test]
    async fn source_token_row_debug_and_error_reports_contain_no_material() {
        let (result, _) = run(FakeSnapshot::new(vec![valid_row()])).await;
        let tokens = result.unwrap();
        let password = SourceDbPassword::from_bytes(b"synthetic-postgres-password").unwrap();
        let rendered = format!("{tokens:?} {password:?}");
        assert_eq!(
            rendered,
            "OwnerTokens([redacted]) SourceDbPassword([redacted])"
        );
        for error in [
            SourceTokenRowError::SourceStillRunning,
            SourceTokenRowError::RowMissing,
            SourceTokenRowError::RowsAmbiguous,
            SourceTokenRowError::TooLarge,
            SourceTokenRowError::KeyMismatch,
            SourceTokenRowError::Invalid,
            SourceTokenRowError::ReadFailed,
        ] {
            assert_eq!(error.to_string(), error.code());
            assert!(std::error::Error::source(&error).is_none());
            let rendered = format!("{error} {error:?}");
            for marker in [
                SYNTHETIC_KEY,
                SYNTHETIC_REFRESH,
                tokens.access_token().as_bytes(),
                b"synthetic-postgres-password",
            ] {
                assert!(!rendered.contains(std::str::from_utf8(marker).unwrap()));
            }
        }
    }

    #[tokio::test]
    async fn source_token_row_invalid_limits_refused_without_connecting() {
        let source = ReadOnlySource::parse("postgresql://synthetic@127.0.0.1/synthetic").unwrap();
        let password = SourceDbPassword::from_bytes(b"synthetic-postgres-password").unwrap();
        let key = Zeroizing::new(SYNTHETIC_KEY.to_vec());
        let limits = SourceDbReadLimits {
            connect_timeout: std::time::Duration::ZERO,
            ..Default::default()
        };
        assert_eq!(
            read_source_token_row(&source, &password, &key, limits)
                .await
                .unwrap_err(),
            SourceTokenRowError::ReadFailed
        );
    }
}
