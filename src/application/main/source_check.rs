// SPDX-License-Identifier: AGPL-3.0-only

/// Ensures every short-lived local writer leaves a checkpointed catalogue,
/// including error paths. Successful commands surface checkpoint failures;
/// failing commands preserve their primary error while still attempting it.
struct CatalogueCheckpointGuard {
    store: HubStore,
    armed: bool,
}

impl CatalogueCheckpointGuard {
    fn new(store: HubStore) -> Self {
        Self { store, armed: true }
    }

    fn finish(&mut self) -> Result<(), teslatlas_hub::db::StoreError> {
        let result = self.store.checkpoint_catalogue_for_immutable_read();
        self.armed = false;
        result
    }
}

impl Drop for CatalogueCheckpointGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.store.checkpoint_catalogue_for_immutable_read();
        }
    }
}

#[cfg(unix)]
const SOURCE_DB_REQUIRED_VERSION: &str = "4.2.0";

#[cfg(unix)]
fn schema_set_label(digest: &str) -> Option<String> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    digest.get(..12).map(str::to_owned)
}

#[cfg(unix)]
fn source_db_version_confirmation(
    accept_schema_evidence: bool,
) -> (&'static str, &'static str, &'static str) {
    if accept_schema_evidence {
        (
            "compatible",
            "v4_2_compatible_schema",
            "The recognised source schema was verified and the version limitation was acknowledged. The source database is read-only and ready for import; its data is never deleted.",
        )
    } else {
        (
            "confirmation_required",
            "v4_2_version_unconfirmed",
            "A schema check cannot establish the source application's running release. Confirm release 4.2.0 or later, then retry with --accept-schema-evidence.",
        )
    }
}

#[cfg(unix)]
fn source_check_success_report(
    car_id: i64,
    snapshot: &SourceDbCheckSnapshot,
    history_only: bool,
    accept_schema_evidence: bool,
) -> serde_json::Value {
    let (status, reason_code, guidance) = if history_only {
        if accept_schema_evidence {
            (
                "compatible",
                "v4_2_compatible_schema",
                "The read-only source database has the recognised history schema. The source application's release is unknown; no Tesla credentials were inspected or imported.",
            )
        } else {
            (
                "confirmation_required",
                "v4_2_schema_acknowledgement_required",
                "The source database has the recognised history schema, but the source application's release is unknown. Acknowledge schema-only history import before continuing.",
            )
        }
    } else {
        source_db_version_confirmation(accept_schema_evidence)
    };
    serde_json::json!({
        "status":status,"reasonCode":reason_code,"requiredVersion":SOURCE_DB_REQUIRED_VERSION,
        "pinnedSourceRevision":snapshot.schema.pinned_source_revision,
        "observedMigrationVersion":snapshot.schema.observed_migration_version,
        "observedMigrationCount":snapshot.schema.observed_migration_count,
        "minimumSupportedMigrationVersion":snapshot.schema.minimum_supported_migration_version,
        "maximumValidatedMigrationVersion":snapshot.schema.maximum_validated_migration_version,
        "selectedCarId":car_id,"connection":snapshot.connection,"selectedCar":snapshot.selected_car,
        "openSessions":snapshot.open_sessions,"selectedCarCounts":snapshot.selected_car_counts,
        "sourceTotals":snapshot.source_totals,"sourceTokensRelationPresent":snapshot.source_tokens_relation_present,
        "legacyTokenPair":snapshot.legacy_token_pair,"sourceNeverMutated":true,
        "credentialScope":if history_only {"history-only"} else {"credentials-and-history"},
        "applicationVersionStatus":if history_only {"unknown"} else {"operator-acknowledged"},
        "versionEvidence":"database_schema_only","schemaEvidence":"v4_2_compatible_schema",
        "versionAcknowledged":accept_schema_evidence,"guidance":guidance,
        "sourceSchemaRecognised":true,"schemaSetLabel":schema_set_label(snapshot.schema.pinned_migration_set_sha256)
    })
}

#[cfg(unix)]
fn source_check_failure_report(
    car_id: i64,
    status: &str,
    reason_code: &str,
    observed_migration_version: Option<i64>,
    guidance: &str,
) -> serde_json::Value {
    let recognition = match reason_code {
        "older_than_v4_2_compatible_schema"
        | "newer_than_v4_2_compatible_schema"
        | "schema_mismatch" => Some(false),
        _ => None,
    };
    serde_json::json!({"status":status,"reasonCode":reason_code,"requiredVersion":SOURCE_DB_REQUIRED_VERSION,
        "pinnedSourceRevision":SOURCE_DB_V4_SOURCE_REVISION,"maximumValidatedMigrationVersion":MAX_VALIDATED_MIGRATION,
        "expectedMigrationCount":SOURCE_DB_V4_MIGRATION_COUNT,"selectedCarId":car_id,
        "observedMigrationVersion":observed_migration_version,"guidance":guidance,"sourceSchemaRecognised":recognition})
}

#[cfg(unix)]
fn print_source_check_success(
    car_id: i64,
    snapshot: &SourceDbCheckSnapshot,
    history_only: bool,
    accept_schema_evidence: bool,
) {
    println!(
        "{}",
        source_check_success_report(car_id, snapshot, history_only, accept_schema_evidence)
    );
}

#[cfg(unix)]
fn print_source_check_failure(
    car_id: i64,
    status: &str,
    reason_code: &str,
    observed_migration_version: Option<i64>,
    guidance: &str,
) {
    println!(
        "{}",
        source_check_failure_report(
            car_id,
            status,
            reason_code,
            observed_migration_version,
            guidance
        )
    );
}

#[cfg(unix)]
fn source_check_failure_details(
    error: &SourceDbReaderError,
) -> (&'static str, &'static str, Option<i64>, &'static str) {
    match error {
        SourceDbReaderError::Schema(SchemaCompatibilityError::LegacyMigration {
            found, ..
        }) => (
            "incompatible",
            "older_than_v4_2_compatible_schema",
            Some(*found),
            "Back up source application, update it to version 4.2.0 or newer, allow its migrations to finish, then retry.",
        ),
        SourceDbReaderError::Schema(SchemaCompatibilityError::UnreviewedMigration {
            found,
            ..
        }) => (
            "incompatible",
            "newer_than_v4_2_compatible_schema",
            Some(*found),
            "This Hub build supports the reviewed source application v4.2-compatible schema only. Do not downgrade a live database; use a separate compatible backup or wait for a reviewed adapter.",
        ),
        SourceDbReaderError::Schema(_) | SourceDbReaderError::MissingMigrationVersion => (
            "incompatible",
            "schema_mismatch",
            None,
            "The source database does not match the recognised schema contract. Do not modify or downgrade the live database.",
        ),
        SourceDbReaderError::SelectedCarMissing { .. } => (
            "incompatible",
            "selected_car_missing",
            None,
            "Choose a car ID that exists in the compatible source database, then retry.",
        ),
        SourceDbReaderError::AmbiguousOpenSession { .. } => (
            "incompatible",
            "ambiguous_open_session",
            None,
            "The source database has more than one open drive, charging process, or state. Finish or repair those sessions, then retry.",
        ),
        SourceDbReaderError::LegacyTokenPairMissing
        | SourceDbReaderError::LegacyTokenPairAmbiguous
        | SourceDbReaderError::LegacyTokenPairEmpty
        | SourceDbReaderError::LegacyTokenCiphertextTooLarge { .. } => (
            "incompatible",
            "legacy_token_pair_invalid",
            None,
            "The source application must contain exactly one non-empty, bounded legacy OAuth token pair before migration. Repair or sign in to the source application again, then retry.",
        ),
        SourceDbReaderError::SourceReadBudgetExceeded { .. } => (
            "incompatible",
            "source_read_budget_exceeded",
            None,
            "The source database returned more rows or bytes than this Hub reads at once, so nothing was imported. Check that the source is an unmodified, supported source database, then retry.",
        ),
        _ => (
            "unavailable",
            "source_unavailable",
            None,
            "Check the password-free PostgreSQL URL, read-only database credentials, network, and TLS trust, then retry.",
        ),
    }
}

#[cfg(unix)]
async fn run_source_check(
    source_url: &str,
    car_id: i64,
    postgres_password_file: &Path,
    history_only: bool,
    accept_schema_evidence: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = match ReadOnlySource::parse(source_url) {
        Ok(source) => source,
        Err(_) => {
            print_source_check_failure(
                car_id,
                "unavailable",
                "invalid_source",
                None,
                "Use a password-free PostgreSQL URL with an explicit read-only user, then retry.",
            );
            return Err(std::io::Error::other(
                "source application compatibility check failed; see JSON report",
            )
            .into());
        }
    };
    if history_only && !source.is_loopback() {
        return Err("history-only import requires a numeric loopback PostgreSQL source".into());
    }
    let password = match read_migration_postgres_password(postgres_password_file) {
        Ok(password) => password,
        Err(_) => {
            print_source_check_failure(
                car_id,
                "unavailable",
                "credential_unavailable",
                None,
                "Provide one safe, bounded PostgreSQL password file or stdin value, then retry.",
            );
            return Err(std::io::Error::other(
                "source application compatibility check failed; see JSON report",
            )
            .into());
        }
    };
    let snapshot = if history_only {
        check_source_history_compatibility(
            &source,
            &password,
            car_id,
            SourceDbReadLimits::default(),
        )
        .await
    } else {
        check_source_compatibility(&source, &password, car_id, SourceDbReadLimits::default()).await
    };
    match snapshot {
        Ok(snapshot) => {
            print_source_check_success(car_id, &snapshot, history_only, accept_schema_evidence);
            if accept_schema_evidence {
                Ok(())
            } else {
                Err(std::io::Error::other(
                    "source application version confirmation required; see JSON report",
                )
                .into())
            }
        }
        Err(error) => {
            let (status, reason_code, observed, guidance) = source_check_failure_details(&error);
            print_source_check_failure(car_id, status, reason_code, observed, guidance);
            Err(std::io::Error::other(
                "source application compatibility check failed; see JSON report",
            )
            .into())
        }
    }
}
