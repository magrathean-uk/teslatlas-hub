// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    ffi::OsString,
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
};

use tempfile::tempdir;

use super::*;

const EUID: u32 = 1000;
const OTHER: u32 = 2000;

fn facts(uid: u32, mode: u32) -> DirectoryFacts {
    DirectoryFacts { uid, mode }
}

fn real_euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn open_for_test(path: &Path, create: bool) -> Result<PrivateDirectory, PrivateDirectoryError> {
    open_validated(path, create)
}

fn error_of(result: Result<PrivateDirectory, PrivateDirectoryError>) -> PrivateDirectoryError {
    match result {
        Ok(directory) => panic!("must be rejected, got {}", directory.path().display()),
        Err(error) => error,
    }
}

/// Walk `parents` and `leaf` below `base`, with `euid` standing in for the
/// effective user so ownership can be simulated without root.
fn walk_below(
    base: &Path,
    parents: &[&str],
    leaf: &str,
    create: bool,
    euid: u32,
) -> Result<PrivateDirectory, PrivateDirectoryError> {
    let descriptor = open(base, directory_flags(), Mode::empty()).unwrap();
    let parents: Vec<OsString> = parents.iter().map(OsString::from).collect();
    walk(
        descriptor,
        base.to_path_buf(),
        &parents,
        OsStr::new(leaf),
        create,
        euid,
    )
}

// ---- the ancestor rule --------------------------------------------------

#[test]
fn ancestors_owned_by_root_or_the_user_without_write_for_others_pass() {
    assert_eq!(ancestor_fault(facts(0, 0o755), EUID), None);
    assert_eq!(ancestor_fault(facts(0, 0o700), EUID), None);
    assert_eq!(ancestor_fault(facts(EUID, 0o755), EUID), None);
    assert_eq!(ancestor_fault(facts(EUID, 0o700), EUID), None);
}

#[test]
fn an_ancestor_owned_by_another_user_is_untrusted_whatever_its_mode() {
    for mode in [0o700, 0o755, 0o775, 0o777, 0o1777] {
        assert_eq!(
            ancestor_fault(facts(OTHER, mode), EUID),
            Some(DirectoryFault::UntrustedOwner),
            "mode {mode:o}"
        );
    }
}

#[test]
fn a_group_or_world_writable_ancestor_needs_the_user_or_a_sticky_root() {
    // Root without the sticky bit: anyone in the group, or anyone at all, may
    // rename or replace what is inside.
    for mode in [0o775, 0o770, 0o020, 0o757, 0o777, 0o002] {
        assert_eq!(
            ancestor_fault(facts(0, mode), EUID),
            Some(DirectoryFault::WritableByOthers),
            "root, mode {mode:o}"
        );
    }
    // Root with the sticky bit (the `/tmp` shape) protects entries by owner.
    assert_eq!(ancestor_fault(facts(0, 0o1777), EUID), None);
    assert_eq!(ancestor_fault(facts(0, 0o1775), EUID), None);
    // The effective user owns it, so only the user (or root) could swap it.
    for mode in [0o775, 0o777, 0o770, 0o757] {
        assert_eq!(ancestor_fault(facts(EUID, mode), EUID), None, "{mode:o}");
    }
}

#[test]
fn a_root_user_may_use_its_own_writable_ancestors() {
    assert_eq!(ancestor_fault(facts(0, 0o777), 0), None);
    assert_eq!(
        ancestor_fault(facts(OTHER, 0o755), 0),
        Some(DirectoryFault::UntrustedOwner)
    );
}

// ---- opening and creating ------------------------------------------------

#[test]
fn a_missing_path_is_created_one_private_directory_at_a_time() {
    let base = tempdir().unwrap();
    let wanted = base.path().join("a/b/leaf");

    let directory = open_for_test(&wanted, true).unwrap();

    for part in ["a", "a/b", "a/b/leaf"] {
        assert_eq!(mode_of(&base.path().join(part)), 0o700, "{part}");
    }
    assert!(directory.path().ends_with("a/b/leaf"));
}

#[test]
fn a_missing_path_is_not_created_without_permission_to_create() {
    let base = tempdir().unwrap();

    let error = error_of(open_for_test(&base.path().join("a/leaf"), false));

    assert!(
        matches!(error, PrivateDirectoryError::Io { .. }),
        "{error:?}"
    );
    assert!(!base.path().join("a").exists());
}

#[test]
fn an_existing_directory_is_opened_and_creation_is_idempotent() {
    let base = tempdir().unwrap();
    let wanted = base.path().join("leaf");

    open_for_test(&wanted, true).unwrap();
    let again = open_for_test(&wanted, true).unwrap();

    assert_eq!(again.entry_names().unwrap(), Vec::<String>::new());
}

#[test]
fn a_symlinked_leaf_is_rejected_whether_or_not_its_target_exists() {
    let base = tempdir().unwrap();
    let real = base.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&real, base.path().join("to-real")).unwrap();
    symlink(base.path().join("missing"), base.path().join("to-missing")).unwrap();

    for name in ["to-real", "to-missing"] {
        let error = error_of(open_for_test(&base.path().join(name), true));
        assert!(
            matches!(&error, PrivateDirectoryError::Symlink(path) if path.ends_with(name)),
            "{name}: {error:?}"
        );
    }
    assert!(!base.path().join("missing").exists());
}

#[test]
fn a_symlinked_ancestor_is_resolved_once_and_the_resolved_path_is_validated() {
    let base = tempdir().unwrap();
    let real = base.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&real, base.path().join("link")).unwrap();

    let directory = open_for_test(&base.path().join("link/leaf"), true).unwrap();

    assert!(real.join("leaf").is_dir());
    assert!(directory.path().starts_with(real.canonicalize().unwrap()));
}

#[test]
fn a_leaf_that_is_a_file_is_not_a_directory() {
    let base = tempdir().unwrap();
    fs::write(base.path().join("leaf"), b"file").unwrap();

    assert!(matches!(
        error_of(open_for_test(&base.path().join("leaf"), true)),
        PrivateDirectoryError::NotDirectory(_)
    ));
}

#[test]
fn an_ancestor_that_is_a_file_is_not_a_directory_and_nothing_is_created() {
    let base = tempdir().unwrap();
    fs::write(base.path().join("file"), b"file").unwrap();

    assert!(matches!(
        error_of(open_for_test(&base.path().join("file/leaf"), true)),
        PrivateDirectoryError::NotDirectory(_)
    ));
    assert_eq!(fs::read(base.path().join("file")).unwrap(), b"file");
}

#[test]
fn only_absolute_paths_of_plain_components_are_accepted() {
    for path in ["relative/leaf", "leaf", "/", "/a/../b", "/a/./b/../c"] {
        assert!(
            matches!(
                error_of(open_for_test(Path::new(path), false)),
                PrivateDirectoryError::InvalidPath
            ),
            "{path}"
        );
    }
}

#[test]
fn a_leaf_with_a_wider_mode_is_tightened_through_its_descriptor() {
    let base = tempdir().unwrap();
    let leaf = base.path().join("leaf");
    fs::create_dir(&leaf).unwrap();
    for wide in [0o750, 0o770, 0o775, 0o777, 0o755, 0o1700, 0o2700] {
        fs::set_permissions(&leaf, fs::Permissions::from_mode(wide)).unwrap();

        open_for_test(&leaf, true).unwrap();

        assert_eq!(mode_of(&leaf), 0o700, "from {wide:o}");
    }
}

#[test]
fn a_read_only_leaf_of_the_user_is_made_writable_through_its_descriptor() {
    let base = tempdir().unwrap();
    let leaf = base.path().join("leaf");
    fs::create_dir(&leaf).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o500)).unwrap();

    open_for_test(&leaf, true).unwrap();

    assert_eq!(mode_of(&leaf), 0o700);
}

#[test]
fn a_writable_ancestor_owned_by_the_user_is_accepted_and_left_as_it_was() {
    let base = tempdir().unwrap();
    let ancestor = base.path().join("shared");
    fs::create_dir(&ancestor).unwrap();
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o777)).unwrap();

    open_for_test(&ancestor.join("leaf"), true).unwrap();

    assert_eq!(mode_of(&ancestor), 0o777);
}

// ---- extended ACLs (macOS) ----------------------------------------------------

/// The ACL entries `ls -le` shows for `path`, one string per entry.
#[cfg(target_os = "macos")]
fn acl_entries(path: &Path) -> Vec<String> {
    let output = std::process::Command::new("/bin/ls")
        .arg("-lde")
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| line.trim().to_owned())
        .collect()
}

#[cfg(target_os = "macos")]
fn add_acl(path: &Path, entry: &str) {
    let status = std::process::Command::new("/bin/chmod")
        .arg("+a")
        .arg(entry)
        .arg(path)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "cannot set `{entry}` on {}",
        path.display()
    );
}

#[cfg(target_os = "macos")]
const OPEN_TO_EVERYONE: &str = "group:everyone allow list,search,add_file,delete_child";

#[cfg(target_os = "macos")]
const INHERITED_BY_CHILDREN: &str =
    "group:everyone allow list,search,add_file,delete_child,file_inherit,directory_inherit";

#[cfg(target_os = "macos")]
#[test]
fn a_private_leaf_with_an_acl_that_admits_others_has_it_cleared() {
    let base = tempdir().unwrap();
    let leaf = base.path().join("leaf");
    fs::create_dir(&leaf).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o700)).unwrap();
    add_acl(&leaf, OPEN_TO_EVERYONE);
    assert_eq!(acl_entries(&leaf).len(), 1, "the fixture ACL is in place");

    let directory = open_for_test(&leaf, false).unwrap();

    // The mode bits already said `0700`; only the ACL let others in.
    assert!(acl_entries(&leaf).is_empty());
    assert_eq!(mode_of(&leaf), 0o700);
    drop(directory);

    // Opening again, with nothing to clear, still works.
    open_for_test(&leaf, false).unwrap();
    assert!(acl_entries(&leaf).is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn a_leaf_with_a_wide_mode_and_an_acl_loses_both() {
    let base = tempdir().unwrap();
    let leaf = base.path().join("leaf");
    fs::create_dir(&leaf).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o775)).unwrap();
    add_acl(&leaf, OPEN_TO_EVERYONE);

    open_for_test(&leaf, true).unwrap();

    assert!(acl_entries(&leaf).is_empty());
    assert_eq!(mode_of(&leaf), 0o700);
}

#[cfg(target_os = "macos")]
#[test]
fn directories_created_below_an_inheritable_acl_do_not_keep_the_inherited_entries() {
    let base = tempdir().unwrap();
    let parent = base.path().join("parent");
    fs::create_dir(&parent).unwrap();
    add_acl(&parent, INHERITED_BY_CHILDREN);

    let directory = open_for_test(&parent.join("middle/leaf"), true).unwrap();

    // Both components were created here, so both inherited the entry and both
    // had it cleared; the parent was not ours to change.
    assert!(acl_entries(&parent.join("middle")).is_empty());
    assert!(acl_entries(&parent.join("middle/leaf")).is_empty());
    assert_eq!(mode_of(&parent.join("middle/leaf")), 0o700);
    assert_eq!(acl_entries(&parent).len(), 1);
    drop(directory);
}

#[cfg(target_os = "macos")]
#[test]
fn a_directory_that_still_shows_an_acl_after_the_clear_is_refused() {
    let base = tempdir().unwrap();
    let leaf = base.path().join("leaf");
    fs::create_dir(&leaf).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o700)).unwrap();
    add_acl(&leaf, INHERITED_BY_CHILDREN);
    let descriptor = open(&leaf, directory_flags(), Mode::empty()).unwrap();

    // A clear that reports success and leaves the entry behind is not taken
    // on trust: the listing that follows it refuses the directory.
    let error = clear_and_verify(&descriptor, &leaf, |_, _| Ok(())).unwrap_err();
    assert!(
        matches!(error, PrivateDirectoryError::WritableByOthers(_)),
        "{error:?}"
    );
    assert_eq!(acl_entries(&leaf).len(), 1, "nothing was cleared");

    // A clear that fails is reported as its own error, before any listing.
    let failed = clear_and_verify(&descriptor, &leaf, |_, path| {
        Err(io_error(path, io::Error::other("chmod refused")))
    })
    .unwrap_err();
    assert!(
        matches!(failed, PrivateDirectoryError::Io { .. }),
        "{failed:?}"
    );

    // The real clear removes the entry and the listing then agrees.
    clear_and_verify(&descriptor, &leaf, chmod_clear_acl).unwrap();
    assert!(acl_entries(&leaf).is_empty());
    verify_no_extended_acl(&descriptor, &leaf).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn the_acl_check_follows_the_held_directory_not_the_path_it_was_given() {
    let base = tempdir().unwrap();
    let leaf = base.path().join("leaf");
    fs::create_dir(&leaf).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o700)).unwrap();
    add_acl(&leaf, OPEN_TO_EVERYONE);
    let descriptor = open(&leaf, directory_flags(), Mode::empty()).unwrap();
    // The path the caller names is now a different directory with no ACL; the
    // listing still looks at the directory the descriptor holds.
    let moved = base.path().join("moved");
    fs::rename(&leaf, &moved).unwrap();
    fs::create_dir(&leaf).unwrap();

    let error = verify_no_extended_acl(&descriptor, &leaf).unwrap_err();
    assert!(
        matches!(error, PrivateDirectoryError::WritableByOthers(_)),
        "{error:?}"
    );
    chmod_clear_acl(&descriptor, &leaf).unwrap();
    verify_no_extended_acl(&descriptor, &leaf).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn acl_entries_are_counted_from_the_numbered_lines_of_a_listing() {
    let entry = "drwx------+ 2 user staff 64 Oct 11 07:32 /private/cache";
    assert_eq!(acl_entry_count(entry), 0);
    assert_eq!(acl_entry_count(&format!("{entry}\n")), 0);
    assert_eq!(
        acl_entry_count(&format!(
            "{entry}\n 0: group:everyone allow list,search,add_file\n"
        )),
        1
    );
    assert_eq!(
        acl_entry_count(&format!(
            "{entry}\n 0: user:a allow search\n 1: group:b deny write\n10: group:c allow read\n"
        )),
        3
    );
    // The first line is the directory itself, whatever it looks like, and a
    // line that merely contains a colon is not an entry.
    assert_eq!(acl_entry_count("0: odd name\nnot an entry: really\n"), 0);
    assert_eq!(acl_entry_count(""), 0);
}

#[cfg(target_os = "macos")]
#[test]
fn chmod_results_are_read_as_cleared_unsupported_or_failed() {
    assert_eq!(acl_clear_outcome(true, ""), Ok(()));
    // A filesystem that cannot hold ACLs has nothing to clear.
    assert_eq!(
        acl_clear_outcome(
            false,
            "chmod: Failed to clear ACL on file /dev/fd/0: Operation not supported\n"
        ),
        Ok(())
    );
    assert_eq!(
        acl_clear_outcome(
            false,
            "chmod: Failed to clear ACL: Permission denied\nmore\n"
        ),
        Err("chmod: Failed to clear ACL: Permission denied".to_owned())
    );
    assert_eq!(acl_clear_outcome(false, ""), Err("chmod failed".to_owned()));
}

// ---- ownership, simulated with an injected effective user ---------------------

#[test]
fn a_leaf_owned_by_someone_else_is_rejected_and_left_untouched() {
    let base = tempdir().unwrap();
    let leaf = base.path().join("leaf");
    fs::create_dir(&leaf).unwrap();
    fs::set_permissions(&leaf, fs::Permissions::from_mode(0o775)).unwrap();

    // The directory belongs to the real user; to the walk the user is another.
    let error = error_of(walk_below(base.path(), &[], "leaf", true, real_euid() + 1));

    assert!(
        matches!(&error, PrivateDirectoryError::UntrustedOwner(path) if path.ends_with("leaf")),
        "{error:?}"
    );
    assert_eq!(mode_of(&leaf), 0o775, "a foreign directory is not chmodded");
}

#[test]
fn an_ancestor_owned_by_someone_else_is_rejected_and_nothing_is_created_below_it() {
    let base = tempdir().unwrap();
    let ancestor = base.path().join("ancestor");
    fs::create_dir(&ancestor).unwrap();
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();

    let error = error_of(walk_below(
        base.path(),
        &["ancestor"],
        "leaf",
        true,
        real_euid() + 1,
    ));

    assert!(
        matches!(&error, PrivateDirectoryError::UntrustedOwner(path) if path.ends_with("ancestor")),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(&ancestor).unwrap().count(), 0);
}

#[test]
fn the_walk_accepts_what_the_real_user_owns() {
    let base = tempdir().unwrap();

    let directory = walk_below(base.path(), &["a"], "leaf", true, real_euid()).unwrap();

    assert_eq!(mode_of(&base.path().join("a")), 0o700);
    assert!(directory.path().ends_with("a/leaf"));
}

// ---- operations stay inside the held directory ---------------------------------

fn held(base: &Path) -> PrivateDirectory {
    open_for_test(&base.join("held"), true).unwrap()
}

#[test]
fn entries_are_single_plain_names() {
    let base = tempdir().unwrap();
    let directory = held(base.path());
    fs::write(base.path().join("outside"), b"outside").unwrap();

    for name in ["", ".", "..", "a/b", "../outside", "/etc/passwd", "a\0b"] {
        let error = directory.open_regular(name).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name:?}");
        assert!(directory.create_exclusive(name).is_err(), "{name:?}");
        assert!(directory.remove(name).is_err(), "{name:?}");
        assert!(directory.rename(name, "x").is_err(), "{name:?}");
        assert!(directory.rename("x", name).is_err(), "{name:?}");
    }
    assert_eq!(fs::read(base.path().join("outside")).unwrap(), b"outside");
}

#[test]
fn create_rename_and_remove_work_on_entries_of_the_directory() {
    let base = tempdir().unwrap();
    let directory = held(base.path());

    let mut file = directory.create_exclusive("first").unwrap();
    file.write_all(b"data").unwrap();
    drop(file);
    assert_eq!(mode_of(&base.path().join("held/first")), 0o600);
    directory.rename("first", "second").unwrap();
    assert_eq!(directory.entry_names().unwrap(), ["second"]);
    assert_eq!(fs::read(base.path().join("held/second")).unwrap(), b"data");
    directory.remove("second").unwrap();
    directory.remove("second").unwrap();
    assert!(directory.entry_names().unwrap().is_empty());
    directory.sync().unwrap();
    assert!(directory.available_bytes().unwrap() > 0);
}

#[test]
fn create_exclusive_never_overwrites_or_follows() {
    let base = tempdir().unwrap();
    let directory = held(base.path());
    fs::write(base.path().join("held/existing"), b"keep").unwrap();
    let target = base.path().join("target");
    symlink(&target, base.path().join("held/link")).unwrap();

    assert_eq!(
        directory.create_exclusive("existing").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        directory.create_exclusive("link").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );

    assert_eq!(
        fs::read(base.path().join("held/existing")).unwrap(),
        b"keep"
    );
    assert!(!target.exists());
}

#[test]
fn open_regular_reports_only_regular_files() {
    let base = tempdir().unwrap();
    let directory = held(base.path());
    let inside = base.path().join("held");
    fs::write(inside.join("file"), b"data").unwrap();
    fs::create_dir(inside.join("directory")).unwrap();
    fs::write(base.path().join("target"), b"target").unwrap();
    symlink(base.path().join("target"), inside.join("link")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(inside.join("fifo"))
            .status()
            .unwrap()
            .success()
    );

    assert!(directory.open_regular("file").unwrap().is_some());
    for name in ["missing", "directory", "link", "fifo"] {
        assert!(directory.open_regular(name).unwrap().is_none(), "{name}");
    }
}

#[test]
fn operations_follow_the_held_directory_after_its_path_is_swapped() {
    let base = tempdir().unwrap();
    let directory = held(base.path());
    let elsewhere = base.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("victim"), b"victim").unwrap();
    fs::write(base.path().join("held/victim"), b"mine").unwrap();
    fs::rename(base.path().join("held"), base.path().join("moved")).unwrap();
    symlink(&elsewhere, base.path().join("held")).unwrap();

    directory.create_exclusive("created").unwrap();
    directory.rename("created", "renamed").unwrap();
    directory.remove("victim").unwrap();

    assert!(base.path().join("moved/renamed").is_file());
    assert!(!base.path().join("moved/victim").exists());
    assert_eq!(
        fs::read(elsewhere.join("victim")).unwrap(),
        b"victim",
        "the swapped-in directory is untouched"
    );
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
}

#[test]
fn entry_names_list_the_entries_without_the_dot_entries() {
    let base = tempdir().unwrap();
    let directory = held(base.path());
    let inside = base.path().join("held");
    fs::write(inside.join("plain"), b"").unwrap();
    fs::write(inside.join(".hidden"), b"").unwrap();
    fs::create_dir(inside.join("sub")).unwrap();

    let mut names = directory.entry_names().unwrap();
    names.sort();
    assert_eq!(names, [".hidden", "plain", "sub"]);
}
