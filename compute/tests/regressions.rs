use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use teslatlas_compute::{
    ALGORITHM_VERSION, Diagnostics, Drive, Error, Progress, ProgressStage, RawMapPosition,
    TILE_GEOMETRY_VERSION, TileOutput, generate_tile_payload_v1,
    generate_tile_payload_v1_from_pages, generate_tile_payload_v1_from_pages_with_rdp_limit,
    generate_tile_payload_v1_with_rdp_limit, parse_position_page_v1,
    prepare_positions_for_tile_rendering_v1,
};

fn drive(drive_id: i32, points: &[(f32, f32)]) -> Drive {
    Drive {
        drive_id,
        points: points.to_vec(),
    }
}

fn generate(drives: &[Drive]) -> TileOutput {
    generate_tile_payload_v1(drives, &AtomicBool::new(false), None).unwrap()
}

fn ready_data_payload(output: &TileOutput, drive_count: i64, input_point_count: i64) -> &[u8] {
    match output {
        TileOutput::ReadyData {
            drive_count: actual_drive_count,
            tile_count,
            payload,
            diagnostics,
        } => {
            assert_eq!(*actual_drive_count, drive_count);
            assert!(*tile_count > 0);
            assert_eq!(
                *diagnostics,
                Diagnostics {
                    visitor_invocations: 1,
                    drive_count,
                    input_point_count,
                    produced_tile_count: *tile_count as i64,
                }
            );
            assert_eq!(u32::from_le_bytes(payload[..4].try_into().unwrap()), 1);
            assert_eq!(
                u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize,
                *tile_count
            );
            payload
        }
        TileOutput::ReadyEmpty { .. } => panic!("expected nonempty tile data"),
    }
}

fn position_page(drives: &[Drive]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&(drives.len() as u32).to_le_bytes());
    for drive in drives {
        bytes.extend_from_slice(&drive.drive_id.to_le_bytes());
        bytes.extend_from_slice(&(drive.points.len() as u32).to_le_bytes());
        for &(latitude, longitude) in &drive.points {
            bytes.extend_from_slice(&latitude.to_bits().to_le_bytes());
            bytes.extend_from_slice(&longitude.to_bits().to_le_bytes());
        }
    }
    bytes
}

fn invalid_data<T: Debug>(result: Result<T, Error>, context: &str) -> String {
    match result {
        Err(Error::InvalidData(message)) => message,
        other => panic!("{context}: expected InvalidData, got {other:?}"),
    }
}

#[test]
fn invalid_gaps_split_before_simplification_and_keep_original_input_counts() {
    let before = [(51.50, -0.14), (51.501, -0.139)];
    let after = [(51.503, -0.137), (51.504, -0.136)];
    let explicit_split = generate(&[drive(17, &before), drive(23, &after)]);
    let expected = ready_data_payload(&explicit_split, 2, 4);
    let continuous = generate(&[drive(17, &[before[0], before[1], after[0], after[1]])]);
    assert_ne!(ready_data_payload(&continuous, 1, 4), expected);

    for gap in [
        (f32::NAN, -0.138),
        (51.502, f32::NAN),
        (f32::INFINITY, -0.138),
        (51.502, 359.862),
        (91.0, -0.138),
        (0.0, 0.0),
    ] {
        let output = generate(&[drive(17, &[before[0], before[1], gap, after[0], after[1]])]);
        assert_eq!(
            ready_data_payload(&output, 1, 5),
            expected,
            "invalid coordinate {gap:?} must remain a break in the route"
        );
    }
}

#[test]
fn invalid_gaps_keep_original_counts_when_valid_runs_are_singletons() {
    assert_eq!(
        generate(&[drive(
            17,
            &[(51.50, -0.14), (f32::NAN, -0.138), (51.504, -0.136)],
        )]),
        TileOutput::ReadyEmpty {
            drive_count: 1,
            diagnostics: Diagnostics {
                visitor_invocations: 1,
                drive_count: 1,
                input_point_count: 3,
                produced_tile_count: 0,
            },
        }
    );
}

#[test]
fn exact_antimeridian_aliases_match_same_start_alias_in_both_directions() {
    for longitude in [-180.0_f32, 180.0] {
        for (start_latitude, end_latitude) in [(0.5, 0.51), (0.51, 0.5)] {
            let mixed = generate(&[drive(
                1,
                &[(start_latitude, longitude), (end_latitude, -longitude)],
            )]);
            let same_alias = generate(&[drive(
                1,
                &[(start_latitude, longitude), (end_latitude, longitude)],
            )]);
            assert_eq!(
                ready_data_payload(&mixed, 1, 2),
                ready_data_payload(&same_alias, 1, 2),
                "starting longitude {longitude}, latitude {start_latitude} to {end_latitude}"
            );
        }
    }
}

#[test]
fn projected_coordinates_outside_i16_fail_without_rejecting_legitimate_extended_offsets() {
    invalid_data(
        generate_tile_payload_v1(
            &[drive(1, &[(89.99, 0.0), (89.99, 10.0)])],
            &AtomicBool::new(false),
            None,
        ),
        "a short polar ground segment with unrepresentable projected offsets",
    );

    let allowed = generate(&[drive(1, &[(90.0, -0.01), (90.0, 0.01)])]);
    let bytes = ready_data_payload(&allowed, 1, 2);
    let mut offset = 8;
    let mut has_negative_offset = false;
    let mut has_offset_beyond_tile = false;
    while offset < bytes.len() {
        let length =
            u32::from_le_bytes(bytes[offset + 12..offset + 16].try_into().unwrap()) as usize;
        offset += 16;
        for coordinate in bytes[offset..offset + length].chunks_exact(2) {
            let coordinate = i16::from_le_bytes(coordinate.try_into().unwrap());
            has_negative_offset |= coordinate < 0;
            has_offset_beyond_tile |= coordinate > 512;
        }
        offset += length;
    }
    assert!(has_negative_offset);
    assert!(has_offset_beyond_tile);
}

#[test]
fn binary_position_pages_reject_unsupported_truncated_and_trailing_data() {
    let valid = position_page(&[drive(17, &[(51.50, -0.14), (51.51, -0.13)])]);
    for version in [0_u32, 2] {
        let mut unsupported = valid.clone();
        unsupported[..4].copy_from_slice(&version.to_le_bytes());
        let message = invalid_data(
            parse_position_page_v1(&unsupported, &AtomicBool::new(false), None),
            "unsupported page version",
        );
        assert!(message.contains("version"));
    }
    for end in 0..valid.len() {
        let message = invalid_data(
            parse_position_page_v1(&valid[..end], &AtomicBool::new(false), None),
            "truncated page",
        );
        assert!(message.contains("truncated"), "prefix length {end}");
    }

    let mut trailing = valid.clone();
    trailing.push(0);
    let message = invalid_data(
        parse_position_page_v1(&trailing, &AtomicBool::new(false), None),
        "trailing byte",
    );
    assert!(message.contains("trailing"));

    let mut missing_drive = valid;
    missing_drive[4..8].copy_from_slice(&2_u32.to_le_bytes());
    invalid_data(
        parse_position_page_v1(&missing_drive, &AtomicBool::new(false), None),
        "declared drive is absent",
    );

    let mut missing_points = position_page(&[drive(17, &[])]);
    missing_points[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    invalid_data(
        parse_position_page_v1(&missing_points, &AtomicBool::new(false), None),
        "declared point count exceeds available bytes",
    );
}

#[test]
fn binary_position_pages_enforce_drive_and_byte_caps() {
    let mut over_drive_cap = position_page(&[]);
    over_drive_cap[4..8].copy_from_slice(&129_u32.to_le_bytes());
    let message = invalid_data(
        parse_position_page_v1(&over_drive_cap, &AtomicBool::new(false), None),
        "binary page drive cap",
    );
    assert!(message.contains("129 drives"));

    let mut oversized = vec![0; 8 * 1024 * 1024 + 1];
    oversized[..4].copy_from_slice(&1_u32.to_le_bytes());
    let message = invalid_data(
        parse_position_page_v1(&oversized, &AtomicBool::new(false), None),
        "binary page byte cap",
    );
    assert!(message.contains("bounded page limit"));
}

#[test]
fn exactly_128_drives_are_accepted_by_binary_and_typed_pages() {
    let drives = (0..128).map(|id| drive(id, &[])).collect::<Vec<_>>();
    let decoded =
        parse_position_page_v1(&position_page(&drives), &AtomicBool::new(false), None).unwrap();
    assert_eq!(decoded, drives);

    let mut visitor_calls = 0;
    let paged = generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
        visitor_calls += 1;
        consume(&decoded)
    })
    .unwrap();
    assert_eq!(visitor_calls, 1);
    let expected = TileOutput::ReadyEmpty {
        drive_count: 128,
        diagnostics: Diagnostics {
            visitor_invocations: 1,
            drive_count: 128,
            input_point_count: 0,
            produced_tile_count: 0,
        },
    };
    assert_eq!(paged, expected);
    assert_eq!(generate(&drives), expected);
}

#[test]
fn paged_generation_visits_once_and_reports_all_original_drive_and_point_counts() {
    let first = vec![drive(17, &[]), drive(23, &[(51.50, -0.14), (51.51, -0.13)])];
    let second = vec![drive(29, &[(51.52, -0.12)])];
    let direct = generate(&[first[0].clone(), first[1].clone(), second[0].clone()]);
    let mut visitor_calls = 0;
    let paged = generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
        visitor_calls += 1;
        consume(&[])?;
        consume(&first)?;
        consume(&second)?;
        consume(&[])
    })
    .unwrap();
    assert_eq!(visitor_calls, 1);
    assert_eq!(
        ready_data_payload(&paged, 3, 3),
        ready_data_payload(&direct, 3, 3)
    );
    assert_eq!(paged, direct);
}

#[test]
fn packing_progress_can_cancel_after_generation_has_completed() {
    let packing_callbacks = AtomicUsize::new(0);
    let progress = |progress: Progress| {
        if progress.stage == ProgressStage::PackingPayload {
            packing_callbacks.fetch_add(1, Ordering::SeqCst);
            assert_eq!(progress.drives_completed, 1);
            assert_eq!(progress.drive_count, 1);
            return Err(Error::Cancelled);
        }
        Ok(())
    };
    assert_eq!(
        generate_tile_payload_v1(
            &[drive(17, &[(51.50, -0.14), (51.51, -0.13)])],
            &AtomicBool::new(false),
            Some(&progress),
        ),
        Err(Error::Cancelled)
    );
    assert_eq!(packing_callbacks.load(Ordering::SeqCst), 1);
}

#[test]
fn ignored_page_consumer_errors_cannot_publish_previously_accumulated_data() {
    let good = vec![drive(17, &[(51.50, -0.14), (51.51, -0.13)])];
    let rejected = (0..129).map(|id| drive(id, &[])).collect::<Vec<_>>();
    let mut observed_error = None;
    let result = generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
        consume(&good)?;
        let error = consume(&rejected).unwrap_err();
        assert!(matches!(&error, Error::InvalidData(_)));
        observed_error = Some(error);
        Ok(())
    });
    assert_eq!(
        result,
        Err(observed_error.expect("page rejection was observed"))
    );
}

#[test]
fn first_page_consumer_error_is_returned_on_retries_without_processing_later_input() {
    let expected = Error::InvalidData("synthetic compute callback failure".into());
    let callback_calls = AtomicUsize::new(0);
    let progress = |_: Progress| {
        callback_calls.fetch_add(1, Ordering::SeqCst);
        Err(expected.clone())
    };
    let good = vec![drive(17, &[(51.50, -0.14), (51.51, -0.13)])];
    let rejected = (0..129).map(|id| drive(id, &[])).collect::<Vec<_>>();
    let result =
        generate_tile_payload_v1_from_pages(&AtomicBool::new(false), Some(&progress), |consume| {
            assert_eq!(consume(&good), Err(expected.clone()));
            assert_eq!(consume(&rejected), Err(expected.clone()));
            assert_eq!(consume(&good), Err(expected.clone()));
            assert_eq!(consume(&[]), Err(expected.clone()));
            Ok(())
        });
    assert_eq!(result, Err(expected));
    assert_eq!(callback_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn legitimate_page_visitor_errors_propagate_before_and_after_a_good_page() {
    let good = vec![drive(17, &[(51.50, -0.14), (51.51, -0.13)])];
    for expected in [
        Error::Cancelled,
        Error::InvalidData("synthetic storage visitor failure".into()),
    ] {
        for consume_good_page in [false, true] {
            assert_eq!(
                generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
                    if consume_good_page {
                        consume(&good)?;
                    }
                    Err(expected.clone())
                }),
                Err(expected.clone())
            );
        }
    }
}

fn rdp_limit_error() -> Error {
    Error::InvalidData("canonical RDP examined-work limit exceeded".into())
}

#[test]
fn zero_rdp_limit_accepts_valid_runs_with_at_most_two_points() {
    for input in [
        drive(17, &[]),
        drive(17, &[(51.50, -0.14)]),
        drive(17, &[(51.50, -0.14), (51.51, -0.13)]),
        drive(
            17,
            &[
                (51.50, -0.14),
                (51.501, -0.139),
                (f32::NAN, -0.138),
                (51.503, -0.137),
                (51.504, -0.136),
            ],
        ),
    ] {
        let drives = std::slice::from_ref(&input);
        let expected = generate(drives);
        assert_eq!(
            generate_tile_payload_v1_with_rdp_limit(drives, &AtomicBool::new(false), None, 0),
            Ok(expected.clone())
        );
        assert_eq!(
            generate_tile_payload_v1_from_pages_with_rdp_limit(
                &AtomicBool::new(false),
                None,
                0,
                |consume| consume(drives),
            ),
            Ok(expected)
        );
    }
}

#[test]
fn exact_rdp_limit_matches_legacy_bytes_without_budgeting_guard_or_raster_work() {
    let drives = vec![drive(17, &[(51.50, -0.14), (51.50, -0.13), (51.50, -0.12)])];
    let expected = generate(&drives);
    ready_data_payload(&expected, 1, 3);
    assert_eq!(
        generate_tile_payload_v1_with_rdp_limit(&drives, &AtomicBool::new(false), None, 4),
        Ok(expected)
    );
    assert_eq!(
        generate_tile_payload_v1_with_rdp_limit(&drives, &AtomicBool::new(false), None, 3),
        Err(rdp_limit_error())
    );
}

#[test]
fn rdp_limit_is_shared_across_drives_and_page_partitions_in_any_order() {
    let first = drive(17, &[(51.50, -0.14), (51.50, -0.13), (51.50, -0.12)]);
    let second = drive(23, &[(51.52, -0.12), (51.52, -0.11), (51.52, -0.10)]);
    let expected = generate(&[first.clone(), second.clone()]);
    ready_data_payload(&expected, 2, 6);

    for drives in [vec![first.clone(), second.clone()], vec![second, first]] {
        assert_eq!(
            generate_tile_payload_v1_with_rdp_limit(&drives, &AtomicBool::new(false), None, 8),
            Ok(expected.clone())
        );
        assert_eq!(
            generate_tile_payload_v1_with_rdp_limit(&drives, &AtomicBool::new(false), None, 7),
            Err(rdp_limit_error())
        );
        for page_size in [1, 2] {
            let generate_paged = |limit| {
                generate_tile_payload_v1_from_pages_with_rdp_limit(
                    &AtomicBool::new(false),
                    None,
                    limit,
                    |consume| {
                        for page in drives.chunks(page_size) {
                            consume(page)?;
                        }
                        Ok(())
                    },
                )
            };
            assert_eq!(generate_paged(8), Ok(expected.clone()));
            assert_eq!(generate_paged(7), Err(rdp_limit_error()));
        }
    }
}

#[test]
fn ignored_rdp_limit_errors_latch_after_an_already_consumed_page() {
    let first = vec![drive(17, &[(51.50, -0.14), (51.50, -0.13), (51.50, -0.12)])];
    let second = vec![drive(23, &[(51.52, -0.12), (51.52, -0.11), (51.52, -0.10)])];
    let result = generate_tile_payload_v1_from_pages_with_rdp_limit(
        &AtomicBool::new(false),
        None,
        7,
        |consume| {
            consume(&first)?;
            assert_eq!(consume(&second), Err(rdp_limit_error()));
            assert_eq!(consume(&[]), Err(rdp_limit_error()));
            assert_eq!(consume(&first), Err(rdp_limit_error()));
            Ok(())
        },
    );
    assert_eq!(result, Err(rdp_limit_error()));
}

#[test]
fn rdp_limited_generation_preserves_cancellation_errors() {
    let drives = vec![drive(17, &[(51.50, -0.14), (51.50, -0.13), (51.50, -0.12)])];
    assert_eq!(
        generate_tile_payload_v1_with_rdp_limit(&drives, &AtomicBool::new(true), None, 0),
        Err(Error::Cancelled)
    );
    assert_eq!(
        generate_tile_payload_v1_from_pages_with_rdp_limit(
            &AtomicBool::new(true),
            None,
            0,
            |_| panic!("cancelled generation must not call the page visitor"),
        ),
        Err(Error::Cancelled)
    );

    let progress = |progress: Progress| {
        if progress.stage == ProgressStage::GeneratingTier && progress.tier_index == 1 {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    };
    assert_eq!(
        generate_tile_payload_v1_with_rdp_limit(
            &drives,
            &AtomicBool::new(false),
            Some(&progress),
            4,
        ),
        Err(Error::Cancelled)
    );
    assert_eq!(
        generate_tile_payload_v1_from_pages_with_rdp_limit(
            &AtomicBool::new(false),
            Some(&progress),
            4,
            |consume| consume(&drives),
        ),
        Err(Error::Cancelled)
    );
}

fn dispersed_drives() -> Vec<Drive> {
    (0..32)
        .flat_map(|row| {
            (0..128).map(move |column| {
                let latitude = -75.0_f32 + row as f32 * 5.0;
                let longitude = -179.0_f32 + column as f32 * 2.8;
                drive(
                    row * 128 + column,
                    &[(latitude, longitude), (latitude, longitude + 0.001)],
                )
            })
        })
        .collect()
}

fn quota_boundary_drive(tile_x: u32, start_pixel: f64, end_pixel: f64) -> Drive {
    // Two endpoints straddle a rounding threshold only at the selected zoom.
    // Pixels .4/.6 differ only at z13; 3.9/4.1 differ only at z10.
    let longitude =
        |pixel| (((f64::from(tile_x) * 512.0 + pixel) / (8192.0 * 512.0)) * 360.0 - 180.0) as f32;
    drive(
        tile_x as i32,
        &[(1.0, longitude(start_pixel)), (1.0, longitude(end_pixel))],
    )
}

fn quota_boundary_drives() -> Vec<Drive> {
    let mut drives = (0..4095)
        .map(|x| quota_boundary_drive(x, 0.4, 0.6))
        .collect::<Vec<_>>();
    drives.push(quota_boundary_drive(0, 3.9, 4.1));
    drives
}

fn generate_zero_rdp_pages(drives: &[Drive]) -> Result<TileOutput, Error> {
    generate_tile_payload_v1_from_pages_with_rdp_limit(
        &AtomicBool::new(false),
        None,
        0,
        |consume| {
            for page in drives.chunks(128) {
                consume(page)?;
            }
            Ok(())
        },
    )
}

#[test]
fn exactly_4096_cross_tier_tiles_admit_duplicates_and_preserve_bytes() {
    let drives = quota_boundary_drives();
    let output = generate_zero_rdp_pages(&drives).unwrap();
    let payload = ready_data_payload(&output, 4096, 8192);
    assert_eq!(u32::from_le_bytes(payload[4..8].try_into().unwrap()), 4096);
    assert_eq!(payload.len(), 8 + 4096 * (16 + 8));
    // Independent wire decoding confirms nonempty, one-segment publication and
    // the exact cross-tier directory, rather than trusting diagnostics alone.
    for (index, tile) in payload[8..].chunks_exact(24).enumerate() {
        let read = |offset| u32::from_le_bytes(tile[offset..offset + 4].try_into().unwrap());
        assert_eq!(read(0), if index == 0 { 10 } else { 13 });
        assert_eq!(read(4), if index == 0 { 0 } else { (index - 1) as u32 });
        assert_eq!(read(12), 8);
        assert_ne!(&tile[16..20], &tile[20..24]);
    }

    let mut duplicated = drives.clone();
    duplicated.extend(drives.iter().cloned());
    let duplicate_output = generate_zero_rdp_pages(&duplicated).unwrap();
    assert_eq!(ready_data_payload(&duplicate_output, 8192, 16384), payload);

    let below = generate_zero_rdp_pages(&drives[1..]).unwrap();
    let below_payload = ready_data_payload(&below, 4095, 8190);
    assert_eq!(
        u32::from_le_bytes(below_payload[4..8].try_into().unwrap()),
        4095
    );
}

#[test]
fn tile_4097_is_rejected_inside_the_consumer_across_tiers() {
    let drives = quota_boundary_drives();
    let next = [quota_boundary_drive(4095, 0.4, 0.6)];
    let expected =
        Error::InvalidData("canonical CPU output contains 4097 tiles over the 4096 cap".into());
    let packing_callbacks = AtomicUsize::new(0);
    let progress = |progress: Progress| {
        if progress.stage == ProgressStage::PackingPayload {
            packing_callbacks.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    };
    let mut consumed = 0;
    let output = generate_tile_payload_v1_from_pages_with_rdp_limit(
        &AtomicBool::new(false),
        Some(&progress),
        0,
        |consume| {
            for page in drives.chunks(128) {
                consume(page)?;
                consumed += page.len();
            }
            assert_eq!(consumed, 4096);
            assert_eq!(consume(&next), Err(expected.clone()));
            assert_eq!(consume(&drives[..1]), Err(expected.clone()));
            Err(Error::Cancelled) // The original consumer error wins.
        },
    );
    assert_eq!(output, Err(expected));
    assert_eq!(packing_callbacks.load(Ordering::SeqCst), 0);
}

#[test]
fn tile_quota_rejects_during_page_admission_before_packing() {
    // Actual public generation, not a constructed encoder state. Two-point
    // drives spend no RDP evaluations, isolating the publication tile limit.
    let drives = dispersed_drives();
    let packing_callbacks = AtomicUsize::new(0);
    let progress = |progress: Progress| {
        if progress.stage == ProgressStage::PackingPayload {
            packing_callbacks.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    };
    let mut accepted_pages = 0;
    let result = generate_tile_payload_v1_from_pages_with_rdp_limit(
        &AtomicBool::new(false),
        Some(&progress),
        0,
        |consume| {
            for page in drives.chunks(128) {
                consume(page)?;
                accepted_pages += 1;
            }
            Ok(())
        },
    );
    let message = invalid_data(result, "distinct tile admission");
    assert!(message.contains("tiles over the 4096 cap"));
    assert!(accepted_pages > 0, "a valid prefix must be admitted");
    assert!(accepted_pages < 31, "later pages must remain unread");
    assert_eq!(packing_callbacks.load(Ordering::SeqCst), 0);
}

#[test]
fn ignored_tile_quota_error_latches_before_callbacks_or_later_validation() {
    let drives = dispersed_drives();
    let callbacks = AtomicUsize::new(0);
    let progress = |_: Progress| {
        callbacks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let cancelled = AtomicBool::new(false);
    let mut original_error = None;
    let result = generate_tile_payload_v1_from_pages_with_rdp_limit(
        &cancelled,
        Some(&progress),
        0,
        |consume| {
            for page in drives.chunks(128) {
                if let Err(error) = consume(page) {
                    assert!(matches!(&error, Error::InvalidData(message)
                        if message.contains("tiles over the 4096 cap")));
                    let before = callbacks.load(Ordering::SeqCst);
                    cancelled.store(true, Ordering::SeqCst);
                    let malformed = (0..129).map(|id| drive(id, &[])).collect::<Vec<_>>();
                    assert_eq!(consume(&malformed), Err(error.clone()));
                    assert_eq!(consume(page), Err(error.clone()));
                    assert_eq!(consume(&[]), Err(error.clone()));
                    assert_eq!(callbacks.load(Ordering::SeqCst), before);
                    original_error = Some(error);
                    return Ok(()); // Even an error-ignoring visitor cannot publish.
                }
            }
            panic!("quota must fail inside the consumer");
        },
    );
    assert_eq!(result, Err(original_error.unwrap()));
}

#[test]
fn whole_drive_page_grouping_preserves_multi_point_routes_but_does_not_join_ids() {
    let mut drives = (0..8)
        .map(|id| {
            let latitude = 51.5 + id as f32 * 0.01;
            drive(
                id,
                &[
                    (latitude, -0.14),
                    (latitude + 0.001, -0.139),
                    (latitude + 0.003, -0.137),
                    (latitude + 0.004, -0.136),
                    (f32::NAN, -0.135),
                    (latitude + 0.006, -0.134),
                    (latitude + 0.007, -0.133),
                ],
            )
        })
        .collect::<Vec<_>>();
    drives.push(drive(8, &[]));
    drives.push(drive(9, &[(51.6, -0.1)]));
    let expected = generate(&drives);
    ready_data_payload(&expected, 10, 57);
    drives.reverse();
    assert_eq!(generate(&drives), expected);
    for page_size in [1, 3, 7, 10] {
        let mut visits = 0;
        let output =
            generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
                visits += 1;
                consume(&[])?;
                for page in drives.chunks(page_size) {
                    consume(page)?;
                }
                consume(&[])
            })
            .unwrap();
        assert_eq!(visits, 1);
        assert_eq!(output, expected);
    }

    let points = [
        (51.5, -0.14),
        (51.501, -0.139),
        (51.503, -0.137),
        (51.504, -0.136),
    ];
    let whole = generate(&[drive(17, &points)]);
    let fragments = [drive(17, &points[..2]), drive(17, &points[2..])];
    let fragmented = generate(&fragments);
    assert_ne!(
        ready_data_payload(&whole, 1, 4),
        ready_data_payload(&fragmented, 2, 4)
    );
    let paged = generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
        consume(&fragments[..1])?;
        consume(&fragments[1..])
    })
    .unwrap();
    assert_eq!(paged, fragmented);
}

#[test]
fn public_cleaner_to_payload_preserves_short_intervals_near_both_epoch_limits() {
    let coordinates = [
        (47.5, 19.0),
        (47.501, 19.001),
        (48.5, 20.0),
        (47.502, 19.002),
        (47.503, 19.003),
    ];
    let expected = [
        (47.5_f32, 19.0_f32),
        (47.501, 19.001),
        (47.502, 19.002),
        (47.503, 19.003),
    ];
    let expected_output = generate(&[drive(17, &expected)]);
    ready_data_payload(&expected_output, 1, 4);
    for base in [0_i64, i64::MIN, i64::MAX - 40_000, 1_700_000_000_000] {
        let raw = coordinates
            .iter()
            .enumerate()
            .map(|(index, &(latitude, longitude))| RawMapPosition {
                date_ms: base.checked_add(index as i64 * 10_000).unwrap(),
                latitude,
                longitude,
                speed_kmh: Some(50),
            })
            .collect::<Vec<_>>();
        let cleaned = prepare_positions_for_tile_rendering_v1(&raw).unwrap();
        assert_eq!(cleaned, expected, "epoch base {base}");
        assert_eq!(generate(&[drive(17, &cleaned)]), expected_output);
    }
}

#[test]
fn public_cleaner_acceleration_requires_a_bridgeable_route() {
    // Meridian displacements with ample margin from every policy threshold.
    // Expected membership is specified directly, rather than recomputing the
    // cleaner's speed/acceleration formula in this test.
    for (metres, interval, retained) in [
        ([0.0, 10.0, 50.0], 1_000, vec![0, 2]),
        ([0.0, 25.0, 50.0], 1_000, vec![0, 1, 2]),
        ([0.0, 1_000.0, 2_000.0], 10_000, vec![0, 1, 2]),
    ] {
        let raw = metres
            .iter()
            .enumerate()
            .map(|(index, &distance)| RawMapPosition {
                date_ms: index as i64 * interval,
                latitude: distance / (6_371_000.0 * std::f64::consts::PI / 180.0),
                longitude: 1.0,
                speed_kmh: None,
            })
            .collect::<Vec<_>>();
        let expected = retained
            .into_iter()
            .map(|index| (raw[index].latitude as f32, raw[index].longitude as f32))
            .collect::<Vec<_>>();
        assert_eq!(
            prepare_positions_for_tile_rendering_v1(&raw).unwrap(),
            expected
        );
        ready_data_payload(&generate(&[drive(17, &expected)]), 1, expected.len() as i64);
    }
}

#[test]
fn corrected_semantic_identity_is_independent_of_package_and_geometry_versions() {
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.1.0");
    assert_eq!(ALGORITHM_VERSION, "0.1.1");
    assert_ne!(ALGORITHM_VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(TILE_GEOMETRY_VERSION, "raster-v9-rounded-tile-px");
    // This valid route distinguished historical empty output from corrected
    // nonempty output. Hub cache/delivery acceptance is a separate product gate.
    for alias in [-180.0_f32, 180.0] {
        let mixed = generate(&[drive(17, &[(66.0, alias), (66.01, -alias)])]);
        let same_alias = generate(&[drive(17, &[(66.0, alias), (66.01, alias)])]);
        assert_eq!(ready_data_payload(&mixed, 1, 2).len(), 296);
        assert_eq!(mixed, same_alias);
    }
}
