// SPDX-License-Identifier: AGPL-3.0-only

//! `take-over-source-token` (DEC-172, design A.3): the migration source's
//! stored Tesla Owner login becomes the Hub's own, refreshed by the Hub alone.
//!
//! Order, so that a refusal reads no secret it does not need and opens no
//! connection it does not need:
//! 1. routed as an import: no window open on this host and the Hub's window
//!    Closed with its handback recorded (`window_active`, `handback_pending`);
//! 2. the source URL is checked (password-free, one host, one database);
//! 3. the data-directory lifetime lock is taken: refused `service_running`
//!    while the Hub service runs, so no refresh can run beside the write;
//! 4. standard input is read, bounded, and must be exactly one JSON object
//!    with `postgresPassword` and `encryptionKey`. A key naming a token is
//!    refused `source_credential_disabled`, anything else
//!    `setup_input_invalid`, before any connection;
//! 5. the store and the credential vault are opened;
//! 6. the source token row is read over the Hub's own read-only, repeatable
//!    read connection, which refuses `source_still_running` while another
//!    client is connected to the source database, and is decrypted and
//!    checked against Tesla's token grammar and issuer pin;
//! 7. the vault's single take-over writer stores it as origin `source_token`
//!    with the Hub as refresher, due for refresh at the next window.
//!
//! Every refusal keeps its stable code from the reader or the vault
//! (`slot_occupied`, `fresh_sign_in_required`, `refresh_token_spent`,
//! `source_still_running`, `source_token_key_mismatch`,
//! `source_token_invalid`, ...). Nothing is sent to Tesla, nothing is written
//! to the source, and no password, key, token or fingerprint is printed or
//! logged.

use std::{
    io::Read,
    path::Path,
};

use serde::Deserialize;
use tesatlas_hub::{
    config::HubConfig,
    credentials::{OwnerTokens, SourceDbPassword},
    db::HubStore,
    hub_user_process::{AdmittedUserHub, UserLifetimeLockError},
    import::source_db::token_row::{SourceTokenRowError, read_source_token_row},
    source_db_connection::ReadOnlySource,
    source_db_reader::SourceDbReadLimits,
};
use zeroize::Zeroizing;

use super::{CatalogueCheckpointGuard, OperatorRefusal, open_command_credential_vault, operator};

/// Largest standard input: the source database password and the source
/// application's encryption key as one JSON object.
pub(crate) const INPUT_MAX_BYTES: usize = 8192;
/// Largest encryption key accepted from standard input.
const ENCRYPTION_KEY_MAX_BYTES: usize = 1024;
/// The refusal while the Hub service holds the data directory.
pub(crate) const SERVICE_RUNNING: &str = "service_running";
/// The refusal of a source URL that is not one password-free database URL.
pub(crate) const SOURCE_URL_INVALID: &str = "source_url_invalid";

/// The secrets the verb reads from standard input; zeroised on drop.
pub(crate) struct TakeoverInput {
    pub(crate) password: SourceDbPassword,
    pub(crate) encryption_key: Zeroizing<Vec<u8>>,
}

/// One secret string from the input, moved straight into a zeroising buffer.
struct SecretText(Zeroizing<String>);

impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|text| Self(Zeroizing::new(text)))
    }
}

/// Exactly the two keys, each once (serde refuses a duplicate field).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInput {
    #[serde(rename = "postgresPassword")]
    postgres_password: SecretText,
    #[serde(rename = "encryptionKey")]
    encryption_key: SecretText,
}

fn input_invalid(message: &str) -> OperatorRefusal {
    OperatorRefusal::new(operator::admission::SETUP_INPUT_INVALID, message)
}

const INPUT_SHAPE: &str = "take-over-source-token reads one JSON object with exactly postgresPassword and encryptionKey";

/// Parses the verb's standard input. The keys are read first, with every
/// value skipped: any key that names a token is a source credential and is
/// refused as one, whatever else the input holds. Only then are the two
/// values read, into zeroising buffers.
pub(crate) fn parse_input(bytes: &[u8]) -> Result<TakeoverInput, OperatorRefusal> {
    let keys: std::collections::BTreeMap<String, serde::de::IgnoredAny> =
        serde_json::from_slice(bytes).map_err(|_| input_invalid(INPUT_SHAPE))?;
    if keys.keys().any(|key| key.to_ascii_lowercase().contains("token")) {
        let error = tesatlas_hub::auth::credential_sign_in::source_credential_disabled();
        return Err(OperatorRefusal::new(error.code(), error.to_string()));
    }
    let raw: RawInput = serde_json::from_slice(bytes).map_err(|_| input_invalid(INPUT_SHAPE))?;
    let password = SourceDbPassword::from_bytes(raw.postgres_password.0.as_bytes())
        .map_err(|_| input_invalid("the source database password is empty, too long or holds a line break"))?;
    let key = raw.encryption_key.0.as_bytes();
    if key.is_empty() || key.len() > ENCRYPTION_KEY_MAX_BYTES {
        return Err(input_invalid("the encryption key is empty or longer than 1024 bytes"));
    }
    Ok(TakeoverInput {
        password,
        encryption_key: Zeroizing::new(key.to_vec()),
    })
}

/// Reads standard input to its end, refusing more than the bound.
fn read_input(input: impl Read) -> Result<Zeroizing<Vec<u8>>, OperatorRefusal> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(INPUT_MAX_BYTES + 1));
    input
        .take(INPUT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| input_invalid("standard input is unreadable"))?;
    if bytes.len() > INPUT_MAX_BYTES {
        return Err(input_invalid("standard input is larger than 8 KiB"));
    }
    Ok(bytes)
}

/// The reader's refusal, with its code unchanged and fixed owner text.
pub(crate) fn reader_refusal(error: SourceTokenRowError) -> OperatorRefusal {
    let message = match error {
        SourceTokenRowError::SourceStillRunning => "another client is connected to the source database; stop the source application and its dashboards, then try again",
        SourceTokenRowError::RowMissing => "the source database holds no stored Tesla login",
        SourceTokenRowError::RowsAmbiguous => "the source database holds more than one stored Tesla login",
        SourceTokenRowError::TooLarge => "the source's stored Tesla login is larger than the Hub accepts",
        SourceTokenRowError::KeyMismatch => "the encryption key does not open the source's stored Tesla login; check the key, or sign in with Tesla in the Hub instead",
        SourceTokenRowError::Invalid => "the source's stored login is not a Tesla Owner login the Hub can use; sign in with Tesla in the Hub instead",
        SourceTokenRowError::ReadFailed => "the source database could not be read",
    };
    OperatorRefusal::new(error.code(), message)
}

/// A vault refusal, with its code unchanged and the owner text for it.
fn vault_refusal(error: tesatlas_hub::CredentialVaultEntryError) -> OperatorRefusal {
    let code = error.code();
    let message = tesatlas_hub::auth::credential_sign_in::credential_owner_message(code, None, None, None)
        .unwrap_or_else(|| "the Hub could not store the login".to_owned());
    OperatorRefusal::new(code, message)
}

/// The report of a completed take-over: no token, fingerprint or key.
pub(crate) fn takeover_report() -> serde_json::Value {
    serde_json::json!({
        "status": "taken_over",
        "origin": "source_token",
        "firstRefresh": "next_window",
        // The reader refuses while any other client is connected, so a
        // completed take-over saw none.
        "sourceSessionsSeen": 0,
        "sourceNeverMutated": true,
    })
}

/// Runs the verb once its import admission is held (see the module notes).
pub(crate) async fn run_take_over(config_path: &Path, source: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let config = HubConfig::load(config_path)?;
    let source = ReadOnlySource::parse(source).map_err(|_| {
        OperatorRefusal::new(SOURCE_URL_INVALID, "the source must be a password-free postgres URL naming one host and one database")
    })?;
    let admission = AdmittedUserHub::admit(&config.data_dir).map_err(|error| -> Box<dyn std::error::Error> {
        match error {
            UserLifetimeLockError::AlreadyRunning => {
                OperatorRefusal::new(SERVICE_RUNNING, "the Hub service is running; stop Hub and try again").into()
            }
            other => other.into(),
        }
    })?;
    let input = parse_input(&read_input(std::io::stdin().lock())?)?;
    // The existing-store lane is read-only and carries no credential
    // connection: it proves the store exists, then the admitted write lane
    // is opened (as sign-out does).
    drop(HubStore::open_existing(&config.data_dir)?);
    let store = HubStore::initialize(&config.data_dir)?;
    let entry = open_command_credential_vault(store.clone(), admission, &config.data_dir).map_err(vault_refusal)?;
    let tokens = read_tokens(&source, &input).await.map_err(reader_refusal)?;
    drop(input);
    let mut checkpoint = CatalogueCheckpointGuard::new(store);
    let stored = entry.store_source_takeover(&tokens);
    drop(tokens);
    drop(entry);
    stored.map_err(vault_refusal)?;
    checkpoint.finish()?;
    tracing::info!("the source's Tesla login was taken over; first refresh at the next window");
    Ok(takeover_report())
}

/// The source pair: from the Hub's own read-only source connection, or, in a
/// test build whose test names one, from the test's double in its place.
async fn read_tokens(source: &ReadOnlySource, input: &TakeoverInput) -> Result<OwnerTokens, SourceTokenRowError> {
    #[cfg(feature = "journey-doubles")]
    if let Some(double) = double::read() {
        return double;
    }
    read_source_token_row(source, &input.password, &input.encryption_key, SourceDbReadLimits::default()).await
}

/// The source reader's test double (test builds only, see
/// `journey_doubles`): a test names a JSON file in [`double::ENV`] holding
/// either `{"refusal": "<reader code>"}` or a synthetic `{"access",
/// "refresh"}` pair, which stands in for the source database.
#[cfg(feature = "journey-doubles")]
mod double {
    use super::*;

    pub(crate) const ENV: &str = "TESATLAS_HUB_SOURCE_TAKEOVER_DOUBLE";

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Double {
        #[serde(default)]
        refusal: Option<String>,
        #[serde(default)]
        access: Option<String>,
        #[serde(default)]
        refresh: Option<String>,
    }

    pub(super) fn read() -> Option<Result<OwnerTokens, SourceTokenRowError>> {
        let path = std::env::var_os(ENV)?;
        let double: Double = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        if let Some(code) = double.refusal {
            let all = [
                SourceTokenRowError::SourceStillRunning,
                SourceTokenRowError::RowMissing,
                SourceTokenRowError::RowsAmbiguous,
                SourceTokenRowError::TooLarge,
                SourceTokenRowError::KeyMismatch,
                SourceTokenRowError::Invalid,
                SourceTokenRowError::ReadFailed,
            ];
            return Some(Err(all.into_iter().find(|error| error.code() == code).unwrap_or(SourceTokenRowError::ReadFailed)));
        }
        let (access, refresh) = (double.access?, double.refresh?);
        Some(
            OwnerTokens::from_file_bytes(Zeroizing::new(access.into_bytes()), Zeroizing::new(refresh.into_bytes()))
                .map_err(|_| SourceTokenRowError::Invalid),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(input: &[u8]) -> String {
        match parse_input(input) {
            Ok(_) => "accepted".to_owned(),
            Err(refusal) => refusal.code,
        }
    }

    #[test]
    fn source_takeover_input_accepts_exactly_the_two_keys() {
        let parsed = parse_input(br#"{"postgresPassword":"synthetic-password","encryptionKey":"synthetic-key"}"#).expect("accepted");
        assert_eq!(parsed.encryption_key.as_slice(), b"synthetic-key");
        // Whitespace and key order do not matter; escapes are decoded.
        let parsed = parse_input(b" {\"encryptionKey\":\"k\\u0065y\",\n\"postgresPassword\":\"p\"} \n").expect("accepted");
        assert_eq!(parsed.encryption_key.as_slice(), b"key");
    }

    #[test]
    fn source_takeover_input_refuses_token_keys_as_source_credentials() {
        for input in [
            &br#"{"postgresPassword":"p","encryptionKey":"k","refreshToken":"r"}"#[..],
            br#"{"accessToken":"a"}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k","TOKEN":1}"#,
            br#"{"postgresPassword":"p","source_token_key":"k"}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k","refreshToken":"r","refreshToken":"s"}"#,
        ] {
            assert_eq!(code(input), "source_credential_disabled", "{}", String::from_utf8_lossy(input));
        }
    }

    #[test]
    fn source_takeover_input_refuses_every_other_shape() {
        for input in [
            &b""[..],
            b"[]",
            b"null",
            b"\"text\"",
            b"{}",
            br#"{"postgresPassword":"p"}"#,
            br#"{"encryptionKey":"k"}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k","extra":"x"}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k","postgresPassword":"q"}"#,
            br#"{"PostgresPassword":"p","encryptionKey":"k"}"#,
            br#"{"postgresPassword":1,"encryptionKey":"k"}"#,
            br#"{"postgresPassword":"p","encryptionKey":null}"#,
            br#"{"postgresPassword":"p","encryptionKey":{"k":"v"}}"#,
            br#"{"postgresPassword":"","encryptionKey":"k"}"#,
            br#"{"postgresPassword":"p\nq","encryptionKey":"k"}"#,
            br#"{"postgresPassword":"p","encryptionKey":""}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k"} trailing"#,
            b"\xef\xbb\xbf{\"postgresPassword\":\"p\",\"encryptionKey\":\"k\"}",
            b"{\"postgresPassword\":\"p\",\"encryptionKey\":\"\xff\"}",
        ] {
            assert_eq!(code(input), operator::admission::SETUP_INPUT_INVALID, "{}", String::from_utf8_lossy(input));
        }
        let long_key = format!(r#"{{"postgresPassword":"p","encryptionKey":"{}"}}"#, "k".repeat(ENCRYPTION_KEY_MAX_BYTES + 1));
        assert_eq!(code(long_key.as_bytes()), operator::admission::SETUP_INPUT_INVALID);
        let at_bound = format!(r#"{{"postgresPassword":"p","encryptionKey":"{}"}}"#, "k".repeat(ENCRYPTION_KEY_MAX_BYTES));
        assert_eq!(code(at_bound.as_bytes()), "accepted");
    }

    #[test]
    fn source_takeover_input_is_bounded() {
        assert!(read_input(&vec![b' '; INPUT_MAX_BYTES][..]).is_ok());
        let error = read_input(&vec![b' '; INPUT_MAX_BYTES + 1][..]).err().expect("refused");
        assert_eq!(error.code, operator::admission::SETUP_INPUT_INVALID);
    }

    #[test]
    fn source_takeover_refusals_keep_the_reader_codes_and_the_report_holds_no_secret() {
        for error in [
            SourceTokenRowError::SourceStillRunning,
            SourceTokenRowError::RowMissing,
            SourceTokenRowError::RowsAmbiguous,
            SourceTokenRowError::TooLarge,
            SourceTokenRowError::KeyMismatch,
            SourceTokenRowError::Invalid,
            SourceTokenRowError::ReadFailed,
        ] {
            assert_eq!(reader_refusal(error).code, error.code());
        }
        assert_eq!(
            takeover_report(),
            serde_json::json!({"status":"taken_over","origin":"source_token","firstRefresh":"next_window","sourceSessionsSeen":0,"sourceNeverMutated":true})
        );
    }
}
