// SPDX-License-Identifier: AGPL-3.0-only

use serde_json::json;

use super::*;
use crate::{
    db::HubStore,
    teslamate_physical_fragments::tests::{
        registered_admission_binding, seed_roots, seed_updates, stage_limits,
    },
    teslamate_stage::{TeslaMateStageLimits, TeslaMateStageTable},
};

/// Replay one retained sealed source without PostgreSQL or catalogue writes.
/// Full mode emits complete candidates; row mode is explicitly a component measurement.
#[test]
#[cfg(unix)]
#[ignore = "explicit retained private physical stage measurement"]
fn measure_retained_physical_stage() {
    use crate::teslamate_projection::{TeslaMateCarPhysicalV2_2, TeslaMatePositionPhysicalV2_2};
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
        path::{Path, PathBuf},
        time::Instant,
    };

    fn checked<T, E>(value: Result<T, E>, message: &'static str) -> T {
        value.unwrap_or_else(|_| panic!("{message}"))
    }
    fn retain_pack(source: &Path, target: &Path, expected: Sha256Digest) {
        let mut source = checked(
            OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(source),
            "pack read failed",
        );
        let mut target = checked(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                .open(target),
            "pack output failed",
        );
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 65_536];
        loop {
            let count = checked(source.read(&mut buffer), "pack read failed");
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
            checked(target.write_all(&buffer[..count]), "pack copy failed");
        }
        assert!(
            Sha256Digest::from_bytes(digest.finalize().into()) == expected,
            "retained pack digest differs from verified candidate"
        );
        checked(target.sync_all(), "retained pack sync failed");
    }
    fn same_bytes(left: &Path, right: &Path) -> bool {
        let mut left = checked(File::open(left), "left pack read failed");
        let mut right = checked(File::open(right), "right pack read failed");
        let mut left_bytes = [0_u8; 65_536];
        let mut right_bytes = [0_u8; 65_536];
        loop {
            let count = checked(left.read(&mut left_bytes), "left pack read failed");
            if count == 0 {
                return checked(right.read(&mut right_bytes[..1]), "right pack read failed") == 0;
            }
            if right.read_exact(&mut right_bytes[..count]).is_err()
                || left_bytes[..count] != right_bytes[..count]
            {
                return false;
            }
        }
    }

    let input = PathBuf::from(
        std::env::var_os("TESLATLAS_HUB_BENCH_STAGE")
            .expect("retained private stage environment is required"),
    );
    assert!(input.is_absolute(), "benchmark input must be absolute");
    let output = PathBuf::from(
        std::env::var_os("TESLATLAS_HUB_BENCH_OUTPUT_DIRECTORY")
            .expect("private benchmark output directory is required"),
    );
    assert!(output.is_absolute(), "benchmark output must be absolute");
    let output_fd = checked(
        crate::runtime::development_event_log::validated_directory(&output),
        "benchmark output must be an existing private directory",
    );
    let full = match std::env::var("TESLATLAS_HUB_BENCH_MODE").as_deref() {
        Ok("full") => true,
        Ok("rows") | Err(_) => false,
        _ => panic!("benchmark mode must be rows or full"),
    };
    let row_limit: u64 = std::env::var("TESLATLAS_HUB_BENCH_POSITION_ROWS")
        .map_or(50_000, |value| value.parse().expect("benchmark row limit"));
    assert!(
        (1..=1_000_000).contains(&row_limit),
        "component row limit must be 1 through 1000000"
    );
    let started = Instant::now();
    let stage = checked(
        TeslaMateStage::open_sealed(&input),
        "sealed input admission failed",
    );
    let open_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(
        checked(stage.format(), "stage format") == TeslaMateStageFormat::PhysicalV3,
        "benchmark requires a physical-v3 stage"
    );
    let stats = checked(stage.stats(), "sealed input statistics");
    let roots = checked(
        stage.page::<TeslaMateCarPhysicalV2_2>(TeslaMateStageTable::Cars, 0, 2),
        "car root read failed",
    );
    assert!(
        roots.rows.len() == 1 && roots.next_after_id.is_none(),
        "exactly one car root required"
    );
    let selected_car_id = i64::from(roots.rows[0].value.id);
    let binding = ProjectionBinding {
        installation_id: Uuid::from_u128(1),
        account_id: Uuid::from_u128(2),
        vehicle_id: Uuid::from_u128(3),
        generation: 1,
        selected_car_id,
    };
    let key = CursorKey::from_bytes([0x55; 32]);
    let sequence = SequenceRange {
        from_exclusive: 1,
        to_inclusive: 1,
    };
    let limits = ProtocolLimits::hub_sync_v1_1_3_schema_2_2();
    let fragment_limits = TeslaMatePhysicalFragmentLimits::default();
    let started = Instant::now();
    let stage_digest = checked(
        stage.sealed_content_digest(),
        "sealed content verification failed",
    );
    let digest_ms = started.elapsed().as_secs_f64() * 1000.0;
    let snapshot_id = physical_v3_snapshot_id(stage_digest, &binding);
    eprintln!(
        "{}",
        json!({"event": "retained_stage_admitted", "open_ms": open_ms,
        "digest_ms": digest_ms, "source_rows": stats.row_count, "payload_bytes": stats.payload_bytes})
    );

    let mut runs = Vec::new();
    let mut deterministic = Vec::new();
    let mut first_manifest = None;
    let mut first_paths: Vec<PathBuf> = Vec::new();
    for run in 0..2 {
        let run_dir = output.join(format!("run-{run}"));
        checked(
            fs::DirBuilder::new().mode(0o700).create(&run_dir),
            "run directory must be new",
        );
        if full {
            let writer = ProjectionPackWriter::with_limits(run_dir.join("packs"), limits)
                .with_minimum_free_bytes(stats.limits.minimum_free_bytes);
            checked(
                writer.ensure_full_snapshot_capacity_for_capture(
                    stats.limits.max_stage_bytes,
                    stats.limits.minimum_free_bytes,
                ),
                "pack capacity admission failed",
            );
            let started = Instant::now();
            let candidate = checked(
                write_staged_physical_updates_snapshot_v3_with_limits(
                    &stage,
                    &writer,
                    binding.clone(),
                    snapshot_id,
                    sequence,
                    &key,
                    fragment_limits,
                ),
                "full physical reconstruction failed",
            );
            let packing_ms = started.elapsed().as_secs_f64() * 1000.0;
            assert!(candidate.binding == binding, "candidate binding changed");
            assert!(
                candidate.logical_source_rows == stats.row_count,
                "source row count changed"
            );
            let manifest_bytes = checked(
                serde_json::to_vec(&candidate.manifest),
                "manifest encode failed",
            );
            let manifest_digest = Sha256Digest::from_bytes(Sha256::digest(&manifest_bytes).into());
            if let Some(first) = &first_manifest {
                assert!(
                    *first == candidate.manifest,
                    "ordered manifest differs between identical-input runs"
                );
            } else {
                first_manifest = Some(candidate.manifest.clone());
            }
            let started = Instant::now();
            let mut paths = Vec::new();
            let mut chunks = Vec::new();
            for chunk in &candidate.chunks {
                let retained = run_dir.join(format!("{}.sqlite.zst", chunk.metadata.ordinal));
                retain_pack(&chunk.path, &retained, chunk.metadata.sha256);
                if run == 1 {
                    assert!(
                        same_bytes(&first_paths[paths.len()], &retained),
                        "compressed bytes differ between identical-input runs"
                    );
                }
                paths.push(retained);
                chunks.push(json!({"ordinal": chunk.metadata.ordinal, "sha256": chunk.metadata.sha256,
                    "compressed_bytes": chunk.metadata.compressed_bytes,
                    "uncompressed_bytes": chunk.metadata.uncompressed_bytes, "row_count": chunk.metadata.row_count}));
            }
            let metadata = json!({"manifest_sha256": manifest_digest,
                "signed_rows": candidate.manifest.total_rows, "chunks": chunks});
            deterministic.push(metadata.clone());
            runs.push(json!({"run": run, "packing_ms": packing_ms,
                "retain_compare_ms": started.elapsed().as_secs_f64() * 1000.0,
                "outputs": paths, "metadata": metadata}));
            if run == 0 {
                first_paths = paths;
            }
            drop(candidate); // Remove only writer-created temporary candidates; retained copies stay private.
        } else {
            let started = Instant::now();
            let mut after_id = 0;
            let mut rows = 0_u64;
            let mut projected_bytes = 0_u64;
            let mut digest = Sha256::new();
            while rows < row_limit {
                let page_size =
                    u32::try_from((row_limit - rows).min(10_000)).expect("bounded page size");
                let page = checked(
                    stage.page::<TeslaMatePositionPhysicalV2_2>(
                        TeslaMateStageTable::Positions,
                        after_id,
                        page_size,
                    ),
                    "position page failed",
                );
                if page.rows.is_empty() {
                    break;
                }
                for row in page.rows {
                    assert!(
                        row.source_id == i64::from(row.value.id)
                            && i64::from(row.value.car_id) == selected_car_id,
                        "position identity mismatch"
                    );
                    let projected: crate::hub_pack::ProjectionPositionV2_2 = row.value.into();
                    let bytes = checked(
                        serde_json::to_vec(&projected),
                        "position serialization failed",
                    );
                    projected_bytes = projected_bytes
                        .checked_add(bytes.len() as u64)
                        .expect("byte count overflow");
                    digest.update((bytes.len() as u64).to_be_bytes());
                    digest.update(bytes);
                    rows += 1;
                }
                match page.next_after_id {
                    Some(next) => after_id = next,
                    None => break,
                }
            }
            assert!(rows > 0, "bounded component requires position rows");
            let metadata = json!({"position_rows": rows, "projected_bytes": projected_bytes,
                "serialized_digest": Sha256Digest::from_bytes(digest.finalize().into())});
            deterministic.push(metadata.clone());
            runs.push(
                json!({"run": run, "decode_serialize_ms": started.elapsed().as_secs_f64() * 1000.0,
                "metadata": metadata}),
            );
        }
        eprintln!("{}", runs.last().expect("run timing"));
    }
    assert!(
        deterministic[0] == deterministic[1],
        "deterministic metadata differs"
    );
    let started = Instant::now();
    assert!(
        checked(
            stage.sealed_content_digest(),
            "final sealed input verification failed"
        ) == stage_digest,
        "sealed input changed during measurement"
    );
    let final_verification_ms = started.elapsed().as_secs_f64() * 1000.0;
    let receipt = json!({"event": "retained_physical_measurement", "mode": if full {"full"} else {"rows"},
        "debug_assertions": cfg!(debug_assertions), "stage_path": input,
        "stage_digest": stage_digest, "source_rows": stats.row_count, "payload_bytes": stats.payload_bytes,
        "open_ms": open_ms, "digest_ms": digest_ms, "final_verification_ms": final_verification_ms,
        "max_rows_per_chunk": fragment_limits.max_rows_per_chunk,
        "max_projected_json_bytes": fragment_limits.max_projected_json_bytes,
        "runs": runs, "identical_metadata": true, "catalogue_published": false});
    let receipt_fd = checked(
        rustix::fs::openat(
            &output_fd,
            "measurement-receipt.json",
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        ),
        "measurement receipt already exists or cannot be created",
    );
    let receipt_file = File::from(receipt_fd);
    checked(
        serde_json::to_writer_pretty(&receipt_file, &receipt),
        "measurement receipt write failed",
    );
    checked(receipt_file.sync_all(), "measurement receipt sync failed");
    eprintln!("{receipt}");
    drop(stage); // Never consume/discard the retained source.
}

#[derive(Default)]
struct BenchmarkCountingWriter {
    bytes: u64,
}

impl std::io::Write for BenchmarkCountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = u64::try_from(bytes.len())
            .map_err(|_| std::io::Error::other("JSON byte count overflow"))?;
        self.bytes = self
            .bytes
            .checked_add(count)
            .ok_or_else(|| std::io::Error::other("JSON byte count overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn benchmark_counted_json_bytes<T: serde::Serialize + ?Sized>(
    value: &T,
) -> Result<u64, serde_json::Error> {
    let mut writer = BenchmarkCountingWriter::default();
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}

struct BenchmarkDigestingWriter<'a> {
    count: BenchmarkCountingWriter,
    digest: &'a mut Sha256,
}

impl std::io::Write for BenchmarkDigestingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = std::io::Write::write(&mut self.count, bytes)?;
        self.digest.update(bytes);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn checked_json_sizing_matches_vec_lengths_digests_and_errors() {
    use std::io::Write;
    let values = [
        json!(null),
        json!(false),
        json!("quoted\" slash\\ newline\n café 🚗"),
        json!([0, -1, 9_007_199_254_740_991_i64, 0.125, 1.0e-20]),
        json!({"empty": [], "nested": {"key": "value"}}),
    ];
    for value in values {
        let reference = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            benchmark_counted_json_bytes(&value).unwrap(),
            reference.len() as u64
        );
        let mut digest = Sha256::new();
        let mut writer = BenchmarkDigestingWriter {
            count: BenchmarkCountingWriter::default(),
            digest: &mut digest,
        };
        serde_json::to_writer(&mut writer, &value).unwrap();
        assert_eq!(writer.count.bytes, reference.len() as u64);
        assert!(
            Sha256Digest::from_bytes(digest.finalize().into())
                == Sha256Digest::from_bytes(Sha256::digest(&reference).into())
        );
    }
    struct Reject;
    impl serde::Serialize for Reject {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom(
                "intentional serialization failure",
            ))
        }
    }
    let reference = serde_json::to_vec(&Reject).unwrap_err();
    let counted = benchmark_counted_json_bytes(&Reject).unwrap_err();
    assert!(reference.is_data() && counted.is_data());
    assert_eq!(reference.to_string(), counted.to_string());
    let mut overflowing = BenchmarkCountingWriter { bytes: u64::MAX };
    let error = overflowing.write_all(b"x").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert_eq!(overflowing.bytes, u64::MAX);
}

/// Decode each bounded page once, then compare sizing alone with identical rows.
/// Full sealed-input validation and exact byte/digest parity are outside the timings.
#[test]
#[cfg(unix)]
#[ignore = "explicit same retained input JSON sizing comparison"]
fn measure_retained_physical_json_sizing() {
    use crate::teslamate_projection::{TeslaMateCarPhysicalV2_2, TeslaMatePositionPhysicalV2_2};
    use std::{
        fs::File,
        path::PathBuf,
        time::{Duration, Instant},
    };
    fn checked<T, E>(value: Result<T, E>, message: &'static str) -> T {
        value.unwrap_or_else(|_| panic!("{message}"))
    }
    let input = PathBuf::from(
        std::env::var_os("TESLATLAS_HUB_BENCH_STAGE")
            .expect("retained private stage environment is required"),
    );
    let output = PathBuf::from(
        std::env::var_os("TESLATLAS_HUB_BENCH_OUTPUT_DIRECTORY")
            .expect("private benchmark output directory is required"),
    );
    assert!(
        input.is_absolute() && output.is_absolute(),
        "benchmark paths must be absolute"
    );
    let output_fd = checked(
        crate::runtime::development_event_log::validated_directory(&output),
        "benchmark output must be an existing private directory",
    );
    let row_limit: u64 = std::env::var("TESLATLAS_HUB_BENCH_POSITION_ROWS")
        .map_or(50_000, |value| value.parse().expect("benchmark row limit"));
    assert!(
        (1..=1_000_000).contains(&row_limit),
        "component row limit must be 1 through 1000000"
    );
    let started = Instant::now();
    let stage = checked(
        TeslaMateStage::open_sealed(&input),
        "sealed input admission failed",
    );
    let open_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(
        checked(stage.format(), "stage format") == TeslaMateStageFormat::PhysicalV3,
        "benchmark requires a physical-v3 stage"
    );
    let stats = checked(stage.stats(), "sealed input statistics");
    let car = checked(
        stage.page::<TeslaMateCarPhysicalV2_2>(TeslaMateStageTable::Cars, 0, 2),
        "car root read failed",
    );
    assert!(
        car.rows.len() == 1 && car.next_after_id.is_none(),
        "exactly one car root required"
    );
    let selected_car_id = car.rows[0].value.id;
    let started = Instant::now();
    let stage_digest = checked(
        stage.sealed_content_digest(),
        "sealed content verification failed",
    );
    let digest_ms = started.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "{}",
        json!({"event": "json_sizing_input_admitted", "open_ms": open_ms,
        "digest_ms": digest_ms, "source_rows": stats.row_count})
    );
    let mut vec_times = [Duration::ZERO; 3];
    let mut counted_times = [Duration::ZERO; 3];
    let mut decode_time = Duration::ZERO;
    let mut parity_time = Duration::ZERO;
    let mut reference_digest = Sha256::new();
    let mut counted_digest = Sha256::new();
    let mut projected_bytes = 0_u64;
    let mut rows = 0_u64;
    let mut after_id = 0;
    let mut page_index = 0_usize;
    while rows < row_limit {
        let started = Instant::now();
        let page_size = u32::try_from((row_limit - rows).min(10_000)).expect("bounded page size");
        let page = checked(
            stage.page::<TeslaMatePositionPhysicalV2_2>(
                TeslaMateStageTable::Positions,
                after_id,
                page_size,
            ),
            "position page failed",
        );
        let mut projected = Vec::with_capacity(page.rows.len());
        for row in page.rows {
            assert!(
                row.source_id == i64::from(row.value.id) && row.value.car_id == selected_car_id,
                "position identity mismatch"
            );
            projected.push(crate::hub_pack::ProjectionPositionV2_2::from(row.value));
        }
        decode_time += started.elapsed();
        if projected.is_empty() {
            break;
        }
        let started = Instant::now();
        let mut page_bytes = 0_u64;
        for row in &projected {
            let reference = checked(serde_json::to_vec(row), "reference serialization failed");
            let count = checked(
                benchmark_counted_json_bytes(row),
                "counting serialization failed",
            );
            assert!(
                count == reference.len() as u64,
                "exact JSON byte length differs"
            );
            reference_digest.update(count.to_be_bytes());
            reference_digest.update(&reference);
            counted_digest.update(count.to_be_bytes());
            let mut digesting = BenchmarkDigestingWriter {
                count: BenchmarkCountingWriter::default(),
                digest: &mut counted_digest,
            };
            checked(
                serde_json::to_writer(&mut digesting, row),
                "digesting serialization failed",
            );
            assert!(
                digesting.count.bytes == count,
                "digesting byte length differs"
            );
            page_bytes = page_bytes
                .checked_add(count)
                .expect("page byte count overflow");
        }
        projected_bytes = projected_bytes
            .checked_add(page_bytes)
            .expect("total byte count overflow");
        parity_time += started.elapsed();
        for repeat in 0..3 {
            // Alternate order per page/repeat; both paths consume the same decoded page.
            for turn in 0..2 {
                let reference = (repeat + page_index + turn) % 2 == 0;
                let started = Instant::now();
                let mut bytes = 0_u64;
                for row in &projected {
                    let row = std::hint::black_box(row);
                    let count = if reference {
                        checked(
                            serde_json::to_vec(row),
                            "timed reference serialization failed",
                        )
                        .len() as u64
                    } else {
                        checked(
                            benchmark_counted_json_bytes(row),
                            "timed counting serialization failed",
                        )
                    };
                    bytes = bytes
                        .checked_add(std::hint::black_box(count))
                        .expect("timed byte count overflow");
                }
                let elapsed = started.elapsed();
                assert!(
                    std::hint::black_box(bytes) == page_bytes,
                    "timed sizing result differs"
                );
                if reference {
                    vec_times[repeat] += elapsed;
                } else {
                    counted_times[repeat] += elapsed;
                }
            }
        }
        rows += projected.len() as u64;
        page_index += 1;
        match page.next_after_id {
            Some(next) => after_id = next,
            None => break,
        }
    }
    assert!(rows > 0, "sizing comparison requires position rows");
    let reference_digest = Sha256Digest::from_bytes(reference_digest.finalize().into());
    let counted_digest = Sha256Digest::from_bytes(counted_digest.finalize().into());
    assert!(
        reference_digest == counted_digest,
        "serialized JSON digests differ"
    );
    let vec_ms = vec_times.map(|time| time.as_secs_f64() * 1000.0);
    let counted_ms = counted_times.map(|time| time.as_secs_f64() * 1000.0);
    let mut sorted_vec = vec_ms;
    let mut sorted_counted = counted_ms;
    sorted_vec.sort_by(f64::total_cmp);
    sorted_counted.sort_by(f64::total_cmp);
    eprintln!(
        "{}",
        json!({"event": "json_sizing_timings", "position_rows": rows,
        "vec_ms": vec_ms, "counted_ms": counted_ms, "vec_median_ms": sorted_vec[1],
        "counted_median_ms": sorted_counted[1], "ratio": sorted_vec[1] / sorted_counted[1]})
    );
    let started = Instant::now();
    assert!(
        checked(
            stage.sealed_content_digest(),
            "final sealed input verification failed"
        ) == stage_digest,
        "sealed input changed during sizing measurement"
    );
    let receipt = json!({"event": "retained_physical_json_sizing", "debug_assertions": cfg!(debug_assertions),
        "stage_path": input, "stage_digest": stage_digest, "source_rows": stats.row_count,
        "position_rows": rows, "projected_bytes": projected_bytes, "serialized_digest": reference_digest,
        "exact_lengths_and_digests": true, "open_ms": open_ms, "digest_ms": digest_ms,
        "decode_ms": decode_time.as_secs_f64() * 1000.0, "parity_ms": parity_time.as_secs_f64() * 1000.0,
        "final_verification_ms": started.elapsed().as_secs_f64() * 1000.0,
        "vec_ms": vec_ms, "counted_ms": counted_ms, "vec_median_ms": sorted_vec[1],
        "counted_median_ms": sorted_counted[1], "ratio": sorted_vec[1] / sorted_counted[1],
        "catalogue_published": false});
    let fd = checked(
        rustix::fs::openat(
            &output_fd,
            "json-sizing-receipt.json",
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        ),
        "sizing receipt already exists or cannot be created",
    );
    let receipt_file = File::from(fd);
    checked(
        serde_json::to_writer_pretty(&receipt_file, &receipt),
        "sizing receipt write failed",
    );
    checked(receipt_file.sync_all(), "sizing receipt sync failed");
    eprintln!("{receipt}");
    drop(stage);
}

fn physical_stage(root: &std::path::Path, name: &str, updates: &[i32]) -> TeslaMateStage {
    let mut stage = TeslaMateStage::create_physical_v3(root.join(name), stage_limits())
        .expect("physical stage");
    seed_roots(&mut stage);
    seed_updates(&mut stage, updates);
    stage.seal().expect("seal physical stage");
    stage
}

async fn blocked_successor(
    store: &HubStore,
    key: &CursorKey,
    binding: &ProjectionBinding,
    stage: &TeslaMateStage,
    now_ms: i64,
) -> PendingPhysicalV3Admission {
    let snapshot_id = physical_v3_snapshot_id(stage.sealed_content_digest().unwrap(), binding);
    let gate = store.acquire_publication_gate().await.unwrap();
    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        stage,
        &ProjectionPackWriter::with_limits(
            store.packs_dir(),
            ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
        ),
        binding.clone(),
        snapshot_id,
        SequenceRange {
            from_exclusive: 2,
            to_inclusive: 2,
        },
        key,
        TeslaMatePhysicalFragmentLimits::default(),
    )
    .unwrap();
    store
        .rotate_pending_physical_v3_admission_at(&gate, candidate, now_ms)
        .unwrap()
}

#[tokio::test]
async fn blocked_rotation_recovers_before_publishing_a_newer_source_snapshot() {
    let temp = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temp.path().join("hub")).unwrap();
    let binding = registered_admission_binding(&store);
    let key = CursorKey::from_bytes([0x53; 32]);
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &key,
        binding.clone(),
        physical_stage(temp.path(), "first", &[10]),
        1_000,
    )
    .await
    .unwrap();
    let interrupted = physical_stage(temp.path(), "interrupted", &[10, 11]);
    let blocked = blocked_successor(&store, &key, &binding, &interrupted, 2_000).await;
    interrupted.discard().unwrap();
    drop(store);

    let store = HubStore::initialize(temp.path().join("hub")).unwrap();
    let latest = publish_sealed_physical_v3_stage_at(
        &store,
        &key,
        binding.clone(),
        physical_stage(temp.path(), "latest", &[10, 11, 12]),
        3_000,
    )
    .await
    .unwrap();
    assert_eq!(latest.kind, PhysicalV3PublicationKind::Rotation);
    assert_eq!(latest.admission.head_sequence, 3);
    assert_ne!(latest.admission.snapshot_id, blocked.snapshot_id);
    for prior in [&first.admission, &blocked] {
        assert_eq!(
            store
                .retained_physical_v3_admission_for_receipt_at(
                    binding.vehicle_id,
                    &prior.receipt_id,
                    3_000,
                    true
                )
                .unwrap()
                .unwrap()
                .admission,
            *prior
        );
    }
    assert_eq!(
        store
            .pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id)
            .unwrap()
            .unwrap(),
        latest.admission
    );
}

#[tokio::test]
async fn blocked_rotation_recovery_rejects_corrupt_or_expired_persisted_artifacts() {
    for target in [Some(true), Some(false), None] {
        let temp = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(temp.path().join("hub")).unwrap();
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x54; 32]);
        let first = publish_sealed_physical_v3_stage_at(
            &store,
            &key,
            binding.clone(),
            physical_stage(temp.path(), "first", &[10]),
            1_000,
        )
        .await
        .unwrap();
        let interrupted = physical_stage(temp.path(), "interrupted", &[10, 11]);
        let blocked = blocked_successor(&store, &key, &binding, &interrupted, 2_000).await;
        interrupted.discard().unwrap();
        if let Some(target) = target {
            let admission = if target { &blocked } else { &first.admission };
            let path = store.packs_dir().join("sha256").join(format!(
                "{}.sqlite.zst",
                admission.manifest.chunks.last().unwrap().sha256
            ));
            let mut bytes = std::fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            std::fs::write(&path, bytes).unwrap();
        }
        let now_ms = if target.is_none() {
            2_000 + crate::db::RETIRED_LINEAGE_PACK_RETENTION_MS
        } else {
            3_000
        };
        assert!(
            publish_sealed_physical_v3_stage_at(
                &store,
                &key,
                binding.clone(),
                physical_stage(temp.path(), "latest", &[10, 11, 12]),
                now_ms
            )
            .await
            .is_err()
        );
        let (state, sequence): (String, i64) = store.open().unwrap().query_row("SELECT serve_state, head_sequence FROM pending_physical_v3_admissions WHERE vehicle_id=?1", [binding.vehicle_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(state, "blocked_rotation");
        assert_eq!(sequence, 2);
    }
}

#[tokio::test]
async fn production_rotation_commits_public_head_and_retained_prior_atomically() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};
    for point in [
        DurabilityFaultPoint::CatalogueBeforeCommit,
        DurabilityFaultPoint::CatalogueAfterCommit,
    ] {
        let temp = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(temp.path().join("hub")).unwrap();
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x56; 32]);
        let first = publish_sealed_physical_v3_stage_at(
            &store,
            &key,
            binding.clone(),
            physical_stage(temp.path(), "first", &[10]),
            1_000,
        )
        .await
        .unwrap();
        let stage = physical_stage(temp.path(), "next", &[10, 11]);
        let snapshot_id = physical_v3_snapshot_id(stage.sealed_content_digest().unwrap(), &binding);
        let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
            &stage,
            &ProjectionPackWriter::with_limits(
                store.packs_dir(),
                ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
            ),
            binding.clone(),
            snapshot_id,
            SequenceRange {
                from_exclusive: 2,
                to_inclusive: 2,
            },
            &key,
            TeslaMatePhysicalFragmentLimits::default(),
        )
        .unwrap();
        let target = crate::db::physical_v3_admission_from_manifest(
            &candidate.manifest,
            binding.selected_car_id,
        )
        .unwrap();
        let delta = crate::import::teslamate::physical_delta_pack::prepare_changed_set(
            &first.admission,
            &target,
            &binding,
            &key,
            stage.sealed_content_digest().unwrap(),
            store.packs_dir(),
            0,
        )
        .unwrap();
        let gate = store.acquire_publication_gate().await.unwrap();
        let result = {
            let _fault = inject(point);
            store.rotate_and_activate_physical_v3_admission_with_delta_at(
                &gate,
                candidate,
                Some(delta),
                2_000,
            )
        };
        stage.discard().unwrap();
        drop(gate);
        drop(store);
        let store = HubStore::initialize(temp.path().join("hub")).unwrap();
        let stored_delta = store
            .physical_v3_delta_receipt_for_base_at(
                binding.vehicle_id,
                &first.admission.receipt_id,
                &target.receipt_id,
                2_001,
            )
            .unwrap();
        let expected = match point {
            DurabilityFaultPoint::CatalogueBeforeCommit => {
                assert!(matches!(
                    result,
                    Err(crate::db::StoreError::CatalogueDurability(_))
                ));
                assert!(stored_delta.is_none());
                assert!(
                    store
                        .retained_physical_v3_admission_for_receipt_at(
                            binding.vehicle_id,
                            &first.admission.receipt_id,
                            2_001,
                            true
                        )
                        .unwrap()
                        .is_none()
                );
                first.admission
            }
            _ => {
                let committed = result.unwrap();
                assert_eq!(committed.head_sequence, 2);
                assert!(stored_delta.is_some());
                assert_eq!(
                    store
                        .retained_physical_v3_admission_for_receipt_at(
                            binding.vehicle_id,
                            &first.admission.receipt_id,
                            2_001,
                            true
                        )
                        .unwrap()
                        .unwrap()
                        .admission,
                    first.admission
                );
                committed
            }
        };
        assert_eq!(
            store
                .pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id)
                .unwrap()
                .unwrap(),
            expected
        );
        assert!(
            matches!(store.physical_v3_publication_state_for_vehicle_at(binding.vehicle_id, 2_000 + crate::db::RETIRED_LINEAGE_PACK_RETENTION_MS).unwrap(), PhysicalV3PublicationState::Public(current) if current == expected)
        );
    }
}

#[tokio::test]
async fn unchanged_publication_reuses_exact_reconstruction_without_fragment_capacity() {
    let temp = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temp.path().join("hub")).unwrap();
    let binding = registered_admission_binding(&store);
    let key = CursorKey::from_bytes([0x57; 32]);
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &key,
        binding.clone(),
        physical_stage(temp.path(), "first", &[10]),
        1_000,
    )
    .await
    .unwrap();
    let stage = physical_stage(temp.path(), "same", &[10]);
    let reconstructed = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::with_limits(
            store.packs_dir(),
            ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
        ),
        binding.clone(),
        first.admission.snapshot_id,
        SequenceRange {
            from_exclusive: 1,
            to_inclusive: 1,
        },
        &key,
        TeslaMatePhysicalFragmentLimits::default(),
    )
    .unwrap();
    assert_eq!(reconstructed.manifest, first.admission.manifest);
    assert_eq!(reconstructed.binding, binding);
    drop(reconstructed);
    let limits = TeslaMatePhysicalFragmentLimits {
        max_rows_per_chunk: 3,
        ..Default::default()
    };
    assert!(
        write_staged_physical_updates_snapshot_v3_with_limits(
            &stage,
            &ProjectionPackWriter::with_limits(
                store.packs_dir(),
                ProtocolLimits::hub_sync_v1_1_3_schema_2_2()
            ),
            binding.clone(),
            first.admission.snapshot_id,
            SequenceRange {
                from_exclusive: 1,
                to_inclusive: 1
            },
            &key,
            limits,
        )
        .is_err(),
        "full reconstruction exceeds the restricted fragment capacity"
    );
    let gate = store.acquire_publication_gate().await.unwrap();
    let result = publish_sealed_physical_v3_stage_inner(
        &store,
        &key,
        binding.clone(),
        &stage,
        &gate,
        1_001,
        limits,
    )
    .unwrap();
    assert_eq!(result.kind, PhysicalV3PublicationKind::Unchanged);
    assert_eq!(result.admission, first.admission);
    stage.discard().unwrap();
    drop(gate);

    let path = store.packs_dir().join("sha256").join(format!(
        "{}.sqlite.zst",
        first.admission.manifest.chunks[0].sha256
    ));
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(
        publish_sealed_physical_v3_stage_at(
            &store,
            &key,
            binding,
            physical_stage(temp.path(), "corrupt-retry", &[10]),
            1_002
        )
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "explicit synthetic publication measurement"]
async fn measure_unchanged_physical_publication() {
    let temp = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temp.path().join("hub")).unwrap();
    let binding = registered_admission_binding(&store);
    let key = CursorKey::from_bytes([0x55; 32]);
    let rows: Vec<_> = (1..=5_000).collect();
    let make_stage = |name: &str| {
        let mut stage = TeslaMateStage::create_physical_v3(
            temp.path().join(name),
            TeslaMateStageLimits {
                max_rows: 5_100,
                max_stage_bytes: 16 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .unwrap();
        seed_roots(&mut stage);
        seed_updates(&mut stage, &rows);
        stage.seal().unwrap();
        stage
    };
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &key,
        binding.clone(),
        make_stage("first"),
        1_000,
    )
    .await
    .unwrap();
    for run in 0..3 {
        let stage = make_stage(&format!("repeat-{run}"));
        let started = std::time::Instant::now();
        let result =
            publish_sealed_physical_v3_stage_at(&store, &key, binding.clone(), stage, 1_001)
                .await
                .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(result.admission, first.admission);
        let verified = std::time::Instant::now();
        assert!(
            matches!(store.physical_v3_publication_state_for_vehicle_at(binding.vehicle_id, 1_001).unwrap(), PhysicalV3PublicationState::Public(value) if value == first.admission)
        );
        eprintln!(
            "synthetic rows={} chunks={} unchanged_ms={:.3} verified_existing_ms={:.3}",
            first.admission.manifest.total_rows,
            first.admission.chunk_count,
            elapsed.as_secs_f64() * 1_000.0,
            verified.elapsed().as_secs_f64() * 1_000.0
        );
    }

    // Pair both paths on the same sealed stage, gate, store, binding and
    // signed receipt. Stage creation, optional prepared maps and cleanup are
    // outside these timings; the reconstruction reference matches the former
    // unchanged inner path, including integrity/digest and persisted checks.
    let stage = make_stage("paired-reference");
    let gate = store.acquire_publication_gate().await.unwrap();
    let expected_bytes = first
        .admission
        .manifest
        .chunks
        .iter()
        .map(|chunk| {
            std::fs::read(
                store
                    .packs_dir()
                    .join("sha256")
                    .join(format!("{}.sqlite.zst", chunk.sha256)),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut reconstruction_ms = Vec::new();
    let mut reuse_ms = Vec::new();
    for run in 0..3 {
        let started = std::time::Instant::now();
        let digest = stage.sealed_content_digest().unwrap();
        let snapshot_id = physical_v3_snapshot_id(digest, &binding);
        let current = store
            .physical_v3_publication_state_for_vehicle_at(binding.vehicle_id, 1_001)
            .unwrap();
        assert!(
            matches!(current, PhysicalV3PublicationState::Public(value) if value == first.admission)
        );
        let limits = stage.stats().unwrap().limits;
        let writer = ProjectionPackWriter::with_limits(
            store.packs_dir(),
            ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
        )
        .with_minimum_free_bytes(limits.minimum_free_bytes);
        writer
            .ensure_full_snapshot_capacity_for_capture(
                limits.max_stage_bytes,
                limits.minimum_free_bytes,
            )
            .unwrap();
        let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
            &stage,
            &writer,
            binding.clone(),
            snapshot_id,
            SequenceRange {
                from_exclusive: first.admission.head_sequence,
                to_inclusive: first.admission.head_sequence,
            },
            &key,
            TeslaMatePhysicalFragmentLimits::default(),
        )
        .unwrap();
        assert_eq!(candidate.manifest, first.admission.manifest);
        assert_eq!(candidate.binding, binding);
        reconstruction_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(
            candidate
                .chunks
                .iter()
                .map(|chunk| &chunk.metadata)
                .collect::<Vec<_>>(),
            first.admission.manifest.chunks.iter().collect::<Vec<_>>()
        );
        assert_eq!(
            candidate
                .chunks
                .iter()
                .map(|chunk| std::fs::read(&chunk.path).unwrap())
                .collect::<Vec<_>>(),
            expected_bytes
        );
        drop(candidate);
        let started = std::time::Instant::now();
        let reused = publish_sealed_physical_v3_stage_inner(
            &store,
            &key,
            binding.clone(),
            &stage,
            &gate,
            1_001,
            TeslaMatePhysicalFragmentLimits::default(),
        )
        .unwrap();
        reuse_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(reused.kind, PhysicalV3PublicationKind::Unchanged);
        assert_eq!(reused.admission, first.admission);
        assert_eq!(
            reused
                .admission
                .manifest
                .chunks
                .iter()
                .map(|chunk| {
                    std::fs::read(
                        store
                            .packs_dir()
                            .join("sha256")
                            .join(format!("{}.sqlite.zst", chunk.sha256)),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>(),
            expected_bytes,
        );
        eprintln!(
            "paired run={run} reconstruction_ms={:.3} reuse_ms={:.3}",
            reconstruction_ms[run], reuse_ms[run]
        );
    }
    reconstruction_ms.sort_by(f64::total_cmp);
    reuse_ms.sort_by(f64::total_cmp);
    eprintln!(
        "paired medians reconstruction_ms={:.3} reuse_ms={:.3} ratio={:.3}",
        reconstruction_ms[1],
        reuse_ms[1],
        reconstruction_ms[1] / reuse_ms[1]
    );
    stage.discard().unwrap();
}

#[tokio::test]
async fn production_caller_publishes_noops_and_rotates_under_one_gate() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let binding = registered_admission_binding(&store);
    let cursor_key = CursorKey::from_bytes([0x51; 32]);

    let first_stage = physical_stage(temporary.path(), "first", &[10]);
    let first_path = first_stage.path().to_path_buf();
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        first_stage,
        1_000,
    )
    .await
    .expect("first public head");
    assert_eq!(first.kind, PhysicalV3PublicationKind::FirstHead);
    assert_eq!(first.admission.head_sequence, 1);
    assert!(!first_path.exists());

    let unchanged_stage = physical_stage(temporary.path(), "unchanged", &[10]);
    let unchanged_path = unchanged_stage.path().to_path_buf();
    let unchanged = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        unchanged_stage,
        1_001,
    )
    .await
    .expect("unchanged public head");
    assert_eq!(unchanged.kind, PhysicalV3PublicationKind::Unchanged);
    assert_eq!(unchanged.admission, first.admission);
    assert!(!unchanged_path.exists());

    let changed_stage = physical_stage(temporary.path(), "changed", &[10, 11]);
    let changed_path = changed_stage.path().to_path_buf();
    let changed = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        changed_stage,
        2_000,
    )
    .await
    .expect("rotated public head");
    assert_eq!(changed.kind, PhysicalV3PublicationKind::Rotation);
    assert_eq!(changed.admission.head_sequence, 2);
    assert_ne!(changed.admission.snapshot_id, first.admission.snapshot_id);
    let (_, _, delta_json) = store
        .physical_v3_delta_receipt_for_base_at(
            binding.vehicle_id,
            &first.admission.receipt_id,
            &changed.admission.receipt_id,
            2_001,
        )
        .expect("delta catalogue")
        .expect("immediate changed-set receipt");
    let delta: serde_json::Value = serde_json::from_slice(&delta_json).expect("signed delta");
    assert_eq!(delta["kind"], "physical_changed_set");
    assert!(delta["total_rows"].as_u64().expect("typed rows") >= 2);
    assert_eq!(delta["target"]["sequence"], 2);
    let signing = crate::manifest_signing::ManifestSigning::from_cursor_key(&cursor_key);
    assert_eq!(
        delta["base"]["manifest_sha256"],
        Sha256Digest::of_bytes(
            &signing
                .signed_physical_manifest_document(&first.admission)
                .expect("base manifest")
        )
        .to_string()
    );
    let pack_digest: Sha256Digest = delta["chunks"][0]["pack"]["sha256"]
        .as_str()
        .expect("pack digest")
        .parse()
        .expect("valid digest");
    let stored = store
        .physical_v3_delta_pack_for_digest_at(pack_digest, 2_001)
        .expect("delta pack catalogue")
        .expect("published delta pack");
    if let Some(output) = std::env::var_os("TESLATLAS_HUB_SYNTHETIC_DELTA_FIXTURE_DIR") {
        let output = std::path::PathBuf::from(output);
        std::fs::create_dir_all(&output).expect("synthetic fixture output directory");
        std::fs::write(output.join("receipt.json"), &delta_json).expect("synthetic signed receipt");
        std::fs::copy(&stored.path, output.join("pack.sqlite.zst"))
            .expect("synthetic immutable delta pack");
    }
    let decoded = zstd::stream::decode_all(std::fs::File::open(&stored.path).expect("pack file"))
        .expect("decode delta pack");
    let sqlite = temporary.path().join("physical-changed-set.sqlite");
    std::fs::write(&sqlite, &decoded).expect("private synthetic pack");
    let connection =
        rusqlite::Connection::open_with_flags(&sqlite, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("delta SQLite");
    let expected_schema = rusqlite::Connection::open_in_memory().expect("Protocol schema memory");
    expected_schema
        .execute_batch(include_str!("../physical_delta_pack_v1.sql"))
        .expect("frozen Protocol SQL");
    let tables = |db: &rusqlite::Connection| -> Vec<(String, String)> {
        db.prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' ORDER BY name")
            .expect("schema query")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("schema rows")
            .collect::<Result<_, _>>()
            .expect("schema values")
    };
    assert_eq!(tables(&connection), tables(&expected_schema));
    let changed_update: (i64, String) = connection
        .query_row(
            "SELECT entity_id,role FROM row_roles WHERE table_name='updates' AND entity_id=11",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("changed update");
    assert_eq!(changed_update, (11, "changed".to_owned()));
    let third = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        physical_stage(temporary.path(), "third-reverts-to-first", &[10]),
        3_000,
    )
    .await
    .expect("third public head");
    assert_eq!(third.kind, PhysicalV3PublicationKind::Rotation);
    assert_eq!(third.admission.head_sequence, 3);
    assert_eq!(third.admission.snapshot_id, first.admission.snapshot_id);
    assert_ne!(third.admission.receipt_id, first.admission.receipt_id);
    let fourth = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        physical_stage(temporary.path(), "fourth", &[10, 11, 12]),
        4_000,
    )
    .await
    .expect("fourth public head");
    assert_eq!(fourth.kind, PhysicalV3PublicationKind::Rotation);
    assert_eq!(fourth.admission.head_sequence, 4);
    for prior in [&first.admission, &changed.admission, &third.admission] {
        assert_eq!(
            store
                .retained_physical_v3_admission_for_receipt_at(
                    binding.vehicle_id,
                    &prior.receipt_id,
                    4_001,
                    true,
                )
                .expect("retained lookup")
                .expect("unexpired prior")
                .admission,
            *prior
        );
    }
    assert_eq!(
        store
            .pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id)
            .expect("public control head")
            .expect("rotated head"),
        fourth.admission
    );
    assert!(!changed_path.exists());
}

#[tokio::test]
async fn physical_delta_pack_is_backed_up_and_expires_with_its_retained_base() {
    let temporary = crate::private_tempdir().expect("private store root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let binding = registered_admission_binding(&store);
    let key = CursorKey::from_bytes([0x52; 32]);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock")
            .as_millis(),
    )
    .expect("bounded clock");
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &key,
        binding.clone(),
        physical_stage(temporary.path(), "backup-first", &[10]),
        now,
    )
    .await
    .expect("first head");
    let second = publish_sealed_physical_v3_stage_at(
        &store,
        &key,
        binding.clone(),
        physical_stage(temporary.path(), "backup-second", &[10, 11]),
        now + 1,
    )
    .await
    .expect("changed head");
    let (_, _, receipt) = store
        .physical_v3_delta_receipt_for_base_at(
            binding.vehicle_id,
            &first.admission.receipt_id,
            &second.admission.receipt_id,
            now + 2,
        )
        .expect("delta lookup")
        .expect("active changed-set receipt");
    let receipt: serde_json::Value = serde_json::from_slice(&receipt).expect("signed receipt");
    let digest: Sha256Digest = receipt["chunks"][0]["pack"]["sha256"]
        .as_str()
        .expect("pack digest")
        .parse()
        .expect("valid digest");
    let original_pack = store
        .physical_v3_delta_pack_for_digest_at(digest, now + 2)
        .expect("pack lookup")
        .expect("current pack");
    let backup = temporary.path().join("backup");
    store
        .backup_to(&backup)
        .expect("referenced delta pack backup");
    let restored = HubStore::initialize(&backup).expect("restore complete backup");
    restored.quick_check().expect("restored integrity");
    let restored_pack = restored
        .physical_v3_delta_pack_for_digest_at(digest, now + 2)
        .expect("restored pack lookup")
        .expect("restored current pack");
    assert_eq!(
        Sha256Digest::of_bytes(&std::fs::read(&original_pack.path).expect("original bytes")),
        Sha256Digest::of_bytes(&std::fs::read(&restored_pack.path).expect("backup bytes"))
    );

    // Expiration removes the base checkpoint, transition and pack reference
    // together; repair may then collect the now-orphaned immutable object.
    let expired = now - crate::db::RETIRED_LINEAGE_PACK_RETENTION_MS;
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE retained_physical_v3_admissions
             SET retained_at_ms=?1, expires_at_ms=?2 WHERE receipt_id=?3",
            rusqlite::params![expired - 1, expired, first.admission.receipt_id],
        )
        .expect("expire synthetic retained base");
    store.repair().expect("expired delta repair");
    assert!(
        store
            .physical_v3_delta_pack_for_digest_at(digest, now + 2)
            .expect("expired pack lookup")
            .is_none()
    );
    assert!(
        !original_pack.path.exists(),
        "expired delta object collected"
    );
    let count: i64 = store
        .open()
        .expect("catalogue after repair")
        .query_row(
            "SELECT COUNT(*) FROM physical_v3_delta_transitions",
            [],
            |row| row.get(0),
        )
        .expect("transition count");
    assert_eq!(count, 0);
}

#[test]
fn physical_delta_multiple_packs_are_all_referenced_and_backed_up() {
    use crate::import::teslamate::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence;

    let temporary = crate::private_tempdir().expect("private store root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let key = CursorKey::from_bytes([0x53; 32]);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock")
            .as_millis(),
    )
    .expect("bounded clock");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let (binding, first_candidate) = public_admission_candidate_fixture_for_source_and_sequence(
        &temporary.path().join("first-candidate"),
        &store,
        &key,
        1,
        "multi-pack",
        1,
    );
    let first = store
        .stage_pending_physical_v3_admission(&gate, first_candidate)
        .expect("first public head");
    let (same_binding, target_candidate) =
        public_admission_candidate_fixture_for_source_and_sequence(
            &temporary.path().join("target-candidate"),
            &store,
            &key,
            384,
            "multi-pack",
            2,
        );
    assert_eq!(same_binding, binding);
    let preview = crate::db::physical_v3_admission_from_manifest(
        &target_candidate.manifest,
        binding.selected_car_id,
    )
    .expect("target preview");
    let delta =
        crate::import::teslamate::physical_delta_pack::prepare_changed_set_with_chunk_target(
            &first,
            &preview,
            &binding,
            &key,
            Sha256Digest::of_bytes(b"synthetic target raw"),
            store.packs_dir(),
            0,
            4096,
        )
        .expect("bounded multi-pack change");
    assert!(delta.packs.len() > 1);
    let digests = delta
        .packs
        .iter()
        .map(|pack| pack.sha256)
        .collect::<Vec<_>>();
    let target = store
        .rotate_pending_physical_v3_admission_with_delta_at(
            &gate,
            target_candidate,
            Some(delta),
            now,
        )
        .expect("atomic multi-pack rotation");
    store
        .activate_pending_physical_v3_rotation_at(&gate, binding.vehicle_id, &first.receipt_id, now)
        .expect("target public");
    drop(gate);
    assert_eq!(target.head_sequence, 2);
    let backup = temporary.path().join("backup");
    store
        .backup_to(&backup)
        .expect("all referenced packs copied");
    let restored = HubStore::initialize(&backup).expect("restored multi-pack store");
    restored.quick_check().expect("restored integrity");
    for digest in digests {
        let source = store
            .physical_v3_delta_pack_for_digest_at(digest, now + 1)
            .expect("source pack lookup")
            .expect("source pack");
        let copied = restored
            .physical_v3_delta_pack_for_digest_at(digest, now + 1)
            .expect("restored pack lookup")
            .expect("restored pack");
        assert_eq!(
            Sha256Digest::of_bytes(&std::fs::read(source.path).expect("source bytes")),
            Sha256Digest::of_bytes(&std::fs::read(copied.path).expect("restored bytes"))
        );
    }
}

#[tokio::test]
async fn retry_finishes_the_same_rotation_after_its_catalogue_commit() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let binding = registered_admission_binding(&store);
    let cursor_key = CursorKey::from_bytes([0x52; 32]);
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        physical_stage(temporary.path(), "first", &[10]),
        1_000,
    )
    .await
    .expect("first public head");

    let retry_stage = physical_stage(temporary.path(), "retry", &[10, 11]);
    let snapshot_id = physical_v3_snapshot_id(
        retry_stage
            .sealed_content_digest()
            .expect("sealed stage digest"),
        &binding,
    );
    let gate = store
        .acquire_publication_gate()
        .await
        .expect("publication gate");
    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &retry_stage,
        &ProjectionPackWriter::with_limits(
            store.packs_dir(),
            ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
        ),
        binding.clone(),
        snapshot_id,
        SequenceRange {
            from_exclusive: 2,
            to_inclusive: 2,
        },
        &cursor_key,
        TeslaMatePhysicalFragmentLimits::default(),
    )
    .expect("rotation candidate");
    let blocked = store
        .rotate_pending_physical_v3_admission_at(&gate, candidate, 2_000)
        .expect("committed blocked rotation");
    assert_eq!(blocked.snapshot_id, snapshot_id);
    drop(gate);

    let resumed = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        retry_stage,
        2_001,
    )
    .await
    .expect("resume blocked rotation");
    assert_eq!(resumed.kind, PhysicalV3PublicationKind::ResumedRotation);
    assert_eq!(resumed.admission, blocked);
    assert_eq!(
        store
            .retained_physical_v3_admission_for_receipt_at(
                binding.vehicle_id,
                &first.admission.receipt_id,
                2_001,
                true,
            )
            .expect("retained receipt")
            .expect("retained prior")
            .admission,
        first.admission
    );

    let third_stage = physical_stage(temporary.path(), "third", &[10, 11, 12]);
    let third_snapshot_id = physical_v3_snapshot_id(
        third_stage
            .sealed_content_digest()
            .expect("third stage digest"),
        &binding,
    );
    let gate = store
        .acquire_publication_gate()
        .await
        .expect("third publication gate");
    let third_candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &third_stage,
        &ProjectionPackWriter::with_limits(
            store.packs_dir(),
            ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
        ),
        binding.clone(),
        third_snapshot_id,
        SequenceRange {
            from_exclusive: 3,
            to_inclusive: 3,
        },
        &cursor_key,
        TeslaMatePhysicalFragmentLimits::default(),
    )
    .expect("third candidate");
    let blocked_third = store
        .rotate_pending_physical_v3_admission_at(&gate, third_candidate, 3_000)
        .expect("third blocked head");
    assert!(matches!(
        store.activate_pending_physical_v3_rotation_at(
            &gate,
            binding.vehicle_id,
            &first.admission.receipt_id,
            3_001,
        ),
        Err(StoreError::PhysicalV3AdmissionConflict)
    ));
    drop(gate);
    drop(store);

    let reopened = HubStore::initialize(temporary.path().join("hub")).expect("restart Hub");
    let resumed_third = publish_sealed_physical_v3_stage_at(
        &reopened,
        &cursor_key,
        binding.clone(),
        third_stage,
        3_001,
    )
    .await
    .expect("resume third rotation after restart");
    assert_eq!(
        resumed_third.kind,
        PhysicalV3PublicationKind::ResumedRotation
    );
    assert_eq!(resumed_third.admission, blocked_third);
    for prior in [&first.admission, &resumed.admission] {
        assert!(
            reopened
                .retained_physical_v3_admission_for_receipt_at(
                    binding.vehicle_id,
                    &prior.receipt_id,
                    3_001,
                    true,
                )
                .expect("retained lookup after restart")
                .is_some()
        );
    }
}

#[tokio::test]
async fn caller_rejects_legacy_incomplete_and_foreign_stages_and_cleans_them() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let binding = registered_admission_binding(&store);
    let cursor_key = CursorKey::from_bytes([0x53; 32]);

    let mut legacy = TeslaMateStage::create(
        temporary.path().join("legacy"),
        TeslaMateStageLimits {
            max_rows: 8,
            max_stage_bytes: 512 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("legacy stage");
    legacy.seal().expect("seal legacy stage");
    let legacy_path = legacy.path().to_path_buf();
    assert!(matches!(
        publish_sealed_physical_v3_stage_at(&store, &cursor_key, binding.clone(), legacy, 1_000,)
            .await,
        Err(TeslaMatePhysicalPublicationError::WrongStageFormat)
    ));
    assert!(!legacy_path.exists());

    let mut incomplete =
        TeslaMateStage::create_physical_v3(temporary.path().join("incomplete"), stage_limits())
            .expect("incomplete stage");
    incomplete.seal().expect("seal incomplete stage");
    let incomplete_path = incomplete.path().to_path_buf();
    assert!(matches!(
        publish_sealed_physical_v3_stage_at(
            &store,
            &cursor_key,
            binding.clone(),
            incomplete,
            1_000,
        )
        .await,
        Err(TeslaMatePhysicalPublicationError::Fragment(
            TeslaMatePhysicalFragmentError::RootCardinality { .. }
        ))
    ));
    assert!(!incomplete_path.exists());

    let foreign = physical_stage(temporary.path(), "foreign", &[10]);
    let foreign_path = foreign.path().to_path_buf();
    let mut foreign_binding = binding;
    foreign_binding.installation_id = Uuid::from_u128(0xdeadbeef_dead_4eef_8ead_deadbeefdead);
    assert!(matches!(
        publish_sealed_physical_v3_stage_at(&store, &cursor_key, foreign_binding, foreign, 1_000,)
            .await,
        Err(TeslaMatePhysicalPublicationError::Store(
            StoreError::PhysicalV3AdmissionInvalid
        ))
    ));
    assert!(!foreign_path.exists());
    let pack_count = std::fs::read_dir(store.packs_dir().join("sha256"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(pack_count, 0);
}

#[test]
fn sealed_digest_covers_every_physical_table_and_is_order_independent() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let all = TeslaMateStageTable::PHYSICAL_V3_ALL;
    let make_stage = |name: &str, omitted: Option<TeslaMateStageTable>, reverse: bool| {
        let mut stage = TeslaMateStage::create_physical_v3(
            temporary.path().join(name),
            TeslaMateStageLimits {
                max_rows: 32,
                max_stage_bytes: 512 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("digest stage");
        let rows: Box<dyn Iterator<Item = TeslaMateStageTable>> = if reverse {
            Box::new(all.into_iter().rev())
        } else {
            Box::new(all.into_iter())
        };
        for table in rows {
            if Some(table) == omitted {
                continue;
            }
            let index = all
                .iter()
                .position(|candidate| *candidate == table)
                .expect("known physical table");
            stage
                .insert(
                    table,
                    i64::try_from(index + 1).expect("bounded source id"),
                    &json!({"table": table.as_str(), "value": index}),
                )
                .expect("digest row");
        }
        stage.seal().expect("seal digest stage");
        stage
    };

    let complete = make_stage("complete", None, false);
    let complete_digest = complete.sealed_content_digest().expect("complete digest");
    let reverse = make_stage("reverse", None, true);
    assert_eq!(
        complete_digest,
        reverse.sealed_content_digest().expect("reverse digest")
    );
    for (index, table) in all.into_iter().enumerate() {
        let omitted = make_stage(&format!("omitted-{index}"), Some(table), false);
        assert_ne!(
            complete_digest,
            omitted.sealed_content_digest().expect("omitted digest"),
            "{} must contribute to the digest",
            table.as_str()
        );
    }
}
