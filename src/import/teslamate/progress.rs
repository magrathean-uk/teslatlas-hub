// SPDX-License-Identifier: AGPL-3.0-only

//! Machine-readable TeslaMate migration progress.

use std::sync::{Arc, Mutex};

use serde::Serialize;

/// Non-secret phase label attached to machine-readable migration progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TeslaMateMigrationPhase {
    Counting,
    Metadata,
    RelatedPositions,
    Positions,
    Charges,
    Finalizing,
    Complete,
}

/// One line-safe migration progress record. The migration CLI serializes this
/// as NDJSON while retaining its existing final result object unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeslaMateMigrationProgressEvent {
    event: &'static str,
    pub completed_rows: u64,
    pub total_rows: u64,
    pub phase: TeslaMateMigrationPhase,
}

impl TeslaMateMigrationProgressEvent {
    pub fn new(phase: TeslaMateMigrationPhase, completed_rows: u64, total_rows: u64) -> Self {
        Self {
            event: "migration_progress",
            completed_rows,
            total_rows,
            phase,
        }
    }
}

#[derive(Debug, Default)]
struct TeslaMateMigrationProgressState {
    completed_rows: u64,
    source_total_rows: Option<u64>,
    progress_total_rows: Option<u64>,
}

const FINALIZATION_PROGRESS_DIVISOR: u64 = 4;

fn progress_total_rows(source_total_rows: u64) -> u64 {
    let finalization_rows = (source_total_rows / FINALIZATION_PROGRESS_DIVISOR).max(1);
    source_total_rows.saturating_add(finalization_rows)
}

struct TeslaMateMigrationProgressReporterInner {
    state: Mutex<TeslaMateMigrationProgressState>,
    callback: Box<dyn Fn(TeslaMateMigrationProgressEvent) + Send + Sync>,
}

/// Cloneable optional progress sink shared by the capture and finalization
/// layers. It clamps retry regressions so every emitted row count is monotonic.
#[derive(Clone, Default)]
pub struct TeslaMateMigrationProgressReporter {
    inner: Option<Arc<TeslaMateMigrationProgressReporterInner>>,
}

impl TeslaMateMigrationProgressReporter {
    pub fn new<F>(callback: F) -> Self
    where
        F: Fn(TeslaMateMigrationProgressEvent) + Send + Sync + 'static,
    {
        Self {
            inner: Some(Arc::new(TeslaMateMigrationProgressReporterInner {
                state: Mutex::new(TeslaMateMigrationProgressState::default()),
                callback: Box::new(callback),
            })),
        }
    }

    pub fn report(&self, phase: TeslaMateMigrationPhase, completed_rows: u64, total_rows: u64) {
        if total_rows == 0 {
            return;
        }
        let Some(inner) = &self.inner else {
            return;
        };
        let event = {
            let mut state = inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state
                .source_total_rows
                .is_some_and(|established| established != total_rows)
            {
                tracing::warn!(
                    established_total_rows = state.source_total_rows,
                    rejected_total_rows = total_rows,
                    "ignored inconsistent TeslaMate migration progress total"
                );
                return;
            }
            state.source_total_rows = Some(total_rows);
            let progress_total_rows = progress_total_rows(total_rows);
            state.progress_total_rows = Some(progress_total_rows);
            let maximum_incomplete_rows = progress_total_rows.saturating_sub(1);
            state.completed_rows = state
                .completed_rows
                .max(completed_rows.min(maximum_incomplete_rows));
            TeslaMateMigrationProgressEvent::new(phase, state.completed_rows, progress_total_rows)
        };
        (inner.callback)(event);
    }

    pub fn transition(&self, phase: TeslaMateMigrationPhase) {
        let Some(inner) = &self.inner else {
            return;
        };
        let event = {
            let state = inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(total_rows) = state.progress_total_rows else {
                return;
            };
            TeslaMateMigrationProgressEvent::new(phase, state.completed_rows, total_rows)
        };
        (inner.callback)(event);
    }

    pub fn advance_finalizing(&self, numerator: u64, denominator: u64) {
        if denominator == 0 {
            return;
        }
        let Some(inner) = &self.inner else {
            return;
        };
        let event = {
            let mut state = inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (Some(source_total_rows), Some(progress_total_rows)) =
                (state.source_total_rows, state.progress_total_rows)
            else {
                return;
            };
            let finalization_rows = progress_total_rows.saturating_sub(source_total_rows);
            let bounded_numerator = numerator.min(denominator);
            let advanced = ((u128::from(finalization_rows) * u128::from(bounded_numerator))
                / u128::from(denominator)) as u64;
            let target = source_total_rows
                .saturating_add(advanced)
                .min(progress_total_rows.saturating_sub(1));
            state.completed_rows = state.completed_rows.max(target);
            TeslaMateMigrationProgressEvent::new(
                TeslaMateMigrationPhase::Finalizing,
                state.completed_rows,
                progress_total_rows,
            )
        };
        (inner.callback)(event);
    }

    pub fn complete(&self, phase: TeslaMateMigrationPhase) {
        let Some(inner) = &self.inner else {
            return;
        };
        let event = {
            let mut state = inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(total_rows) = state.progress_total_rows else {
                return;
            };
            state.completed_rows = total_rows;
            TeslaMateMigrationProgressEvent::new(phase, total_rows, total_rows)
        };
        (inner.callback)(event);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn event_has_stable_ndjson_shape() {
        let event =
            TeslaMateMigrationProgressEvent::new(TeslaMateMigrationPhase::Positions, 123, 456);

        assert_eq!(
            serde_json::to_string(&event).expect("progress JSON"),
            r#"{"event":"migration_progress","completedRows":123,"totalRows":456,"phase":"positions"}"#
        );
    }

    #[test]
    fn reporter_keeps_completed_rows_monotonic_and_finishes_at_total() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&observed);
        let reporter = TeslaMateMigrationProgressReporter::new(move |event| {
            captured.lock().expect("progress capture").push(event);
        });

        reporter.report(TeslaMateMigrationPhase::Positions, 456, 456);
        reporter.report(TeslaMateMigrationPhase::RelatedPositions, 10, 456);
        reporter.transition(TeslaMateMigrationPhase::Finalizing);
        reporter.advance_finalizing(1, 2);
        reporter.complete(TeslaMateMigrationPhase::Complete);

        let observed = observed.lock().expect("progress capture");
        assert_eq!(observed.len(), 5);
        assert_eq!(observed[0].completed_rows, 456);
        assert_eq!(observed[1].completed_rows, 456);
        assert_eq!(observed[2].phase, TeslaMateMigrationPhase::Finalizing);
        assert_eq!(observed[2].completed_rows * 5, observed[2].total_rows * 4);
        assert_eq!(observed[3].phase, TeslaMateMigrationPhase::Finalizing);
        assert!(observed[3].completed_rows > observed[2].completed_rows);
        assert!(observed[3].completed_rows < observed[3].total_rows);
        assert_eq!(observed[4].phase, TeslaMateMigrationPhase::Complete);
        assert_eq!(observed[4].completed_rows, observed[4].total_rows);
        assert!(
            observed
                .iter()
                .all(|event| event.total_rows == observed[0].total_rows)
        );
    }
}
