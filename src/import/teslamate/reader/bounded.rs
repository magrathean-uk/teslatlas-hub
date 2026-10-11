// SPDX-License-Identifier: AGPL-3.0-only

// The shared bounded reader. Every PostgreSQL read in `import::teslamate` that
// collects rows into memory streams them through here, so the decode budgets
// documented on `TeslaMateReadLimits` are checked before a row is retained and
// the first excess ends the read with a typed error after the server has been
// asked to stop. The driver buffers one DataRow before it reaches this code:
// the budgets bound retention and the cumulative size of a read, not that
// transient buffer.
//
// All admission logic lives in the generic `BoundedRows`, which a mock stream
// and a mock canceller exercise in `bounded_tests.rs`. What is specific to
// PostgreSQL is only `query_bounded`, `open_bounded_copy`, `ServerCancel` and
// the `WireFields` impl for `BinaryCopyOutRow`.

/// The row, per-row and per-page budgets of one bounded source read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceReadBudget {
    max_rows: usize,
    row_bytes: usize,
    page_bytes: usize,
}

impl SourceReadBudget {
    /// At most `max_rows` rows and `page_bytes` bytes in total; one row may be
    /// at most `min(page_bytes, 16 MiB)`.
    pub(crate) const fn new(max_rows: usize, page_bytes: usize) -> Self {
        let row_bytes = if page_bytes < MAX_SOURCE_ROW_BYTES {
            page_bytes
        } else {
            MAX_SOURCE_ROW_BYTES
        };
        Self {
            max_rows,
            row_bytes,
            page_bytes,
        }
    }

    /// A read with no `TeslaMateReadLimits` in reach (the schema probes and the
    /// token reads): the page byte budget is the 64 MiB ceiling.
    pub(crate) const fn fixed(max_rows: usize) -> Self {
        Self::new(max_rows, MAX_SOURCE_PAGE_BYTES)
    }

    /// Allow one lookahead row beyond the retained rows, for a caller that
    /// reads one row more than it keeps to detect ambiguity (`LIMIT 2`).
    pub(crate) const fn with_lookahead(self) -> Self {
        Self {
            max_rows: self.max_rows.saturating_add(1),
            ..self
        }
    }
}

/// The size of one source row as the budgets measure it.
pub(crate) trait SourceRow {
    fn source_bytes(&self) -> usize;
}

impl SourceRow for Row {
    fn source_bytes(&self) -> usize {
        self.raw_size_bytes()
    }
}

/// Running totals of one bounded read. `admit` is the only place a budget is
/// enforced; it leaves the totals untouched when it refuses a row.
#[derive(Debug)]
pub(crate) struct SourceReadMeter {
    table: &'static str,
    budget: SourceReadBudget,
    rows: usize,
    bytes: usize,
}

impl SourceReadMeter {
    pub(crate) const fn new(table: &'static str, budget: SourceReadBudget) -> Self {
        Self {
            table,
            budget,
            rows: 0,
            bytes: 0,
        }
    }

    /// Account for one more row of `row_bytes` bytes, or refuse it with the
    /// first budget it exceeds. The caller retains the row only after `Ok`.
    pub(crate) fn admit(&mut self, row_bytes: usize) -> Result<(), TeslaMateReaderError> {
        if self.rows >= self.budget.max_rows {
            return Err(self.exceeded(SourceBudgetLimit::Rows, self.budget.max_rows));
        }
        if row_bytes > self.budget.row_bytes {
            return Err(self.exceeded(SourceBudgetLimit::RowBytes, self.budget.row_bytes));
        }
        let bytes = self
            .bytes
            .checked_add(row_bytes)
            .filter(|bytes| *bytes <= self.budget.page_bytes)
            .ok_or_else(|| self.exceeded(SourceBudgetLimit::PageBytes, self.budget.page_bytes))?;
        self.rows += 1;
        self.bytes = bytes;
        Ok(())
    }

    fn exceeded(&self, limit: SourceBudgetLimit, maximum: usize) -> TeslaMateReaderError {
        TeslaMateReaderError::SourceReadBudgetExceeded {
            table: self.table,
            limit,
            maximum,
        }
    }
}

/// Stops the server-side work behind a read that went over a budget.
pub(crate) trait SourceCancel {
    /// Ask the server to stop the statement behind the read in progress, and
    /// return once the request has been delivered or has failed. Best effort:
    /// the read is over either way, so a failure is not an error.
    async fn cancel(&self);
}

/// Cancels through the PostgreSQL cancel request of the read's own connection.
/// Dropping a `RowStream` or `CopyOutStream` does not stop the statement: the
/// driver keeps reading and discarding its output until the server is done.
pub(crate) struct ServerCancel {
    token: CancelToken,
}

impl ServerCancel {
    pub(crate) fn of(client: &Client) -> Self {
        Self {
            token: client.cancel_token(),
        }
    }
}

impl SourceCancel for ServerCancel {
    async fn cancel(&self) {
        // The token carries the connection's own SSL mode: a plaintext
        // loopback connection never uses the connector, and any other one
        // verifies the server against the native roots like the connection did.
        crate::crypto::install_default_provider();
        let sent = match MakeRustlsConnect::with_native_certs() {
            Ok((tls, _)) => timeout(SOURCE_CANCEL_TIMEOUT, self.token.cancel_query(tls)).await,
            Err(_) => timeout(SOURCE_CANCEL_TIMEOUT, self.token.cancel_query(NoTls)).await,
        };
        if !matches!(sent, Ok(Ok(()))) {
            tracing::warn!("the source query cancel request did not reach the server");
        }
    }
}

/// A stream of rows that are measured against a budget before the caller sees
/// them. On the first excess it asks `cancel` to stop the server, then ends
/// the read with the typed budget error; the excess row is never returned, and
/// the stream is not polled again.
pub(crate) struct BoundedRows<S, C> {
    rows: Pin<Box<S>>,
    meter: SourceReadMeter,
    cancel: C,
}

impl<S, R, E, C> BoundedRows<S, C>
where
    S: futures_util::Stream<Item = Result<R, E>>,
    R: SourceRow,
    TeslaMateReaderError: From<E>,
    C: SourceCancel,
{
    pub(crate) fn new(stream: S, table: &'static str, budget: SourceReadBudget, cancel: C) -> Self {
        Self {
            rows: Box::pin(stream),
            meter: SourceReadMeter::new(table, budget),
            cancel,
        }
    }

    /// The next admitted row, or `None` at the end of the stream.
    pub(crate) async fn next_row(&mut self) -> Result<Option<R>, TeslaMateReaderError> {
        let Some(row) = self.rows.as_mut().try_next().await? else {
            return Ok(None);
        };
        if let Err(excess) = self.meter.admit(row.source_bytes()) {
            self.cancel.cancel().await;
            return Err(excess);
        }
        Ok(Some(row))
    }
}

/// Hand each row of `stream` to `retain` once the budgets admit it. On the
/// first excess the server is asked to stop (`cancel`), the stream is not
/// polled again and the typed budget error is returned; the excess row is
/// never passed to `retain`.
pub(crate) async fn retain_bounded_rows<S, R, E, C, F>(
    stream: S,
    table: &'static str,
    budget: SourceReadBudget,
    cancel: C,
    mut retain: F,
) -> Result<(), TeslaMateReaderError>
where
    S: futures_util::Stream<Item = Result<R, E>>,
    R: SourceRow,
    TeslaMateReaderError: From<E>,
    C: SourceCancel,
    F: FnMut(R) -> Result<(), TeslaMateReaderError>,
{
    let mut rows = BoundedRows::new(stream, table, budget, cancel);
    while let Some(row) = rows.next_row().await? {
        retain(row)?;
    }
    Ok(())
}

/// Collect the rows of `stream` within `budget`; see [`retain_bounded_rows`].
pub(crate) async fn collect_bounded_rows<S, R, E, C>(
    stream: S,
    table: &'static str,
    budget: SourceReadBudget,
    cancel: C,
) -> Result<Vec<R>, TeslaMateReaderError>
where
    S: futures_util::Stream<Item = Result<R, E>>,
    R: SourceRow,
    TeslaMateReaderError: From<E>,
    C: SourceCancel,
{
    let mut rows = Vec::new();
    retain_bounded_rows(stream, table, budget, cancel, |row| {
        rows.push(row);
        Ok(())
    })
    .await?;
    Ok(rows)
}

/// Run `sql` and collect its rows within `budget`. This replaces
/// `Client::query`, which buffers every row before the caller can look at it.
pub(crate) async fn query_bounded(
    client: &Client,
    sql: &str,
    params: &[&(dyn ToSql + Sync)],
    table: &'static str,
    budget: SourceReadBudget,
) -> Result<Vec<Row>, TeslaMateReaderError> {
    let stream = client.query_raw(sql, params.iter().copied()).await?;
    collect_bounded_rows(stream, table, budget, ServerCancel::of(client)).await
}

/// How a COPY row is measured: the byte length of each of its fields on the
/// wire, `None` for SQL NULL.
pub(crate) trait WireFields {
    fn field_length(&self, column: usize) -> Option<usize>;
}

impl WireFields for BinaryCopyOutRow {
    fn field_length(&self, column: usize) -> Option<usize> {
        match self.try_get::<Option<RawField<'_>>>(column) {
            Ok(field) => field.map(|field| field.0.len()),
            // Unreachable for a column within the declared types; if it ever
            // fails the row counts as maximal, so the budget refuses it.
            Err(_) => Some(usize::MAX),
        }
    }
}

/// A COPY row of `columns` fields, sized from its field lengths.
pub(crate) struct CopyRow<T> {
    row: T,
    columns: usize,
}

impl<T: WireFields> SourceRow for CopyRow<T> {
    fn source_bytes(&self) -> usize {
        wire_row_bytes((0..self.columns).map(|column| self.row.field_length(column)))
    }
}

/// The rows of a COPY stream, each tagged with its column count so that
/// [`CopyRow`] can measure it.
pub(crate) struct CopyRowStream<S> {
    rows: Pin<Box<S>>,
    columns: usize,
}

impl<S> CopyRowStream<S> {
    /// Tag the rows of `rows` with the number of columns `types` declares.
    /// The column count is derived here, in code the mock-stream tests run,
    /// and not at the PostgreSQL call site.
    pub(crate) fn new(rows: S, types: &[Type]) -> Self {
        Self {
            rows: Box::pin(rows),
            columns: types.len(),
        }
    }
}

impl<S, T, E> futures_util::Stream for CopyRowStream<S>
where
    S: futures_util::Stream<Item = Result<T, E>>,
{
    type Item = Result<CopyRow<T>, E>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let columns = self.columns;
        self.rows
            .as_mut()
            .poll_next(context)
            .map(|item| item.map(|row| row.map(|row| CopyRow { row, columns })))
    }
}

/// A binary COPY stream whose rows are measured against a budget before the
/// caller sees them. The caller still applies `retain_row` for the global row
/// cap.
pub(crate) struct BoundedCopyRows {
    rows:
        BoundedRows<CopyRowStream<tokio_postgres::binary_copy::BinaryCopyOutStream>, ServerCancel>,
}

/// Open a `COPY ... TO STDOUT (FORMAT BINARY)` read of `types` columns.
pub(crate) async fn open_bounded_copy(
    client: &Client,
    sql: &str,
    types: &'static [Type],
    table: &'static str,
    budget: SourceReadBudget,
) -> Result<BoundedCopyRows, TeslaMateReaderError> {
    let stream = client.copy_out(sql).await?;
    let rows = CopyRowStream::new(
        tokio_postgres::binary_copy::BinaryCopyOutStream::new(stream, types),
        types,
    );
    Ok(BoundedCopyRows {
        rows: BoundedRows::new(rows, table, budget, ServerCancel::of(client)),
    })
}

impl BoundedCopyRows {
    /// The next row, or `None` at the end of the copy. A row over a budget
    /// stops the server and is returned as the typed error; it never reaches
    /// the caller.
    pub(crate) async fn next_row(
        &mut self,
    ) -> Result<Option<BinaryCopyOutRow>, TeslaMateReaderError> {
        Ok(self.rows.next_row().await?.map(|copy| copy.row))
    }
}

/// Row limit of a COPY read: one more than the rows still allowed under
/// `maximum_rows`, so the server stops one row past the cap and `retain_row`
/// fails on it.
pub(crate) fn copy_row_limit(limits: TeslaMateReadLimits, retained_rows: usize) -> i64 {
    let remaining = limits.maximum_rows.saturating_sub(retained_rows);
    i64::try_from(remaining.saturating_add(1)).unwrap_or(i64::MAX)
}

/// A column value as raw wire bytes, whatever its PostgreSQL type; used only to
/// measure a COPY row.
struct RawField<'a>(&'a [u8]);

impl<'a> FromSql<'a> for RawField<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self(raw))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

/// Approximate wire size of a row: the field count, then one four-byte length
/// per field plus the field bytes (`None` is SQL NULL and has no bytes).
fn wire_row_bytes(field_lengths: impl IntoIterator<Item = Option<usize>>) -> usize {
    field_lengths.into_iter().fold(2_usize, |total, length| {
        total.saturating_add(4).saturating_add(length.unwrap_or(0))
    })
}
