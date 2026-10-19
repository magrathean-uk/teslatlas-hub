// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering},
};

use futures_util::{StreamExt, stream};

use super::*;

/// A row that only reports its size. `tokio_postgres::Row` cannot be built
/// outside its crate, so the bounded reader is generic over `SourceRow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MockRow(usize);

impl SourceRow for MockRow {
    fn source_bytes(&self) -> usize {
        self.0
    }
}

/// A COPY row that only reports its field lengths: `None` is SQL NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MockFields(Vec<Option<usize>>);

impl WireFields for MockFields {
    fn field_length(&self, column: usize) -> Option<usize> {
        self.0[column]
    }
}

/// A canceller that records the requests it receives: how many items the
/// source stream had yielded when each one arrived. Delivery takes a yield, so
/// a read that does not wait for it is seen as `delivered == false`.
#[derive(Clone)]
struct RecordingCancel {
    polled: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<usize>>>,
    delivered: Arc<AtomicBool>,
}

impl RecordingCancel {
    fn new(polled: &Arc<AtomicUsize>) -> Self {
        Self {
            polled: Arc::clone(polled),
            requests: Arc::default(),
            delivered: Arc::default(),
        }
    }

    fn requests(&self) -> Vec<usize> {
        self.requests.lock().unwrap().clone()
    }

    fn delivered(&self) -> bool {
        self.delivered.load(AtomicOrdering::SeqCst)
    }
}

impl SourceCancel for RecordingCancel {
    async fn cancel(&self) {
        self.requests
            .lock()
            .unwrap()
            .push(self.polled.load(AtomicOrdering::SeqCst));
        tokio::task::yield_now().await;
        self.delivered.store(true, AtomicOrdering::SeqCst);
    }
}

/// What one bounded read over a mock stream did: its result, how many items it
/// pulled from the stream, the sizes of the rows it handed to the caller, and
/// the cancel requests it sent.
struct Observed {
    result: Result<(), TeslaMateReaderError>,
    polled: usize,
    retained: Vec<usize>,
    cancels: Vec<usize>,
    cancel_delivered: bool,
}

fn counted<I>(items: I, polled: &Arc<AtomicUsize>) -> impl futures_util::Stream<Item = I::Item>
where
    I: IntoIterator,
{
    let polled = Arc::clone(polled);
    stream::iter(items).inspect(move |_| {
        polled.fetch_add(1, AtomicOrdering::SeqCst);
    })
}

async fn read_sizes(sizes: &[usize], budget: SourceReadBudget) -> Observed {
    let polled = Arc::new(AtomicUsize::new(0));
    let cancel = RecordingCancel::new(&polled);
    let items = sizes
        .iter()
        .map(|size| Ok::<_, TeslaMateReaderError>(MockRow(*size)))
        .collect::<Vec<_>>();
    let mut retained = Vec::new();
    let result = retain_bounded_rows(
        counted(items, &polled),
        "positions",
        budget,
        cancel.clone(),
        |row| {
            retained.push(row.source_bytes());
            Ok(())
        },
    )
    .await;
    Observed {
        result,
        polled: polled.load(AtomicOrdering::SeqCst),
        retained,
        cancels: cancel.requests(),
        cancel_delivered: cancel.delivered(),
    }
}

fn assert_budget_error(
    result: &Result<(), TeslaMateReaderError>,
    expected_limit: SourceBudgetLimit,
    expected_maximum: usize,
) {
    match result {
        Err(TeslaMateReaderError::SourceReadBudgetExceeded {
            table,
            limit,
            maximum,
        }) => {
            assert_eq!(*table, "positions");
            assert_eq!(*limit, expected_limit);
            assert_eq!(*maximum, expected_maximum);
        }
        other => panic!("expected a {expected_limit:?} budget error, got {other:?}"),
    }
}

#[tokio::test]
async fn rows_over_the_count_stop_at_the_first_excess_row_without_retaining_it() {
    let budget = SourceReadBudget::new(3, 1000);
    let observed = read_sizes(&[10, 10, 10, 10, 10], budget).await;

    assert_budget_error(&observed.result, SourceBudgetLimit::Rows, 3);
    assert_eq!(observed.retained, vec![10, 10, 10]);
    // The fourth row is seen (that is how the excess is detected) and refused;
    // the fifth is never pulled from the stream.
    assert_eq!(observed.polled, 4);
}

#[tokio::test]
async fn a_page_with_exactly_the_row_budget_is_accepted() {
    let observed = read_sizes(&[10, 10, 10], SourceReadBudget::new(3, 1000)).await;

    observed.result.expect("rows at the budget are accepted");
    assert_eq!(observed.retained, vec![10, 10, 10]);
    assert_eq!(observed.polled, 3);

    let empty = read_sizes(&[], SourceReadBudget::new(3, 1000)).await;
    empty.result.expect("an empty page is accepted");
    assert!(empty.retained.is_empty());
}

#[tokio::test]
async fn one_oversized_row_is_refused_before_retention_even_when_the_page_has_room() {
    // The page budget is the 64 MiB ceiling, so only the 16 MiB per-row cap can
    // refuse the second row.
    let budget = SourceReadBudget::fixed(10);
    let observed = read_sizes(&[1024, MAX_SOURCE_ROW_BYTES + 1, 1024], budget).await;

    assert_budget_error(
        &observed.result,
        SourceBudgetLimit::RowBytes,
        MAX_SOURCE_ROW_BYTES,
    );
    assert_eq!(observed.retained, vec![1024]);
    assert_eq!(observed.polled, 2);
}

#[tokio::test]
async fn a_row_exactly_at_the_per_row_cap_is_accepted() {
    let observed = read_sizes(&[MAX_SOURCE_ROW_BYTES], SourceReadBudget::fixed(10)).await;

    observed.result.expect("a row at the cap is accepted");
    assert_eq!(observed.retained, vec![MAX_SOURCE_ROW_BYTES]);
}

#[tokio::test]
async fn the_row_cap_never_exceeds_the_page_budget() {
    // A page budget below 16 MiB lowers the per-row cap with it, and the
    // per-row cap is reported before the page cap.
    let budget = SourceReadBudget::new(10, 100);
    let observed = read_sizes(&[101], budget).await;

    assert_budget_error(&observed.result, SourceBudgetLimit::RowBytes, 100);
    assert!(observed.retained.is_empty());

    let at_cap = read_sizes(&[100], budget).await;
    at_cap.result.expect("a row at the page budget fits alone");
    assert_eq!(at_cap.retained, vec![100]);
}

#[tokio::test]
async fn a_page_over_the_byte_budget_stops_at_the_row_that_crosses_it() {
    let budget = SourceReadBudget::new(10, 100);
    let observed = read_sizes(&[40, 40, 40, 40], budget).await;

    assert_budget_error(&observed.result, SourceBudgetLimit::PageBytes, 100);
    // 40 + 40 + 40 = 120 > 100: the third row is refused, not retained.
    assert_eq!(observed.retained, vec![40, 40]);
    assert_eq!(observed.polled, 3);
}

#[tokio::test]
async fn a_page_exactly_at_the_byte_budget_is_accepted() {
    let budget = SourceReadBudget::new(10, 100);
    let observed = read_sizes(&[40, 40, 20], budget).await;

    observed
        .result
        .expect("a page at the byte budget is accepted");
    assert_eq!(observed.retained, vec![40, 40, 20]);
    assert_eq!(observed.polled, 3);

    // One byte more than the budget is refused.
    let over = read_sizes(&[40, 40, 21], budget).await;
    assert_budget_error(&over.result, SourceBudgetLimit::PageBytes, 100);
    assert_eq!(over.retained, vec![40, 40]);
}

#[tokio::test]
async fn a_lookahead_row_is_retained_and_the_next_one_is_not() {
    // LIMIT 2 callers (settings singleton, token pair) keep one row plus the
    // lookahead row that reveals ambiguity.
    let budget = SourceReadBudget::new(1, 1000).with_lookahead();

    let ambiguous = read_sizes(&[10, 10], budget).await;
    ambiguous
        .result
        .expect("the lookahead row is within budget");
    assert_eq!(ambiguous.retained, vec![10, 10]);

    let hostile = read_sizes(&[10, 10, 10], budget).await;
    assert_budget_error(&hostile.result, SourceBudgetLimit::Rows, 2);
    assert_eq!(hostile.retained, vec![10, 10]);
    assert_eq!(hostile.polled, 3);

    // Without the lookahead the same second row is already an excess.
    let strict = read_sizes(&[10, 10], SourceReadBudget::new(1, 1000)).await;
    assert_budget_error(&strict.result, SourceBudgetLimit::Rows, 1);
    assert_eq!(strict.retained, vec![10]);
}

#[tokio::test]
async fn a_stream_error_ends_the_read_with_that_error() {
    let polled = Arc::new(AtomicUsize::new(0));
    let cancel = RecordingCancel::new(&polled);
    let items = vec![
        Ok(MockRow(10)),
        Err(TeslaMateReaderError::InvalidSelectedCarId),
        Ok(MockRow(10)),
    ];
    let mut retained = 0_usize;
    let result = retain_bounded_rows(
        counted(items, &polled),
        "positions",
        SourceReadBudget::new(5, 100),
        cancel.clone(),
        |_| {
            retained += 1;
            Ok(())
        },
    )
    .await;

    assert!(matches!(
        result,
        Err(TeslaMateReaderError::InvalidSelectedCarId)
    ));
    assert_eq!(retained, 1);
    assert_eq!(polled.load(AtomicOrdering::SeqCst), 2);
    // Only a budget excess asks the server to stop.
    assert!(cancel.requests().is_empty());
}

#[tokio::test]
async fn a_caller_error_stops_the_read_at_that_row() {
    let polled = Arc::new(AtomicUsize::new(0));
    let cancel = RecordingCancel::new(&polled);
    let items = (0..5)
        .map(|_| Ok::<_, TeslaMateReaderError>(MockRow(1)))
        .collect::<Vec<_>>();
    let mut seen = 0_usize;
    let result = retain_bounded_rows(
        counted(items, &polled),
        "positions",
        SourceReadBudget::new(5, 100),
        cancel.clone(),
        |_| {
            seen += 1;
            if seen == 2 {
                Err(TeslaMateReaderError::InvalidStateStatus)
            } else {
                Ok(())
            }
        },
    )
    .await;

    assert!(matches!(
        result,
        Err(TeslaMateReaderError::InvalidStateStatus)
    ));
    assert_eq!(polled.load(AtomicOrdering::SeqCst), 2);
    assert!(cancel.requests().is_empty());
}

#[tokio::test]
async fn collecting_returns_every_row_in_order_when_within_budget() {
    let polled = Arc::new(AtomicUsize::new(0));
    let items = [1_usize, 2, 3]
        .into_iter()
        .map(|size| Ok::<_, TeslaMateReaderError>(MockRow(size)))
        .collect::<Vec<_>>();
    let within = RecordingCancel::new(&polled);
    let rows = collect_bounded_rows(
        stream::iter(items),
        "drives",
        SourceReadBudget::new(3, 6),
        within.clone(),
    )
    .await
    .expect("rows at both budgets are accepted");
    assert_eq!(rows, vec![MockRow(1), MockRow(2), MockRow(3)]);
    assert!(within.requests().is_empty());

    let items = [1_usize, 2, 3, 4]
        .into_iter()
        .map(|size| Ok::<_, TeslaMateReaderError>(MockRow(size)))
        .collect::<Vec<_>>();
    let over = RecordingCancel::new(&polled);
    let error = collect_bounded_rows(
        stream::iter(items),
        "drives",
        SourceReadBudget::new(3, 6),
        over.clone(),
    )
    .await
    .expect_err("a fourth row is over the row budget");
    assert_eq!(
        error.to_string(),
        "source drives read exceeded its row-count budget of 3"
    );
    assert_eq!(over.requests().len(), 1);
}

#[tokio::test]
async fn the_first_excess_of_each_budget_cancels_the_server_once_before_the_error_returns() {
    let rows = SourceReadBudget::new(2, 1000);
    let row_bytes = SourceReadBudget::new(10, 100);
    let page_bytes = SourceReadBudget::new(10, 100);
    for (sizes, budget, limit, maximum, polled, retained) in [
        (
            &[10_usize, 10, 10, 10][..],
            rows,
            SourceBudgetLimit::Rows,
            2,
            3,
            2,
        ),
        (
            &[10, 101, 10, 10][..],
            row_bytes,
            SourceBudgetLimit::RowBytes,
            100,
            2,
            1,
        ),
        (
            &[40, 40, 40, 40][..],
            page_bytes,
            SourceBudgetLimit::PageBytes,
            100,
            3,
            2,
        ),
    ] {
        let observed = read_sizes(sizes, budget).await;
        assert_budget_error(&observed.result, limit, maximum);
        // One request, sent when the excess row had been read and no further
        // row had been pulled, and delivered before the read returned.
        assert_eq!(observed.cancels, vec![polled], "{limit:?}");
        assert!(observed.cancel_delivered, "{limit:?}");
        assert_eq!(observed.polled, polled, "{limit:?}");
        assert_eq!(observed.retained.len(), retained, "{limit:?}");
    }

    // Reads inside every budget never send a cancel request.
    let within = read_sizes(&[40, 40, 20], SourceReadBudget::new(3, 100)).await;
    within.result.expect("a page exactly at the budgets");
    assert!(within.cancels.is_empty());
    assert!(!within.cancel_delivered);
}

/// The COPY readers measure rows by their field lengths and share the same
/// admission code (`BoundedRows`) as the keyset readers.
async fn read_copy_rows(
    rows: Vec<Vec<Option<usize>>>,
    columns: usize,
    budget: SourceReadBudget,
) -> (
    Vec<MockFields>,
    Result<(), TeslaMateReaderError>,
    Vec<usize>,
) {
    let polled = Arc::new(AtomicUsize::new(0));
    let cancel = RecordingCancel::new(&polled);
    let items = rows
        .into_iter()
        .map(|lengths| Ok::<_, TeslaMateReaderError>(MockFields(lengths)))
        .collect::<Vec<_>>();
    let types = vec![Type::INT8; columns];
    let copy = CopyRowStream::new(counted(items, &polled), &types);
    let mut bounded = BoundedRows::new(copy, "positions", budget, cancel.clone());
    let mut seen = Vec::new();
    let result = loop {
        match bounded.next_row().await {
            Ok(Some(row)) => seen.push(row.row),
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        }
    };
    (seen, result, cancel.requests())
}

#[tokio::test]
async fn an_oversized_copy_row_is_refused_before_the_caller_sees_it() {
    // A 3-column row is 2 + 3 * 4 header bytes plus its fields; NULL has none.
    let small = vec![Some(8), None, Some(10)];
    let huge = vec![Some(8), None, Some(1_000)];
    // A row of two header bytes, three length words and 18 field bytes is 32.
    let budget = SourceReadBudget::new(10, 64);

    let (seen, result, cancels) =
        read_copy_rows(vec![small.clone(), huge, small.clone()], 3, budget).await;

    assert_budget_error(&result, SourceBudgetLimit::RowBytes, 64);
    assert_eq!(seen, vec![MockFields(small)]);
    assert_eq!(cancels, vec![2]);
}

#[tokio::test]
async fn a_copy_collection_over_its_byte_budget_is_refused_at_the_crossing_row() {
    let row = vec![Some(8), None, Some(10)]; // 32 bytes
    let budget = SourceReadBudget::new(10, 100);

    let (seen, result, cancels) = read_copy_rows(vec![row.clone(); 4], 3, budget).await;

    // 32 + 32 + 32 = 96 fits; the fourth row would make 128.
    assert_budget_error(&result, SourceBudgetLimit::PageBytes, 100);
    assert_eq!(seen.len(), 3);
    assert_eq!(cancels, vec![4]);

    let (seen, result, cancels) = read_copy_rows(vec![row; 3], 3, budget).await;
    result.expect("a collection inside the budget");
    assert_eq!(seen.len(), 3);
    assert!(cancels.is_empty());
}

#[tokio::test]
async fn a_copy_row_that_cannot_be_measured_counts_as_maximal() {
    // A field that fails to read is reported as `usize::MAX` bytes.
    let (seen, result, cancels) =
        read_copy_rows(vec![vec![Some(usize::MAX)]], 1, SourceReadBudget::fixed(10)).await;
    assert_budget_error(&result, SourceBudgetLimit::RowBytes, MAX_SOURCE_ROW_BYTES);
    assert!(seen.is_empty());
    assert_eq!(cancels.len(), 1);
}

#[tokio::test]
async fn copy_rows_keep_their_columns_when_the_stream_is_adapted() {
    // Only the declared columns are measured, whatever the mock row holds.
    let (_, result, _) = read_copy_rows(
        vec![vec![Some(10), Some(10_000)]],
        1,
        SourceReadBudget::new(1, 64),
    )
    .await;
    result.expect("the second field is not a declared column");
}

#[test]
fn a_refused_row_does_not_consume_any_budget() {
    let mut meter = SourceReadMeter::new("states", SourceReadBudget::new(2, 100));

    meter.admit(60).expect("first row");
    assert!(matches!(
        meter.admit(200),
        Err(TeslaMateReaderError::SourceReadBudgetExceeded {
            limit: SourceBudgetLimit::RowBytes,
            ..
        })
    ));
    assert!(matches!(
        meter.admit(41),
        Err(TeslaMateReaderError::SourceReadBudgetExceeded {
            limit: SourceBudgetLimit::PageBytes,
            ..
        })
    ));
    // Neither refusal counted: exactly one row and 60 bytes are used.
    meter.admit(40).expect("second row fits the page exactly");
    assert!(matches!(
        meter.admit(0),
        Err(TeslaMateReaderError::SourceReadBudgetExceeded {
            limit: SourceBudgetLimit::Rows,
            maximum: 2,
            ..
        })
    ));
}

#[test]
fn budget_errors_name_the_table_and_the_limit_never_a_value() {
    let error = TeslaMateReaderError::SourceReadBudgetExceeded {
        table: "private.tokens",
        limit: SourceBudgetLimit::RowBytes,
        maximum: MAX_SOURCE_ROW_BYTES,
    };
    assert_eq!(
        error.to_string(),
        "source private.tokens read exceeded its per-row byte budget of 16777216"
    );
    assert_eq!(
        TeslaMateReaderError::SourceReadBudgetExceeded {
            table: "positions",
            limit: SourceBudgetLimit::PageBytes,
            maximum: MAX_SOURCE_PAGE_BYTES,
        }
        .to_string(),
        "source positions read exceeded its per-page byte budget of 67108864"
    );
}

#[test]
fn budget_error_text_never_names_the_source_project() {
    // The text reaches logs and import errors. Independence rule (a): a new
    // error names no source project, in any letter case.
    for limit in [
        SourceBudgetLimit::Rows,
        SourceBudgetLimit::RowBytes,
        SourceBudgetLimit::PageBytes,
    ] {
        for table in ["positions", "private.tokens", "states"] {
            let text = TeslaMateReaderError::SourceReadBudgetExceeded {
                table,
                limit,
                maximum: 4096,
            }
            .to_string()
            .to_ascii_lowercase();
            assert!(!text.contains("teslamate"), "{text}");
            assert!(text.contains(table), "{text}");
        }
        assert!(
            !limit.to_string().to_ascii_lowercase().contains("teslamate"),
            "{limit:?}"
        );
    }
}

#[test]
fn page_budgets_follow_the_stage_limit_between_one_and_sixty_four_mebibytes() {
    const MIB: u64 = 1024 * 1024;
    let limits = |stage| TeslaMateReadLimits {
        maximum_stage_bytes: stage,
        ..TeslaMateReadLimits::default()
    };

    assert_eq!(
        TeslaMateReadLimits::default().page_byte_budget(),
        64 * 1024 * 1024
    );
    // Below the floor a small stage limit does not undercut a real page.
    assert_eq!(limits(256 * 1024).page_byte_budget(), 1024 * 1024);
    assert_eq!(limits(MIB).page_byte_budget(), 1024 * 1024);
    assert_eq!(limits(8 * MIB).page_byte_budget(), 8 * 1024 * 1024);
    assert_eq!(limits(64 * MIB).page_byte_budget(), 64 * 1024 * 1024);
    assert_eq!(limits(64 * MIB + 1).page_byte_budget(), 64 * 1024 * 1024);
    assert_eq!(limits(u64::MAX).page_byte_budget(), 64 * 1024 * 1024);
}

#[test]
fn page_budget_derives_rows_and_row_cap_from_the_limits() {
    let defaults = TeslaMateReadLimits::default();
    assert_eq!(
        defaults.page_budget(),
        SourceReadBudget::new(10_000, 64 * 1024 * 1024)
    );
    assert_eq!(defaults.page_budget().row_bytes, MAX_SOURCE_ROW_BYTES);

    let small = TeslaMateReadLimits {
        page_size: 1,
        maximum_stage_bytes: 256 * 1024,
        ..defaults
    };
    assert_eq!(small.page_budget(), SourceReadBudget::new(1, 1024 * 1024));
    // The per-row cap is the page budget when that is below 16 MiB.
    assert_eq!(small.page_budget().row_bytes, 1024 * 1024);

    let invalid = TeslaMateReadLimits {
        page_size: -5,
        ..defaults
    };
    assert_eq!(invalid.page_budget().max_rows, 1);

    assert_eq!(defaults.budget_for_rows(2).max_rows, 2);
    // LIMIT 1 reads keep one row; LIMIT 2 reads (settings singleton, token
    // pair) keep a second, lookahead row so ambiguity stays a typed error.
    assert_eq!(defaults.singleton_budget().max_rows, 1);
    assert_eq!(defaults.ambiguity_budget().max_rows, 2);
    assert_eq!(legacy_token_budget().max_rows, 2);
    assert_eq!(legacy_token_budget().page_bytes, MAX_SOURCE_PAGE_BYTES);
    assert_eq!(defaults.copy_budget().max_rows, usize::MAX);
    assert_eq!(SourceReadBudget::fixed(7).page_bytes, MAX_SOURCE_PAGE_BYTES);
    assert_eq!(SourceReadBudget::fixed(7).row_bytes, MAX_SOURCE_ROW_BYTES);
    assert_eq!(SourceReadBudget::fixed(7).with_lookahead().max_rows, 8);
    assert_eq!(
        SourceReadBudget::fixed(usize::MAX)
            .with_lookahead()
            .max_rows,
        usize::MAX
    );
}

#[test]
fn copy_reads_stop_one_row_past_the_rows_left_under_the_global_cap() {
    let limits = TeslaMateReadLimits {
        maximum_rows: 10,
        ..TeslaMateReadLimits::default()
    };
    assert_eq!(copy_row_limit(limits, 0), 11);
    assert_eq!(copy_row_limit(limits, 4), 7);
    assert_eq!(copy_row_limit(limits, 10), 1);
    assert_eq!(copy_row_limit(limits, 25), 1);
    let unbounded = TeslaMateReadLimits {
        maximum_rows: usize::MAX,
        ..limits
    };
    assert_eq!(copy_row_limit(unbounded, 0), i64::MAX);
}

#[test]
fn capped_copy_sql_replaces_limit_all_for_every_collected_table() {
    for table in [
        SourceTable::Cars,
        SourceTable::Drives,
        SourceTable::ChargingProcesses,
        SourceTable::Charges,
        SourceTable::Addresses,
        SourceTable::Updates,
    ] {
        let capped = capped_binary_copy_sql(table, 7, 12);
        assert!(capped.starts_with("COPY ("), "{table:?}");
        assert!(capped.ends_with(") TO STDOUT WITH (FORMAT BINARY)"));
        assert!(capped.contains("LIMIT 12"), "{table:?}: {capped}");
        assert!(!capped.contains("LIMIT ALL"), "{table:?}");
        // Same projection and car scope as the uncapped statement.
        let uncapped = binary_copy_sql(table, 7);
        assert!(uncapped.contains("LIMIT ALL"), "{table:?}");
        assert_eq!(capped.replace("LIMIT 12", "LIMIT ALL"), uncapped);
    }
}

#[test]
fn copy_rows_are_measured_from_their_field_lengths() {
    // Field count, then a four-byte length per field plus its bytes; NULL has
    // no bytes.
    assert_eq!(wire_row_bytes([]), 2);
    assert_eq!(wire_row_bytes([Some(4), None, Some(10)]), 2 + 8 + 4 + 14);
    assert_eq!(
        wire_row_bytes([Some(usize::MAX), Some(1)]),
        usize::MAX,
        "the sum saturates instead of wrapping"
    );
}

#[test]
fn raw_fields_expose_the_wire_bytes_of_any_column_type() {
    for ty in [
        Type::INT4,
        Type::TEXT,
        Type::TIMESTAMP,
        Type::NUMERIC,
        Type::BYTEA,
    ] {
        assert!(<RawField<'_> as FromSql>::accepts(&ty), "{ty}");
        assert!(<Option<RawField<'_>> as FromSql>::accepts(&ty), "{ty}");
    }
    let field = <Option<RawField<'_>> as FromSql>::from_sql_nullable(
        &Type::TEXT,
        Some(b"synthetic".as_slice()),
    )
    .expect("raw field");
    assert_eq!(field.map(|field| field.0.len()), Some(9));
    let null = <Option<RawField<'_>> as FromSql>::from_sql_nullable(&Type::TEXT, None)
        .expect("null field");
    assert!(null.is_none());
}

/// Every PostgreSQL read that collects rows under `import::teslamate` must go
/// through the bounded reader. These are the call shapes it replaced.
#[test]
fn no_collecting_read_is_left_on_the_unbounded_client_query() {
    let reader_sources = [
        ("binary_rows.rs", include_str!("binary_rows.rs")),
        ("capture.rs", include_str!("capture.rs")),
        ("connection.rs", include_str!("connection.rs")),
        ("credentials.rs", include_str!("credentials.rs")),
        ("current_position.rs", include_str!("current_position.rs")),
        ("decoding.rs", include_str!("decoding.rs")),
        ("diagnostics.rs", include_str!("diagnostics.rs")),
        ("open_session.rs", include_str!("open_session.rs")),
        ("physical_capture.rs", include_str!("physical_capture.rs")),
        ("schema.rs", include_str!("schema.rs")),
        ("staging.rs", include_str!("staging.rs")),
        ("v2_rows.rs", include_str!("v2_rows.rs")),
        ("../reader.rs", include_str!("../reader.rs")),
        ("../direct.rs", include_str!("../direct.rs")),
        ("../importer.rs", include_str!("../importer.rs")),
    ];
    for (name, source) in reader_sources {
        assert!(
            !source.contains(".query("),
            "{name} collects rows with Client::query; use query_bounded"
        );
        assert!(
            !source.contains("Vec::with_capacity(page_len)"),
            "{name} sizes a vector from an unchecked page length"
        );
    }
    // Rows are streamed (`query_raw`, `copy_out`) in the bounded reader only,
    // apart from the direct producer's own streaming COPY passes.
    for (name, source) in reader_sources {
        if name == "../direct.rs" {
            continue;
        }
        assert!(
            !source.contains(".query_raw("),
            "{name} streams rows outside the bounded reader"
        );
        assert!(
            !source.contains(".copy_out("),
            "{name} reads a COPY outside the bounded reader"
        );
    }
    let bounded = include_str!("bounded.rs");
    assert!(bounded.contains(".query_raw("));
    assert!(bounded.contains(".copy_out("));
}

/// The PostgreSQL-facing glue (`query_bounded`, `open_bounded_copy`,
/// `ServerCancel`, `BoundedCopyRows`) cannot run without a server. Its
/// admission and cancel logic is the generic code tested above; these pins
/// fail if a wrapper stops calling it.
#[test]
fn the_postgres_entry_points_call_the_shared_admission_and_cancel_code() {
    let bounded = include_str!("bounded.rs");
    for call in [
        "collect_bounded_rows(stream, table, budget, ServerCancel::of(client))",
        "CopyRowStream::new(",
        "BoundedRows::new(rows, table, budget, ServerCancel::of(client))",
        "self.rows.next_row().await?",
        "self.meter.admit(row.source_bytes())",
        "self.cancel.cancel().await;",
        "token: client.cancel_token()",
        "self.token.cancel_query(tls)",
        "self.token.cancel_query(NoTls)",
        // The two measurements the budgets rest on: a row's size as the
        // server client reports it, and a COPY field's length on the wire
        // (an unreadable field counts as maximal).
        "self.raw_size_bytes()",
        "Ok(field) => field.map(|field| field.0.len())",
        "Err(_) => Some(usize::MAX)",
    ] {
        assert!(
            bounded.contains(call),
            "bounded.rs no longer contains `{call}`"
        );
    }
    // The COPY stream is opened with the declared column types, which size
    // every row. Whitespace is collapsed so a rustfmt re-wrap of the call
    // does not matter.
    let collapsed: String = bounded.split_whitespace().collect();
    let open_call = "CopyRowStream::new(tokio_postgres::binary_copy::BinaryCopyOutStream::new(stream,types),types";
    assert!(
        collapsed.contains(open_call),
        "open_bounded_copy no longer passes `types` to both the COPY stream and `CopyRowStream::new`"
    );
}

// ---- live proof against a real server -------------------------------------
//
// Ignored like the other PostgreSQL-gated tests. Point
// TESLATLAS_HUB_BOUNDED_READ_TEST_POSTGRES_URL at a throwaway server (any
// database; the tests only read generated series) and run with --ignored.
//
// A test that proves a bound must stay small and fast when that bound is
// broken (decision 42). Three limits hold for every live test below:
//
// * the input is at most about a thousand times its budget
//   (`LIVE_SERIES_ROWS` against a ten row budget), never a series sized to
//   exhaust memory;
// * the session has a server-side `statement_timeout`, so a statement that
//   nothing cancelled still ends by itself;
// * the whole test has a wall-clock limit (`LIVE_TEST_TIMEOUT`).
//
// The series is throttled with `pg_sleep`, not enlarged: a server that is never
// told to stop keeps producing for about a hundred seconds, far past the few
// seconds a test waits for it to go quiet, yet it cannot produce more than the
// series holds.

const LIVE_URL_ENV: &str = "TESLATLAS_HUB_BOUNDED_READ_TEST_POSTGRES_URL";
const LIVE_INT8: &[Type] = &[Type::INT8];
const LIVE_COPY_COLUMNS: &[Type] = &[Type::INT8, Type::TEXT, Type::TEXT];
const LIVE_ROW_BUDGET: usize = 10;
const LIVE_SERIES_ROWS: usize = 1000 * LIVE_ROW_BUDGET;
const LIVE_ROW_PAUSE: &str = "0.01";
const LIVE_STATEMENT_TIMEOUT: &str = "10s";
const LIVE_TEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a test lets the server go quiet after the cancel request. It is
/// shorter than the statement timeout, so the timeout cannot stand in for the
/// cancel.
const LIVE_QUIET_WINDOW: Duration = Duration::from_secs(5);

/// A set-returning function in the select list streams its rows; one in FROM
/// is materialised by the server before the first row is sent. `pg_sleep` runs
/// once per produced row.
fn live_query_sql() -> String {
    format!(
        "SELECT generate_series(1, {LIVE_SERIES_ROWS})::bigint AS id, \
         repeat('x', 200) AS padding, pg_sleep({LIVE_ROW_PAUSE})::text AS pause"
    )
}

/// The rows are wide so that the server's 8 KiB send buffer, which is flushed
/// only when full, reaches the client after a handful of rows.
fn live_copy_sql() -> String {
    format!(
        "COPY (SELECT generate_series(1, {LIVE_SERIES_ROWS})::bigint, \
         repeat('x', 1000), pg_sleep({LIVE_ROW_PAUSE})::text) \
         TO STDOUT WITH (FORMAT BINARY)"
    )
}

fn live_budget() -> SourceReadBudget {
    SourceReadBudget::new(LIVE_ROW_BUDGET, MAX_SOURCE_PAGE_BYTES)
}

/// The live series stay about a thousand times their budget at the most, and
/// the limits that bound a broken run are in place. This runs without a
/// server, so a later edit cannot quietly turn a live test into the unbounded
/// input that reset the owner's Mac.
#[test]
fn live_tests_keep_their_inputs_and_run_time_bounded() {
    for sql in [live_query_sql(), live_copy_sql()] {
        let after = sql
            .split("generate_series(1, ")
            .nth(1)
            .expect("the live statement generates a series");
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        let rows: usize = digits.parse().expect("the series length is a number");
        assert!(rows <= 1000 * LIVE_ROW_BUDGET, "{rows} rows in {sql}");
        assert!(rows > LIVE_ROW_BUDGET, "the series must outrun the budget");
        assert!(sql.contains("pg_sleep("), "the series is throttled: {sql}");
        assert_eq!(sql.matches("generate_series(").count(), 1, "{sql}");
    }
    assert_eq!(LIVE_COPY_COLUMNS.len(), 3);
    assert!(LIVE_TEST_TIMEOUT <= Duration::from_secs(120));
    // A broken run is ended by the server within the statement timeout, which
    // is longer than the quiet window, so the timeout alone could not make a
    // test pass.
    let statement_timeout: u64 = LIVE_STATEMENT_TIMEOUT
        .strip_suffix('s')
        .and_then(|seconds| seconds.parse().ok())
        .expect("the statement timeout is whole seconds");
    assert!((1..=30).contains(&statement_timeout));
    assert!(LIVE_QUIET_WINDOW.as_secs() < statement_timeout);
    // At the pause per row, an uncancelled series outlasts the quiet window by
    // a wide margin.
    let pause: f64 = LIVE_ROW_PAUSE.parse().expect("the pause is a number");
    assert!(LIVE_SERIES_ROWS as f64 * pause > 10.0 * LIVE_QUIET_WINDOW.as_secs_f64());
}

/// Run one live test body under the wall-clock limit.
async fn within_live_limit<F: std::future::Future>(body: F) -> F::Output {
    timeout(LIVE_TEST_TIMEOUT, body)
        .await
        .unwrap_or_else(|_| panic!("the live test passed its {LIVE_TEST_TIMEOUT:?} limit"))
}

async fn live_client() -> Client {
    let url = std::env::var(LIVE_URL_ENV).unwrap_or_else(|_| panic!("{LIVE_URL_ENV} is not set"));
    let (client, connection) = timeout(
        Duration::from_secs(10),
        tokio_postgres::connect(&url, NoTls),
    )
    .await
    .expect("connecting to the throwaway PostgreSQL server timed out")
    .expect("connect to the throwaway PostgreSQL server");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    // A statement that nothing cancelled ends by itself.
    client
        .batch_execute(&format!(
            "SET statement_timeout = '{LIVE_STATEMENT_TIMEOUT}'"
        ))
        .await
        .expect("set the session statement timeout");
    client
}

async fn backend_pid(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .expect("backend pid")
        .get(0)
}

/// The server stops producing within a few seconds of the budget error: the
/// backend is no longer `active`. Without the cancel request the throttled
/// series below keeps it active for about a hundred seconds.
async fn assert_backend_stops_producing(observer: &Client, pid: i32) {
    let polls = LIVE_QUIET_WINDOW.as_millis() / 100;
    for _ in 0..polls {
        let state: String = observer
            .query_one(
                "SELECT COALESCE((SELECT state FROM pg_stat_activity WHERE pid = $1), 'gone')",
                &[&pid],
            )
            .await
            .expect("observe the backend")
            .get(0);
        if state != "active" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the server is still producing rows after the read went over its budget");
}

#[tokio::test]
#[ignore = "needs PostgreSQL: set TESLATLAS_HUB_BOUNDED_READ_TEST_POSTGRES_URL (run with --ignored)"]
async fn live_a_query_over_its_row_budget_stops_the_server_and_leaves_the_connection_usable() {
    within_live_limit(async {
        let client = live_client().await;
        let observer = live_client().await;
        let pid = backend_pid(&client).await;

        let error = query_bounded(&client, &live_query_sql(), &[], "positions", live_budget())
            .await
            .expect_err("a thousand times the row budget is over the row budget");
        assert!(matches!(
            error,
            TeslaMateReaderError::SourceReadBudgetExceeded {
                table: "positions",
                limit: SourceBudgetLimit::Rows,
                maximum: LIVE_ROW_BUDGET,
            }
        ));

        assert_backend_stops_producing(&observer, pid).await;
        // The cancelled statement leaves the connection usable.
        let one: i32 = client.query_one("SELECT 1", &[]).await.unwrap().get(0);
        assert_eq!(one, 1);
    })
    .await;
}

#[tokio::test]
#[ignore = "needs PostgreSQL: set TESLATLAS_HUB_BOUNDED_READ_TEST_POSTGRES_URL (run with --ignored)"]
async fn live_a_copy_over_its_row_budget_stops_the_server() {
    within_live_limit(async {
        let client = live_client().await;
        let observer = live_client().await;
        let pid = backend_pid(&client).await;

        let mut copy = open_bounded_copy(
            &client,
            &live_copy_sql(),
            LIVE_COPY_COLUMNS,
            "positions",
            live_budget(),
        )
        .await
        .expect("open the copy");
        let mut seen = Vec::new();
        let error = loop {
            match copy.next_row().await {
                Ok(Some(row)) => seen.push(row.get::<i64>(0)),
                Ok(None) => panic!("the series cannot end inside a ten row budget"),
                Err(error) => break error,
            }
        };
        assert_eq!(seen, (1..=10).collect::<Vec<i64>>());
        assert!(matches!(
            error,
            TeslaMateReaderError::SourceReadBudgetExceeded {
                limit: SourceBudgetLimit::Rows,
                maximum: LIVE_ROW_BUDGET,
                ..
            }
        ));

        // A caller's `?` drops the reader on the error. A server blocked
        // writing to a client that no longer reads cannot act on a cancel, so
        // the driver has to be free to discard the rest.
        drop(copy);
        assert_backend_stops_producing(&observer, pid).await;
        let one: i32 = client.query_one("SELECT 1", &[]).await.unwrap().get(0);
        assert_eq!(one, 1);
    })
    .await;
}

#[tokio::test]
#[ignore = "needs PostgreSQL: set TESLATLAS_HUB_BOUNDED_READ_TEST_POSTGRES_URL (run with --ignored)"]
async fn live_rows_inside_every_budget_are_returned_and_each_budget_is_enforced() {
    within_live_limit(async {
        let client = live_client().await;
        let series = "SELECT g::int AS value FROM generate_series(1, 3) AS g";

        // The session carries the statement timeout that bounds a broken run.
        let timeout_setting: String = client
            .query_one("SHOW statement_timeout", &[])
            .await
            .expect("read the statement timeout")
            .get(0);
        assert_eq!(timeout_setting, LIVE_STATEMENT_TIMEOUT);

        let rows = query_bounded(
            &client,
            series,
            &[],
            "states",
            SourceReadBudget::new(3, 1024),
        )
        .await
        .expect("three rows inside the budget");
        let values: Vec<i32> = rows.iter().map(|row| row.get(0)).collect();
        assert_eq!(values, vec![1, 2, 3]);

        let rows = query_bounded(
            &client,
            series,
            &[],
            "states",
            SourceReadBudget::new(2, 1024),
        )
        .await
        .expect_err("a third row is over a two row budget");
        assert!(matches!(
            rows,
            TeslaMateReaderError::SourceReadBudgetExceeded {
                limit: SourceBudgetLimit::Rows,
                maximum: 2,
                ..
            }
        ));

        // One 5000 byte value against a 1000 byte page, and so per-row, budget.
        let wide = query_bounded(
            &client,
            "SELECT repeat('x', 5000) AS padding",
            &[],
            "states",
            SourceReadBudget::new(1, 1000),
        )
        .await
        .expect_err("one wide row is over the byte budget");
        assert!(matches!(
            wide,
            TeslaMateReaderError::SourceReadBudgetExceeded {
                limit: SourceBudgetLimit::RowBytes,
                maximum: 1000,
                ..
            }
        ));

        // The same three rows through COPY, then over the page byte budget:
        // each INT8 row is 2 + 4 + 8 = 14 bytes, so 42 bytes fit 42 but not 41.
        let copy_sql = "COPY (SELECT g::bigint FROM generate_series(1, 3) AS g) \
                        TO STDOUT WITH (FORMAT BINARY)";
        let mut copy = open_bounded_copy(
            &client,
            copy_sql,
            LIVE_INT8,
            "states",
            SourceReadBudget::new(3, 42),
        )
        .await
        .expect("open the copy");
        let mut values = Vec::new();
        while let Some(row) = copy.next_row().await.expect("rows inside the budget") {
            values.push(row.get::<i64>(0));
        }
        assert_eq!(values, vec![1, 2, 3]);

        let mut copy = open_bounded_copy(
            &client,
            copy_sql,
            LIVE_INT8,
            "states",
            SourceReadBudget::new(3, 41),
        )
        .await
        .expect("open the copy");
        let mut count = 0;
        let error = loop {
            match copy.next_row().await {
                Ok(Some(_)) => count += 1,
                Ok(None) => panic!("the third row is over a 41 byte page"),
                Err(error) => break error,
            }
        };
        assert_eq!(count, 2);
        assert!(matches!(
            error,
            TeslaMateReaderError::SourceReadBudgetExceeded {
                limit: SourceBudgetLimit::PageBytes,
                maximum: 41,
                ..
            }
        ));
    })
    .await;
}
