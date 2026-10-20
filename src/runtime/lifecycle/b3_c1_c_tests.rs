// SPDX-License-Identifier: AGPL-3.0-only

//! C1-C unit-level spec test (C1 section 8a, C1 R94: C1 ST113): the time an
//! import records for the `updates` watermark, `updates_watermark_time_ms`. The
//! storage half (C1 ST104 (g) and (h), through the seeding paths) is in
//! `src/storage/db/tests/b3_c1_c.rs`. Synthetic data only. `T0` is
//! 1_760_000_000_000 ms and `D` one day; each pair is `(start_ms, end_ms)`.

use super::*;

const T0: i64 = 1_760_000_000_000;
const D: i64 = 86_400_000;

/// The function over `pairs` in the given order and in the reverse order, which
/// must agree (the result does not depend on input order).
fn both_orders(pairs: &[(Option<i64>, Option<i64>)]) -> Option<i64> {
    let forward = updates_watermark_time_ms(pairs.iter().copied());
    let backward = updates_watermark_time_ms(pairs.iter().rev().copied());
    assert_eq!(forward, backward, "order changed the result of {pairs:?}");
    forward
}

#[test]
fn c1_st113_updates_watermark_time() {
    // No updates.
    assert_eq!(both_orders(&[]), None);
    // One closed update: its end, not its start.
    assert_eq!(
        both_orders(&[(Some(T0 - 3 * D), Some(T0 - 2 * D))]),
        Some(T0 - 2 * D)
    );
    // Two closed updates: the greatest end, in either order.
    let older = (Some(T0 - 5 * D), Some(T0 - 4 * D));
    let newer = (Some(T0 - 3 * D), Some(T0 - 2 * D));
    assert_eq!(updates_watermark_time_ms([older, newer]), Some(T0 - 2 * D));
    assert_eq!(updates_watermark_time_ms([newer, older]), Some(T0 - 2 * D));
    // The open newest update counts from its start.
    assert_eq!(
        both_orders(&[(Some(T0 - 3 * D), Some(T0 - 2 * D)), (Some(T0 - D), None)]),
        Some(T0 - D)
    );
    // A later end beats an older open update, in either order.
    let open = (Some(T0 - 3 * D), None);
    let ended = (Some(T0 - 5 * D), Some(T0 - 2 * D));
    assert_eq!(updates_watermark_time_ms([open, ended]), Some(T0 - 2 * D));
    assert_eq!(updates_watermark_time_ms([ended, open]), Some(T0 - 2 * D));
    // A non-positive end counts as none, so the start is used.
    assert_eq!(
        both_orders(&[(Some(T0 - 3 * D), Some(0))]),
        Some(T0 - 3 * D)
    );
    assert_eq!(
        both_orders(&[(Some(T0 - 3 * D), Some(-1))]),
        Some(T0 - 3 * D)
    );
    // Nothing known or positive: none.
    assert_eq!(both_orders(&[(None, None)]), None);
    assert_eq!(both_orders(&[(Some(0), None)]), None);
    assert_eq!(both_orders(&[(Some(-5), Some(-1))]), None);
    assert_eq!(both_orders(&[(Some(0), Some(0)), (None, Some(-3))]), None);
    // An update with a known end but no start counts from its end.
    assert_eq!(both_orders(&[(None, Some(T0 - 2 * D))]), Some(T0 - 2 * D));
    // An end before its start is taken as recorded.
    assert_eq!(
        both_orders(&[(Some(T0 - D), Some(T0 - 2 * D))]),
        Some(T0 - 2 * D)
    );
    // An update with no time does not hide the others.
    assert_eq!(
        both_orders(&[(None, None), (Some(T0 - 3 * D), Some(T0 - 2 * D))]),
        Some(T0 - 2 * D)
    );
}
