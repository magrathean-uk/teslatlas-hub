// SPDX-FileCopyrightText: 2026 Magrathean Technologies Ltd
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use teslatlas_compute::{
    Drive, Error, TileOutput, generate_tile_payload_v1, generate_tile_payload_v1_from_pages,
    parse_position_page_v1,
};

fn drive(id: i32, points: &[(f32, f32)]) -> Drive {
    Drive {
        drive_id: id,
        points: points.to_vec(),
    }
}

fn position_page(drives: &[Drive]) -> Vec<u8> {
    let mut page = Vec::new();
    page.extend_from_slice(&1_u32.to_le_bytes());
    page.extend_from_slice(&(drives.len() as u32).to_le_bytes());
    for drive in drives {
        page.extend_from_slice(&drive.drive_id.to_le_bytes());
        page.extend_from_slice(&(drive.points.len() as u32).to_le_bytes());
        for (latitude, longitude) in &drive.points {
            page.extend_from_slice(&latitude.to_bits().to_le_bytes());
            page.extend_from_slice(&longitude.to_bits().to_le_bytes());
        }
    }
    page
}

fn payload(output: TileOutput) -> Vec<u8> {
    match output {
        TileOutput::ReadyData { payload, .. } => payload,
        TileOutput::ReadyEmpty { .. } => panic!("expected tile data"),
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn assert_golden(bytes: &[u8], expected_len: usize, expected_fingerprint: u64) {
    assert_eq!(
        (bytes.len(), fingerprint(bytes)),
        (expected_len, expected_fingerprint)
    );
}

fn decoded_keys(bytes: &[u8]) -> Vec<(u32, u32, u32)> {
    let read = |offset: &mut usize| {
        let value = u32::from_le_bytes(bytes[*offset..*offset + 4].try_into().unwrap());
        *offset += 4;
        value
    };
    let mut offset = 0;
    assert_eq!(read(&mut offset), 1);
    let count = read(&mut offset);
    let mut keys = Vec::new();
    for _ in 0..count {
        let key = (read(&mut offset), read(&mut offset), read(&mut offset));
        let len = read(&mut offset) as usize;
        offset += len;
        keys.push(key);
    }
    assert_eq!(offset, bytes.len());
    keys
}

#[test]
fn empty_input_is_the_typed_empty_golden() {
    let output = generate_tile_payload_v1(&[], &AtomicBool::new(false), None).unwrap();
    assert_eq!(
        output,
        TileOutput::ReadyEmpty {
            drive_count: 0,
            diagnostics: teslatlas_compute::Diagnostics {
                visitor_invocations: 1,
                ..Default::default()
            },
        }
    );
}

#[test]
fn position_page_v1_and_page_splits_preserve_order_independent_bytes() {
    let first = drive(17, &[(51.50, -0.14), (51.51, -0.13)]);
    let second = drive(23, &[(51.52, -0.12), (51.53, -0.11)]);
    let direct = payload(
        generate_tile_payload_v1(
            &[first.clone(), second.clone()],
            &AtomicBool::new(false),
            None,
        )
        .unwrap(),
    );
    let reversed = payload(
        generate_tile_payload_v1(
            &[second.clone(), first.clone()],
            &AtomicBool::new(false),
            None,
        )
        .unwrap(),
    );
    let first_page = parse_position_page_v1(
        &position_page(std::slice::from_ref(&first)),
        &AtomicBool::new(false),
        None,
    )
    .unwrap();
    let second_page = parse_position_page_v1(
        &position_page(std::slice::from_ref(&second)),
        &AtomicBool::new(false),
        None,
    )
    .unwrap();
    let paged = payload(
        generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
            consume(&first_page)?;
            consume(&second_page)
        })
        .unwrap(),
    );
    assert_eq!(direct, reversed);
    assert_eq!(direct, paged);
    assert_golden(&direct, 440, 7_014_253_990_521_307_000);

    assert_eq!(
        parse_position_page_v1(
            &position_page(std::slice::from_ref(&first)),
            &AtomicBool::new(false),
            None
        )
        .unwrap(),
        vec![first]
    );
}

#[test]
fn antimeridian_golden_uses_only_world_edge_tiles() {
    let bytes = payload(
        generate_tile_payload_v1(
            &[drive(1, &[(0.5, 179.99), (0.5, -179.99)])],
            &AtomicBool::new(false),
            None,
        )
        .unwrap(),
    );
    let keys = decoded_keys(&bytes);
    assert!(!keys.is_empty());
    assert!(
        keys.iter().all(|&(zoom, tile_x, _)| {
            tile_x == 0 || tile_x == (1_u32 << zoom).saturating_sub(1)
        })
    );
    assert_golden(&bytes, 392, 2_748_772_190_798_614_767);
}

#[test]
fn exact_tile_boundary_golden_is_sorted_and_legal_at_every_lod() {
    let bytes = payload(
        generate_tile_payload_v1(
            &[drive(1, &[(84.0, -90.1), (84.0, -90.0), (84.0, -89.9)])],
            &AtomicBool::new(false),
            None,
        )
        .unwrap(),
    );
    let keys = decoded_keys(&bytes);
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        keys.iter()
            .map(|(zoom, _, _)| *zoom)
            .collect::<BTreeSet<_>>(),
        (2..=13).collect()
    );
    assert!(
        keys.iter()
            .all(|&(zoom, x, y)| x < (1 << zoom) && y < (1 << zoom))
    );
    assert_golden(&bytes, 728, 2_764_110_651_771_840_634);
}

#[test]
fn polar_clamp_golden_stays_in_web_mercator_bounds() {
    let bytes = payload(
        generate_tile_payload_v1(
            &[drive(1, &[(90.0, -0.01), (90.0, 0.01)])],
            &AtomicBool::new(false),
            None,
        )
        .unwrap(),
    );
    let keys = decoded_keys(&bytes);
    assert!(!keys.is_empty());
    assert!(
        keys.iter()
            .all(|&(zoom, x, y)| x < (1 << zoom) && y < (1 << zoom))
    );
    assert_golden(&bytes, 392, 17_577_750_102_124_562_831);
}

#[test]
fn invalid_coordinate_discontinuity_matches_explicitly_split_drives() {
    let with_gap = payload(
        generate_tile_payload_v1(
            &[drive(
                1,
                &[
                    (51.50, -0.14),
                    (51.501, -0.139),
                    (0.0, 0.0),
                    (51.503, -0.137),
                    (51.504, -0.136),
                ],
            )],
            &AtomicBool::new(false),
            None,
        )
        .unwrap(),
    );
    let split = payload(
        generate_tile_payload_v1(
            &[
                drive(1, &[(51.50, -0.14), (51.501, -0.139)]),
                drive(2, &[(51.503, -0.137), (51.504, -0.136)]),
            ],
            &AtomicBool::new(false),
            None,
        )
        .unwrap(),
    );
    assert_eq!(with_gap, split);
    assert_golden(&with_gap, 200, 1_482_231_178_438_507_166);

    let long_gap = generate_tile_payload_v1(
        &[drive(3, &[(51.0, -0.1), (51.2, -0.1)])],
        &AtomicBool::new(false),
        None,
    )
    .unwrap();
    assert!(matches!(
        long_gap,
        TileOutput::ReadyEmpty { drive_count: 1, .. }
    ));
}

#[test]
fn cancellation_is_observed_before_and_during_compute() {
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        generate_tile_payload_v1(&[], &cancelled, None),
        Err(Error::Cancelled)
    );

    let cancelled = Arc::new(AtomicBool::new(false));
    let progress_token = Arc::clone(&cancelled);
    let progress = move |_| {
        progress_token.store(true, Ordering::SeqCst);
        Ok(())
    };
    assert_eq!(
        generate_tile_payload_v1(
            &[drive(1, &[(51.50, -0.14), (51.51, -0.13)])],
            cancelled.as_ref(),
            Some(&progress),
        ),
        Err(Error::Cancelled)
    );

    let cancelled = AtomicBool::new(false);
    assert_eq!(
        generate_tile_payload_v1_from_pages(&cancelled, None, |consume| {
            cancelled.store(true, Ordering::SeqCst);
            consume(&[])
        }),
        Err(Error::Cancelled)
    );
}

#[test]
fn public_typed_pages_reject_drive_and_byte_limit_overruns() {
    let too_many_drives = (0..129)
        .map(|drive_id| Drive {
            drive_id,
            points: Vec::new(),
        })
        .collect::<Vec<_>>();
    let drive_error =
        generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
            consume(&too_many_drives)
        })
        .unwrap_err();
    assert!(matches!(drive_error, Error::InvalidData(message) if message.contains("129 drives")));

    let oversized_page = vec![Drive {
        drive_id: 1,
        points: vec![(51.0, -0.1); 1_048_576],
    }];
    let byte_error =
        generate_tile_payload_v1_from_pages(&AtomicBool::new(false), None, |consume| {
            consume(&oversized_page)
        })
        .unwrap_err();
    assert!(
        matches!(byte_error, Error::InvalidData(message) if message.contains("bounded page limit"))
    );
}
