// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic process-fault witnesses compiled only for explicit acceptance tests.

use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

static PUBLICATION_FAILED: AtomicBool = AtomicBool::new(false);
static RETURNED_ERROR_WITNESSED: AtomicBool = AtomicBool::new(false);

fn write_witness(path_variable: &str, value: &str) {
    let path = std::env::var_os(path_variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .expect("Edge acceptance fault witness path");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("create Edge acceptance fault witness");
    file.write_all(value.as_bytes())
        .expect("write Edge acceptance fault witness");
    file.sync_all().expect("sync Edge acceptance fault witness");
}

pub(crate) fn abort_at(point: &str) {
    if std::env::var("TESLATLAS_EDGE_TEST_ABORT_POINT").as_deref() == Ok(point) {
        write_witness("TESLATLAS_EDGE_TEST_WITNESS_PATH", point);
        std::process::abort();
    }
}

pub(crate) fn fail_publication_once() -> bool {
    if std::env::var_os("TESLATLAS_EDGE_TEST_FAIL_PUBLICATION_ONCE").is_some()
        && !PUBLICATION_FAILED.swap(true, Ordering::SeqCst)
    {
        write_witness(
            "TESLATLAS_EDGE_TEST_PUBLICATION_WITNESS_PATH",
            "publication_failed",
        );
        true
    } else {
        false
    }
}

pub(crate) fn return_error_at(point: &str) -> bool {
    if std::env::var("TESLATLAS_EDGE_TEST_RETURN_ERROR_AT").as_deref() != Ok(point) {
        return false;
    }
    if !RETURNED_ERROR_WITNESSED.swap(true, Ordering::SeqCst) {
        write_witness("TESLATLAS_EDGE_TEST_RETURN_ERROR_WITNESS_PATH", point);
    }
    true
}
