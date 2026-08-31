use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::spool::SpoolSnapshot;

#[derive(Default)]
pub(crate) struct Metrics {
    admitted: AtomicU64,
    duplicates: AtomicU64,
    rejected_auth: AtomicU64,
    rejected_invalid: AtomicU64,
    rejected_capacity: AtomicU64,
    rejected_storage: AtomicU64,
    rejected_internal: AtomicU64,
}

impl Metrics {
    pub(crate) fn admitted(&self) {
        self.admitted.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn duplicate(&self) {
        self.duplicates.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn rejected_auth(&self) {
        self.rejected_auth.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn rejected_invalid(&self) {
        self.rejected_invalid.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn rejected_capacity(&self) {
        self.rejected_capacity.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn rejected_storage(&self) {
        self.rejected_storage.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn rejected_internal(&self) {
        self.rejected_internal.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn render(&self, spool: &SpoolSnapshot) -> String {
        let mut output = String::with_capacity(2_048);
        gauge(&mut output, "teslatlas_edge_build_info", 1);
        gauge(
            &mut output,
            "teslatlas_edge_spool_records",
            u64::try_from(spool.pending_records).unwrap_or(u64::MAX),
        );
        gauge(
            &mut output,
            "teslatlas_edge_spool_bytes",
            spool.pending_bytes,
        );
        gauge(
            &mut output,
            "teslatlas_edge_spool_oldest_age_seconds",
            spool.oldest_age_seconds,
        );
        gauge(
            &mut output,
            "teslatlas_edge_spool_corrupt_records",
            spool.corrupt_records,
        );
        counter(
            &mut output,
            "teslatlas_edge_spool_expired_records_total",
            spool.expired_records,
        );
        gauge(
            &mut output,
            "teslatlas_edge_spool_gap_notices",
            u64::try_from(spool.pending_gap_notices).unwrap_or(u64::MAX),
        );
        gauge(
            &mut output,
            "teslatlas_edge_spool_gap_bytes",
            spool.pending_gap_bytes,
        );
        counter(
            &mut output,
            "teslatlas_edge_receiver_admitted_total",
            self.admitted.load(Ordering::Relaxed),
        );
        counter(
            &mut output,
            "teslatlas_edge_receiver_duplicates_total",
            self.duplicates.load(Ordering::Relaxed),
        );
        counter(
            &mut output,
            "teslatlas_edge_receiver_rejected_auth_total",
            self.rejected_auth.load(Ordering::Relaxed),
        );
        counter(
            &mut output,
            "teslatlas_edge_receiver_rejected_invalid_total",
            self.rejected_invalid.load(Ordering::Relaxed),
        );
        counter(
            &mut output,
            "teslatlas_edge_receiver_rejected_capacity_total",
            self.rejected_capacity.load(Ordering::Relaxed),
        );
        counter(
            &mut output,
            "teslatlas_edge_receiver_rejected_storage_total",
            self.rejected_storage.load(Ordering::Relaxed),
        );
        counter(
            &mut output,
            "teslatlas_edge_receiver_rejected_internal_total",
            self.rejected_internal.load(Ordering::Relaxed),
        );
        output
    }
}

fn gauge(output: &mut String, name: &str, value: u64) {
    typed_metric(output, name, "gauge", value);
}

fn counter(output: &mut String, name: &str, value: u64) {
    typed_metric(output, name, "counter", value);
}

fn typed_metric(output: &mut String, name: &str, metric_type: &str, value: u64) {
    let _ = writeln!(output, "# TYPE {name} {metric_type}");
    let _ = writeln!(output, "{name} {value}");
}
