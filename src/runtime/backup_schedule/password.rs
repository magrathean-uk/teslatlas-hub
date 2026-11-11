// SPDX-License-Identifier: AGPL-3.0-only

//! Reads the recovery password from the owned file the configuration (or the
//! manual command) names. The password never appears in a configuration, a
//! request, a response or a log; the service reads it at each run and wipes
//! it afterwards.

use std::{
    fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
};

use rustix::{
    fs::{FileType, Mode, OFlags, fcntl_getfl, fcntl_setfl, fstat, open},
    io::Errno,
    process::getuid,
};
use zeroize::Zeroizing;

use crate::runtime::operator::admission::OperatorRefusal;

/// Longest password file the service reads.
pub const PASSWORD_FILE_MAX_BYTES: usize = 1024;

pub const PASSWORD_FILE_INVALID: &str = "backup_password_file_invalid";

fn invalid(message: &'static str) -> OperatorRefusal {
    OperatorRefusal::new(PASSWORD_FILE_INVALID, message)
}

/// Reads the password: an absolute path to a regular file owned by the
/// service's user, mode 0600 or tighter, not a symbolic link, unchanged
/// while it is read, holding UTF-8 of at most 1 KiB. One trailing line end
/// is ignored.
pub fn read_password_file(path: &Path) -> Result<Zeroizing<String>, OperatorRefusal> {
    if !path.is_absolute() {
        return Err(invalid("the backup password file is named by an absolute path"));
    }
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| match error {
        Errno::LOOP => invalid("the backup password file must not be a symbolic link"),
        Errno::NOENT => invalid("the backup password file does not exist"),
        _ => invalid("the backup password file cannot be opened"),
    })?;
    let before = fstat(&descriptor).map_err(|_| invalid("the backup password file cannot be read"))?;
    if !FileType::from_raw_mode(before.st_mode).is_file()
        || before.st_uid != getuid().as_raw()
        || (before.st_mode & 0o077) != 0
        || (before.st_mode & 0o400) == 0
    {
        return Err(invalid(
            "the backup password file must be a regular file you own, readable by you alone (mode 0600)",
        ));
    }
    let flags = fcntl_getfl(&descriptor).map_err(|_| invalid("the backup password file cannot be read"))?;
    fcntl_setfl(&descriptor, flags & !OFlags::NONBLOCK)
        .map_err(|_| invalid("the backup password file cannot be read"))?;
    let file: fs::File = descriptor.into();
    let mut bytes = Zeroizing::new(Vec::with_capacity(PASSWORD_FILE_MAX_BYTES + 1));
    (&file)
        .take(u64::try_from(PASSWORD_FILE_MAX_BYTES + 1).expect("cap fits"))
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("the backup password file cannot be read"))?;
    if bytes.len() > PASSWORD_FILE_MAX_BYTES {
        return Err(invalid("the backup password file is larger than 1 KiB"));
    }
    let after = file
        .metadata()
        .map_err(|_| invalid("the backup password file cannot be read"))?;
    if after.dev() != u64::try_from(before.st_dev).unwrap_or(u64::MAX)
        || after.ino() != before.st_ino
        || after.len() != u64::try_from(before.st_size).unwrap_or(u64::MAX)
    {
        return Err(invalid("the backup password file changed while it was read"));
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| invalid("the backup password file must hold UTF-8 text"))?;
    Ok(Zeroizing::new(text.to_owned()))
}
