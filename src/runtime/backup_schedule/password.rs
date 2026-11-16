// SPDX-License-Identifier: AGPL-3.0-only

//! Reads the recovery password from the owned file the configuration (or the
//! manual command) names. The password never appears in a configuration, a
//! request, a response or a log; the service reads it at each run and wipes
//! it afterwards.
//!
//! Mode bits alone do not say who else can open a file: an access control
//! list can grant another account read or write on a mode-0600 file (REV-M06-T06
//! finding 6). The reader therefore refuses a file that carries any ACL entry
//! granting access: on Linux the `system.posix_acl_access` attribute beyond
//! its three base entries, on macOS any `allow` entry (macOS keeps the list
//! out of reach of the attribute calls, so `/bin/ls -led` lists it).

use std::{
    fs,
    io::Read,
    os::{fd::AsFd, unix::fs::MetadataExt},
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
    if acl_grants_access(&descriptor, path)? {
        return Err(invalid(
            "the backup password file has an access control list that lets another account open it; remove it (chmod -N on macOS, setfacl -b on Linux)",
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
    // What `#pw-file` recovery takes (REV-CX-M06-T08 finding 1): an empty
    // password could never be given back to `verify-backup` or `restore-data`.
    if bytes.is_empty() {
        return Err(invalid("the backup password file holds no password"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| invalid("the backup password file must hold UTF-8 text"))?;
    Ok(Zeroizing::new(text.to_owned()))
}

/// Whether the open file carries an ACL entry that grants access beyond the
/// mode bits. Fails closed: a list that cannot be read refuses the file.
#[cfg(target_os = "linux")]
fn acl_grants_access(descriptor: &impl AsFd, _path: &Path) -> Result<bool, OperatorRefusal> {
    let mut buffer = [0_u8; 4096];
    match rustix::fs::fgetxattr(descriptor, "system.posix_acl_access", &mut buffer[..]) {
        Ok(length) => Ok(posix_acl_grants_access(&buffer[..length])),
        Err(Errno::NODATA | Errno::NOTSUP) => Ok(false),
        Err(_) => Err(invalid("the backup password file's access control list cannot be read")),
    }
}

/// Lists the ACL of the opened file itself: `/.vol/<device>/<inode>` names
/// the held file, so a file swapped in at the pathname is never the one
/// inspected (REV-CX-M06-T12 finding 8; the pattern of M06-T11).
#[cfg(target_os = "macos")]
fn acl_grants_access(descriptor: &impl AsFd, path: &Path) -> Result<bool, OperatorRefusal> {
    use std::process::{Command, Stdio};
    #[cfg(not(test))]
    let _ = path;
    let unreadable = || invalid("the backup password file's access control list cannot be read");
    let open = fstat(descriptor).map_err(|_| unreadable())?;
    let held = format!("/.vol/{}/{}", open.st_dev, open.st_ino);
    #[cfg(test)]
    acl_listing_seam::run(acl_listing_seam::Stage::Before, path);
    let output = Command::new("/bin/ls")
        .arg("-l")
        .arg("-e")
        .arg("-d")
        .arg("--")
        .arg(&held)
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| unreadable())?;
    #[cfg(test)]
    acl_listing_seam::run(acl_listing_seam::Stage::After, path);
    if !output.status.success() {
        return Err(unreadable());
    }
    let listed = fs::symlink_metadata(&held).map_err(|_| unreadable())?;
    if listed.dev() != u64::try_from(open.st_dev).unwrap_or(u64::MAX) || listed.ino() != open.st_ino {
        return Err(unreadable());
    }
    let listing = std::str::from_utf8(&output.stdout).map_err(|_| unreadable())?;
    Ok(macos_acl_listing_grants_access(listing))
}

/// Test seam: runs around the ACL listing, so a test can swap the pathname
/// while the listing runs.
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod acl_listing_seam {
    use std::{cell::RefCell, path::Path};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Stage {
        Before,
        After,
    }

    type Hook = Box<dyn Fn(Stage, &Path)>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(crate) fn set(hook: Option<Hook>) {
        HOOK.with(|cell| *cell.borrow_mut() = hook);
    }

    pub(super) fn run(stage: Stage, path: &Path) {
        HOOK.with(|cell| {
            if let Some(hook) = cell.borrow().as_ref() {
                hook(stage, path);
            }
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn acl_grants_access(_descriptor: &impl AsFd, _path: &Path) -> Result<bool, OperatorRefusal> {
    Err(invalid("the backup password file's access control list cannot be checked on this system"))
}

/// Linux `system.posix_acl_access` value: a little-endian version 2 header,
/// then 8-byte entries (tag u16, permissions u16, id u32). A named user or
/// group entry with any permission grants access the mode bits do not show;
/// a malformed value counts as granting.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn posix_acl_grants_access(value: &[u8]) -> bool {
    const ACL_USER: u16 = 0x02;
    const ACL_GROUP: u16 = 0x08;
    const KNOWN_TAGS: [u16; 6] = [0x01, ACL_USER, 0x04, ACL_GROUP, 0x10, 0x20];
    let Some((header, entries)) = value.split_first_chunk::<4>() else {
        return true;
    };
    if u32::from_le_bytes(*header) != 2 || entries.len() % 8 != 0 {
        return true;
    }
    entries.chunks_exact(8).any(|entry| {
        let tag = u16::from_le_bytes([entry[0], entry[1]]);
        let permissions = u16::from_le_bytes([entry[2], entry[3]]);
        !KNOWN_TAGS.contains(&tag) || ((tag == ACL_USER || tag == ACL_GROUP) && permissions != 0)
    })
}

/// `ls -led` output on macOS: entry lines read ` N: <who> [inherited] allow|deny <rights>`.
/// Any `allow` entry grants access the mode bits do not show.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn macos_acl_listing_grants_access(listing: &str) -> bool {
    listing.lines().skip(1).any(|line| {
        let Some((index, entry)) = line.trim_start().split_once(':') else {
            return false;
        };
        !index.is_empty()
            && index.bytes().all(|byte| byte.is_ascii_digit())
            && entry.split_whitespace().any(|word| word == "allow")
    })
}
