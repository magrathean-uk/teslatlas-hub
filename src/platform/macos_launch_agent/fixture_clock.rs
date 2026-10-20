// SPDX-License-Identifier: AGPL-3.0-only

//! The clock a Fixture-mode source run serves the current summary at (C6 R28
//! (c)). The interop fixture seeds observations at fixed past times; the
//! fixture itself records the time its values are measured at, in a marker
//! file ([`FIXTURE_SUMMARY_CLOCK_FILE`]) inside its own data directory. Only the
//! Fixture development mode reads it; it is not a configuration key and no other
//! mode, nor production, can be given a pinned clock this way. This module is
//! macOS only, like every source run; the marker's file name is defined in the
//! always-compiled server module so the fixture seed builds on every target.

use super::DevelopmentServeMode;
use crate::server::FIXTURE_SUMMARY_CLOCK_FILE;
use rustix::{
    fs::{Mode, OFlags, open},
    io::Errno,
    process::getuid,
};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::Path,
};

/// Longest marker the reader takes in: 32 decimal digits and one newline.
const MAX_MARKER_BYTES: usize = 33;
const MAX_MARKER_DIGITS: usize = MAX_MARKER_BYTES - 1;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// The pinned summary clock of a source run. `None` for production admission
/// and for the Standalone and PrivateLan modes, whatever their data directory
/// holds, and for a Fixture directory without a marker (system clock). A marker
/// that is not an owner-private regular file holding one positive decimal
/// integer (ASCII digits and at most one trailing newline) is an error, never a
/// silent fall back to the system clock. The file is opened without following a
/// link and its owner, type and mode are read from the opened handle.
pub fn fixture_summary_now_ms(
    development_mode: Option<DevelopmentServeMode>,
    data_dir: &Path,
) -> io::Result<Option<i64>> {
    if development_mode != Some(DevelopmentServeMode::Fixture) {
        return Ok(None);
    }
    let descriptor = match open(
        data_dir.join(FIXTURE_SUMMARY_CLOCK_FILE),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP) => {
            return Err(invalid(
                "fixture summary clock marker must be an owner-private regular file",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let file = fs::File::from(descriptor);
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != getuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(invalid(
            "fixture summary clock marker must be an owner-private regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MARKER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    // The read above is bounded one byte past the limit, so an over-long marker
    // has at least 33 digits and fails the digit bound; an empty one fails the
    // parse below.
    let digits = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    if digits.len() > MAX_MARKER_DIGITS || !digits.iter().all(u8::is_ascii_digit) {
        return Err(invalid(
            "fixture summary clock marker must hold one positive epoch-millisecond integer",
        ));
    }
    std::str::from_utf8(digits)
        .ok()
        .and_then(|text| text.parse::<i64>().ok())
        .filter(|ms| *ms > 0)
        .map(Some)
        .ok_or_else(|| {
            invalid("fixture summary clock marker must hold one positive epoch-millisecond integer")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    fn write_marker(dir: &Path, content: &[u8], mode: u32) {
        use std::io::Write;
        let path = dir.join(FIXTURE_SUMMARY_CLOCK_FILE);
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&path)
            .unwrap()
            .write_all(content)
            .unwrap();
        // The creation mode is masked by the umask; state the mode exactly.
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn only_fixture_mode_reads_the_marker() {
        let temporary = crate::private_tempdir().unwrap();
        write_marker(temporary.path(), b"1788566460000\n", 0o600);
        for (mode, expected) in [
            (Some(DevelopmentServeMode::Fixture), Some(1_788_566_460_000)),
            (Some(DevelopmentServeMode::Standalone), None),
            (Some(DevelopmentServeMode::PrivateLan), None),
            (None, None),
        ] {
            assert_eq!(
                fixture_summary_now_ms(mode, temporary.path()).unwrap(),
                expected,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_fixture_without_a_marker_keeps_the_system_clock() {
        let temporary = crate::private_tempdir().unwrap();
        assert_eq!(
            fixture_summary_now_ms(Some(DevelopmentServeMode::Fixture), temporary.path()).unwrap(),
            None
        );
    }

    #[test]
    fn a_bad_marker_is_refused_not_ignored() {
        for (content, mode) in [
            (&b""[..], 0o600),
            (b"0\n", 0o600),
            (b"-5\n", 0o600),
            (b"soon\n", 0o600),
            (b"1788566460000 \n", 0o600),
            (b"+1788566460000\n", 0o600),
            (b"1788566460000\n\n", 0o600),
            (b"\n", 0o600),
            (b"1788566460000\r\n", 0o600),
            (b"1788566460000 1\n", 0o600),
            (b"1788566460000\n", 0o644),
            (&[b'7'; 40][..], 0o600),
            // 33 digits, with and without the newline: over the digit bound.
            (b"000000000000000000001788566460000", 0o600),
            (b"000000000000000000001788566460000\n", 0o600),
            (b"1788566460000\n", 0o640),
            (b"1788566460000\n", 0o604),
        ] {
            let temporary = crate::private_tempdir().unwrap();
            write_marker(temporary.path(), content, mode);
            let error =
                fixture_summary_now_ms(Some(DevelopmentServeMode::Fixture), temporary.path())
                    .unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::InvalidData,
                "{content:?} {mode:o}"
            );
        }
        // A directory at the marker path is refused by the handle's type check,
        // not left to fail later in the read.
        let temporary = crate::private_tempdir().unwrap();
        fs::create_dir(temporary.path().join(FIXTURE_SUMMARY_CLOCK_FILE)).unwrap();
        fs::set_permissions(
            temporary.path().join(FIXTURE_SUMMARY_CLOCK_FILE),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert_eq!(
            fixture_summary_now_ms(Some(DevelopmentServeMode::Fixture), temporary.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let temporary = crate::private_tempdir().unwrap();
        let target = temporary.path().join("elsewhere");
        fs::write(&target, b"1788566460000\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&target, temporary.path().join(FIXTURE_SUMMARY_CLOCK_FILE))
            .unwrap();
        assert!(
            fixture_summary_now_ms(Some(DevelopmentServeMode::Fixture), temporary.path()).is_err()
        );
    }

    #[test]
    fn the_marker_is_read_up_to_its_byte_bound() {
        // 32 digits and a newline are 33 bytes; leading zeros are digits.
        let temporary = crate::private_tempdir().unwrap();
        write_marker(
            temporary.path(),
            format!("{:0>32}\n", 1_788_566_460_000_i64).as_bytes(),
            0o600,
        );
        let marker = fs::read(temporary.path().join(FIXTURE_SUMMARY_CLOCK_FILE)).unwrap();
        assert_eq!(marker.len(), MAX_MARKER_BYTES);
        assert_eq!(
            fixture_summary_now_ms(Some(DevelopmentServeMode::Fixture), temporary.path()).unwrap(),
            Some(1_788_566_460_000)
        );
        // A marker without its newline is the same value.
        let temporary = crate::private_tempdir().unwrap();
        write_marker(temporary.path(), b"1788566460000", 0o600);
        assert_eq!(
            fixture_summary_now_ms(Some(DevelopmentServeMode::Fixture), temporary.path()).unwrap(),
            Some(1_788_566_460_000)
        );
    }
}
