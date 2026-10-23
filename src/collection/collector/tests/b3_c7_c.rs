// SPDX-License-Identifier: AGPL-3.0-only

//! C7 part C test for the enrichment caller (spec section 7.5, C7 T72): the
//! pre-claim gate leaves a refused or paced job pending, untouched. A store
//! with one pending job, the default geocoder (the public host), a stored
//! refusal record, a limiter of the test's own and an injected `now_ms`;
//! nothing is sent.

use super::*;

/// The job seeded the way the kept `disabled_geocoder_leaves_pending_jobs_untouched`
/// seeds its own (C7 T72).
const JOB_KEY: &str = "gate-closed-job";

fn store_with_pending_job() -> (tempfile::TempDir, HubStore) {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("test", "geocoder-gate"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "vehicle"), 1_000)
        .expect("vehicle");
    store
        .open()
        .expect("database")
        .execute(
            "INSERT INTO address_enrichment_jobs(
                    job_key, vehicle_id, target_type, target_id, field,
                    latitude, longitude, status, attempts, next_attempt_ms,
                    lease_until_ms
                 ) VALUES (?1, ?2, 'drive', 1, 'start_address', 1.0, 2.0,
                           'pending', 0, 0, 0)",
            rusqlite::params![JOB_KEY, vehicle.vehicle_id.to_string()],
        )
        .expect("address job");
    (temp, store)
}

fn config_with_default_geocoder(directory: &std::path::Path) -> HubConfig {
    HubConfig {
        data_dir: directory.to_path_buf(),
        bind: "127.0.0.1:8080".parse().expect("bind"),
        tls: None,
        http: Default::default(),
        pairing: Default::default(),
        collector: crate::config::CollectorConfig::default(),
        geocoder: crate::config::GeocoderConfig::default(),
        teslamate: crate::config::TeslaMateConfig::default(),
        terrain: TerrainConfig::default(),
    }
}

/// The job's `(status, attempts, last_error)`.
fn job_state(store: &HubStore) -> (String, i64, Option<String>) {
    store
        .open()
        .expect("database")
        .query_row(
            "SELECT status, attempts, last_error FROM address_enrichment_jobs WHERE job_key = ?1",
            [JOB_KEY],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("job state")
}

#[tokio::test]
async fn c7_t72_jobs_pending() {
    let (temp, store) = store_with_pending_job();
    let config = config_with_default_geocoder(temp.path());
    assert!(config.geocoder.enabled);
    let limiter = crate::geocoder::isolate_global_limiter_for_test();
    let cursor_key = CursorKey::from_bytes([7; 32]);
    store
        .set_geocoder_provider_state(Some(
            r#"{"v":1,"endpoint":"https://nominatim.openstreetmap.org:443/","host":"nominatim.openstreetmap.org","status":429,"refusals":1,"since_ms":1000,"until_ms":3601000}"#,
        ))
        .expect("record");

    // While the pause is in force nothing is claimed: the job is untouched, not
    // retried with an error (without the gate it would be claimed and retried
    // with the test-build refusal of the public host).
    assert!(
        !run_address_enrichment_once(&store, &config, &cursor_key, &[], 1_000)
            .await
            .expect("a refused step")
    );
    assert_eq!(job_state(&store), ("pending".to_owned(), 0, None));

    // The same while the endpoint's next slot is not free, with no record.
    store.set_geocoder_provider_state(None).expect("no record");
    *limiter.lock().unwrap() = Some(std::time::Instant::now());
    assert!(
        !run_address_enrichment_once(&store, &config, &cursor_key, &[], 1_000)
            .await
            .expect("a paced step")
    );
    assert_eq!(job_state(&store), ("pending".to_owned(), 0, None));

    // Control: with the gate open the same step does claim the job, and this
    // build's refusal of the public host retries it with the kept error text.
    *limiter.lock().unwrap() = None;
    assert!(
        !run_address_enrichment_once(&store, &config, &cursor_key, &[], 1_000)
            .await
            .expect("an open step")
    );
    let (status, attempts, last_error) = job_state(&store);
    assert_eq!(status, "retry");
    assert_eq!(attempts, 1);
    assert_eq!(last_error.as_deref(), Some("geocoder endpoint is invalid"));
}
