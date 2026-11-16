// SPDX-License-Identifier: AGPL-3.0-only

//! Points `config.toml` at the moved folder (DEC-160).
//!
//! The configuration holds absolute paths (`data_dir`, the TLS files, the
//! backup password item's account) under the Hub folder. After the folder
//! moves, each path that starts with the earlier folder is rewritten to start
//! with the current one. Only whole path prefixes inside quoted TOML strings
//! change: a sibling such as `<folder> Window` is not touched. The text is
//! otherwise kept byte for byte (comments and layout included), parsed before
//! it is used, and replaced atomically (temporary file, `fsync`, `rename`).
//!
//! The rewrite runs only once the earlier folder is gone and the current one
//! exists, and finds nothing to do on a second run, so a crash between the
//! folder move and this step is finished by the next run (the app's start-up
//! step, or the service's own start-up through [`prepare_config_path`]).

use std::{
    fs,
    io::{self, Write as _},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::Path,
};

/// The largest configuration this step reads.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigOutcome {
    /// No configuration file.
    Missing,
    /// Nothing points at the earlier folder (or it still exists).
    Current,
    /// Earlier-folder paths now name the current folder.
    Rewritten,
}

/// Rewrites earlier-folder paths in `config` (see the module notes).
pub fn rewrite_moved_root(config: &Path, earlier_root: &Path, current_root: &Path) -> io::Result<ConfigOutcome> {
    if earlier_root == current_root || exists(earlier_root)? || !exists(current_root)? {
        return Ok(if exists(config)? { ConfigOutcome::Current } else { ConfigOutcome::Missing });
    }
    let metadata = match fs::symlink_metadata(config) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ConfigOutcome::Missing),
        Err(error) => return Err(error),
    };
    // A link, a folder or an oversized file is not this step's to change: the
    // configuration loader reports it as it always has.
    if !metadata.file_type().is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Ok(ConfigOutcome::Current);
    }
    let (Some(earlier), Some(current)) = (earlier_root.to_str(), current_root.to_str()) else {
        return Ok(ConfigOutcome::Current);
    };
    let text = fs::read_to_string(config)?;
    let Some(rewritten) = replace_root(&text, earlier, current) else {
        return Ok(ConfigOutcome::Current);
    };
    if rewritten.parse::<toml::Table>().is_err() {
        return Err(io::Error::other("the Hub configuration would not parse after pointing it at the moved folder"));
    }
    replace_atomically(config, rewritten.as_bytes(), metadata.mode() & 0o7777)?;
    Ok(ConfigOutcome::Rewritten)
}

/// The configuration at `config_path` before anything reads it: when its
/// folder carries the current spelling and the earlier folder has moved,
/// stale earlier-folder paths are rewritten. A failure is returned, so a
/// command never opens a missing earlier data folder (which would start an
/// empty Hub there).
pub fn prepare_config_path(config_path: &Path) -> io::Result<ConfigOutcome> {
    let Some(current_root) = config_path.parent() else { return Ok(ConfigOutcome::Current) };
    let Some(name) = current_root.file_name().and_then(|name| name.to_str()) else {
        return Ok(ConfigOutcome::Current);
    };
    let Some(earlier_name) = super::spelling::earlier(name) else { return Ok(ConfigOutcome::Current) };
    rewrite_moved_root(config_path, &current_root.with_file_name(earlier_name), current_root)
}

/// `text` with every quoted path that starts with `earlier` (followed by `/`
/// or the closing quote) starting with `current` instead; `None` if none does.
fn replace_root(text: &str, earlier: &str, current: &str) -> Option<String> {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some(position) = rest.find(earlier) {
        let (before, from) = rest.split_at(position);
        let after = &from[earlier.len()..];
        let opened = before.ends_with('"') || before.ends_with('\'');
        let closed = after.starts_with('/') || after.starts_with('"') || after.starts_with('\'');
        output.push_str(before);
        if opened && closed {
            output.push_str(current);
            changed = true;
        } else {
            output.push_str(earlier);
        }
        rest = after;
    }
    output.push_str(rest);
    changed.then_some(output)
}

fn replace_atomically(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| io::Error::other("the configuration has no folder"))?;
    let name = path.file_name().ok_or_else(|| io::Error::other("the configuration has no name"))?;
    let mut temporary_name = std::ffi::OsString::from(".");
    temporary_name.push(name);
    temporary_name.push(format!(".rename-step-{}", std::process::id()));
    let temporary = directory.join(temporary_name);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .mode(mode)
            .open(&temporary)?;
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(directory)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    const CONFIG: &str = "# kept comment\n\
        data_dir = \"ROOT/Earlier Hub/data\"\n\
        bind = \"127.0.0.1:8443\"\n\
        [tls]\n\
        certificate_path = 'ROOT/Earlier Hub/tls/hub.pem'\n\
        [backup]\n\
        password_file = \"ROOT/Earlier Hub/backup-password\"\n\
        destination = \"ROOT/Earlier Hub Backup\"\n";

    fn seed(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let earlier = root.join("Earlier Hub");
        let current = root.join("Current Hub");
        fs::create_dir_all(&current).unwrap();
        let config = current.join("config.toml");
        fs::write(&config, CONFIG.replace("ROOT", root.to_str().unwrap())).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        (earlier, current, config)
    }

    #[test]
    fn earlier_folder_paths_are_rewritten_once_and_nothing_else_changes() {
        let root = tempfile::tempdir().unwrap();
        let (earlier, current, config) = seed(root.path());
        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Rewritten);
        let text = fs::read_to_string(&config).unwrap();
        let base = root.path().to_str().unwrap();
        assert_eq!(text, CONFIG.replace("ROOT/Earlier Hub/", "ROOT/Current Hub/").replace("ROOT", base));
        assert!(text.contains("Earlier Hub Backup"), "a sibling folder keeps its name");
        assert!(text.starts_with("# kept comment\n"));
        assert_eq!(fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Current);
        assert_eq!(fs::read_to_string(&config).unwrap(), text);
    }

    #[test]
    fn nothing_is_rewritten_while_the_earlier_folder_still_exists() {
        let root = tempfile::tempdir().unwrap();
        let (earlier, current, config) = seed(root.path());
        fs::create_dir_all(&earlier).unwrap();
        let before = fs::read_to_string(&config).unwrap();
        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Current);
        assert_eq!(fs::read_to_string(&config).unwrap(), before);
    }

    #[test]
    fn a_missing_configuration_is_reported_not_created() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("Current Hub");
        fs::create_dir_all(&current).unwrap();
        let config = current.join("config.toml");
        assert_eq!(rewrite_moved_root(&config, &root.path().join("Earlier Hub"), &current).unwrap(), ConfigOutcome::Missing);
        assert!(!config.exists());
    }

    #[test]
    fn an_interrupted_rewrite_leaves_the_old_text_and_the_next_run_finishes() {
        let root = tempfile::tempdir().unwrap();
        let (earlier, current, config) = seed(root.path());
        // A crash after the temporary file was written but before the rename.
        fs::write(current.join(".config.toml.rename-step-1"), "partial").unwrap();
        let before = fs::read_to_string(&config).unwrap();
        assert!(before.contains("Earlier Hub/data"));
        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Rewritten);
        let own = format!(".config.toml.rename-step-{}", std::process::id());
        assert!(!current.join(own).exists());
        assert!(fs::read_to_string(&config).unwrap().contains("Current Hub/data"));
    }

    #[test]
    fn a_rewrite_that_would_not_parse_is_refused_and_the_file_kept() {
        let root = tempfile::tempdir().unwrap();
        let earlier = root.path().join("Earlier Hub");
        let current = root.path().join("Current \"Hub");
        fs::create_dir_all(&current).unwrap();
        let config = current.join("config.toml");
        let text = format!("data_dir = \"{}/data\"\n", earlier.display());
        fs::write(&config, &text).unwrap();
        assert!(rewrite_moved_root(&config, &earlier, &current).is_err());
        assert_eq!(fs::read_to_string(&config).unwrap(), text);
    }

    #[test]
    fn a_linked_configuration_is_left_to_the_loader() {
        let root = tempfile::tempdir().unwrap();
        let (earlier, current, config) = seed(root.path());
        let real = root.path().join("real.toml");
        fs::rename(&config, &real).unwrap();
        std::os::unix::fs::symlink(&real, &config).unwrap();
        let before = fs::read_to_string(&real).unwrap();
        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Current);
        assert_eq!(fs::read_to_string(&real).unwrap(), before);
    }

    #[test]
    fn the_service_start_up_rewrites_only_for_a_renamed_folder() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("Tesatlas Hub");
        fs::create_dir_all(&current).unwrap();
        let config = current.join("config.toml");
        let earlier = root.path().join("Teslatlas Hub"); // rename:keep
        fs::write(&config, format!("data_dir = \"{}/data\"\n", earlier.display())).unwrap();
        assert_eq!(prepare_config_path(&config).unwrap(), ConfigOutcome::Rewritten);
        assert_eq!(fs::read_to_string(&config).unwrap(), format!("data_dir = \"{}/data\"\n", current.display()));
        let unrenamed = root.path().join("Other Hub").join("config.toml");
        assert_eq!(prepare_config_path(&unrenamed).unwrap(), ConfigOutcome::Current);
    }
}
