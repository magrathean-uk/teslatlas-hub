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
const TESLAMATE_REQUIRED_VERSION: &str = "4.1.1";

#[cfg(unix)]
fn print_teslamate_check_success(car_id: i64, snapshot: &TeslaMateCheckSnapshot) {
    println!(
        "{}",
        serde_json::json!({
            "status": "compatible",
            "reasonCode": "exact_4_1_1",
            "requiredVersion": TESLAMATE_REQUIRED_VERSION,
            "pinnedSourceRevision": snapshot.schema.pinned_source_revision,
            "observedMigrationVersion": snapshot.schema.observed_migration_version,
            "observedMigrationCount": snapshot.schema.observed_migration_count,
            "minimumSupportedMigrationVersion": snapshot.schema.minimum_supported_migration_version,
            "maximumValidatedMigrationVersion": snapshot.schema.maximum_validated_migration_version,
            "selectedCarId": car_id,
            "connection": snapshot.connection,
            "selectedCar": snapshot.selected_car,
            "openSessions": snapshot.open_sessions,
            "selectedCarCounts": snapshot.selected_car_counts,
            "sourceTotals": snapshot.source_totals,
            "sourceTokensRelationPresent": snapshot.source_tokens_relation_present,
            "legacyTokenPair": snapshot.legacy_token_pair,
            "sourceNeverMutated": true,
            "guidance": "Exact TeslaMate 4.1.1 compatibility verified. The source is read-only and ready for migration. TeslaMate data is not deleted.",
        })
    );
}

#[cfg(unix)]
fn print_teslamate_check_failure(
    car_id: i64,
    status: &str,
    reason_code: &str,
    observed_migration_version: Option<i64>,
    guidance: &str,
) {
    println!(
        "{}",
        serde_json::json!({
            "status": status,
            "reasonCode": reason_code,
            "requiredVersion": TESLAMATE_REQUIRED_VERSION,
            "pinnedSourceRevision": TESLAMATE_V4_SOURCE_REVISION,
            "maximumValidatedMigrationVersion": MAX_VALIDATED_MIGRATION,
            "expectedMigrationCount": TESLAMATE_V4_MIGRATION_COUNT,
            "selectedCarId": car_id,
            "observedMigrationVersion": observed_migration_version,
            "guidance": guidance,
        })
    );
}

#[cfg(unix)]
fn teslamate_check_failure_details(
    error: &TeslaMateReaderError,
) -> (&'static str, &'static str, Option<i64>, &'static str) {
    match error {
        TeslaMateReaderError::Schema(SchemaCompatibilityError::LegacyMigration {
            found, ..
        }) => (
            "incompatible",
            "older_than_4_1_1",
            Some(*found),
            "Back up TeslaMate, update it to exact version 4.1.1, allow its migrations to finish, then retry.",
        ),
        TeslaMateReaderError::Schema(SchemaCompatibilityError::UnreviewedMigration {
            found,
            ..
        }) => (
            "incompatible",
            "newer_than_4_1_1",
            Some(*found),
            "This Hub build supports exact TeslaMate 4.1.1 only. Do not downgrade a live database; use a separate compatible backup or wait for a reviewed adapter.",
        ),
        TeslaMateReaderError::Schema(_) | TeslaMateReaderError::MissingMigrationVersion => (
            "incompatible",
            "schema_mismatch",
            None,
            "The source does not match the exact TeslaMate 4.1.1 migration and physical-schema contract. Do not modify or downgrade the live database.",
        ),
        TeslaMateReaderError::SelectedCarMissing { .. } => (
            "incompatible",
            "selected_car_missing",
            None,
            "Choose a car ID that exists in the compatible TeslaMate source, then retry.",
        ),
        TeslaMateReaderError::AmbiguousOpenSession { .. } => (
            "incompatible",
            "ambiguous_open_session",
            None,
            "The TeslaMate source has more than one open drive, charging process, or state. Finish or repair those sessions, then retry.",
        ),
        TeslaMateReaderError::LegacyTokenPairMissing
        | TeslaMateReaderError::LegacyTokenPairAmbiguous
        | TeslaMateReaderError::LegacyTokenPairEmpty
        | TeslaMateReaderError::LegacyTokenCiphertextTooLarge { .. } => (
            "incompatible",
            "legacy_token_pair_invalid",
            None,
            "TeslaMate must contain exactly one non-empty, bounded legacy OAuth token pair before migration. Repair or re-login to TeslaMate, then retry.",
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
async fn run_teslamate_check(
    source_url: &str,
    car_id: i64,
    postgres_password_file: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let source = match ReadOnlySource::parse(source_url) {
        Ok(source) => source,
        Err(_) => {
            print_teslamate_check_failure(
                car_id,
                "unavailable",
                "invalid_source",
                None,
                "Use a password-free PostgreSQL URL with an explicit read-only user, then retry.",
            );
            return Err(std::io::Error::other(
                "TeslaMate compatibility check failed; see JSON report",
            )
            .into());
        }
    };
    let password = match read_migration_postgres_password(postgres_password_file) {
        Ok(password) => password,
        Err(_) => {
            print_teslamate_check_failure(
                car_id,
                "unavailable",
                "credential_unavailable",
                None,
                "Provide one safe, bounded PostgreSQL password file or stdin value, then retry.",
            );
            return Err(std::io::Error::other(
                "TeslaMate compatibility check failed; see JSON report",
            )
            .into());
        }
    };
    match check_teslamate_compatibility(&source, &password, car_id, TeslaMateReadLimits::default())
        .await
    {
        Ok(snapshot) => {
            print_teslamate_check_success(car_id, &snapshot);
            Ok(())
        }
        Err(error) => {
            let (status, reason_code, observed, guidance) = teslamate_check_failure_details(&error);
            print_teslamate_check_failure(car_id, status, reason_code, observed, guidance);
            Err(
                std::io::Error::other("TeslaMate compatibility check failed; see JSON report")
                    .into(),
            )
        }
    }
}
