// SPDX-License-Identifier: AGPL-3.0-only

//! Points `config.toml` at the moved folder (DEC-160).
//!
//! The configuration holds absolute paths (`data_dir`, the TLS files, the
//! backup password item's account) under the Hub folder. After the folder
//! moves, every TOML string whose decoded value is the earlier folder or a
//! path in it is rewritten to name the current folder instead; a sibling such
//! as `<folder> Window` is not touched. The decoded values decide, so escaped
//! (`/...`) and multi-line strings are found like plain ones.
//!
//! The text is kept byte for byte where the earlier folder is written plainly
//! inside quotes (comments and layout included). When that edit does not
//! produce exactly the expected values (an escaped or multi-line path), the
//! document is written again from its parsed values instead: the values are
//! all kept, comments and layout are not. Before anything is written, the new
//! text is parsed again and must hold exactly the expected values, none of
//! them naming the earlier folder; it then replaces the file atomically
//! (temporary file, `fsync`, `rename`).
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
    // A configuration that does not parse is the loader's to report.
    let Ok(values) = text.parse::<toml::Table>() else { return Ok(ConfigOutcome::Current) };
    let mut expected = values.clone();
    if !move_table(&mut expected, earlier, current) {
        return Ok(ConfigOutcome::Current);
    }
    let holds_expected = |candidate: &str| candidate.parse::<toml::Table>().is_ok_and(|parsed| parsed == expected);
    let rewritten = match replace_root(&text, earlier, current).filter(|candidate| holds_expected(candidate)) {
        Some(layout_kept) => layout_kept,
        None => toml::to_string(&expected).map_err(io::Error::other)?,
    };
    match rewritten.parse::<toml::Table>() {
        Ok(parsed) if parsed == expected && !names_root(&parsed, earlier) => {}
        _ => {
            return Err(io::Error::other(
                "the Hub configuration could not be pointed at the moved folder; it was left unchanged",
            ));
        }
    }
    replace_atomically(config, rewritten.as_bytes(), metadata.mode() & 0o7777)?;
    Ok(ConfigOutcome::Rewritten)
}

/// Rewrites every string value in `table` that is `earlier` or a path in it;
/// whether any was.
fn move_table(table: &mut toml::Table, earlier: &str, current: &str) -> bool {
    let mut changed = false;
    for (_, value) in table.iter_mut() {
        changed |= move_value(value, earlier, current);
    }
    changed
}

fn move_value(value: &mut toml::Value, earlier: &str, current: &str) -> bool {
    match value {
        toml::Value::String(text) => match moved(text, earlier, current) {
            Some(rewritten) => {
                *text = rewritten;
                true
            }
            None => false,
        },
        toml::Value::Array(items) => {
            items.iter_mut().fold(false, |changed, item| move_value(item, earlier, current) | changed)
        }
        toml::Value::Table(table) => move_table(table, earlier, current),
        _ => false,
    }
}

/// `path` under `current` when it is `earlier` or a path in it.
fn moved(path: &str, earlier: &str, current: &str) -> Option<String> {
    let rest = path.strip_prefix(earlier)?;
    (rest.is_empty() || rest.starts_with('/')).then(|| format!("{current}{rest}"))
}

/// Whether any decoded string in `table` still names `earlier` or a path in it.
fn names_root(table: &toml::Table, earlier: &str) -> bool {
    fn names(value: &toml::Value, earlier: &str) -> bool {
        match value {
            toml::Value::String(text) => moved(text, earlier, "").is_some(),
            toml::Value::Array(items) => items.iter().any(|item| names(item, earlier)),
            toml::Value::Table(table) => names_root(table, earlier),
            _ => false,
        }
    }
    table.values().any(|value| names(value, earlier))
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
    fn a_current_folder_that_cannot_be_written_plainly_still_gets_exact_values() {
        let root = tempfile::tempdir().unwrap();
        let earlier = root.path().join("Earlier Hub");
        let current = root.path().join("Current \"Hub");
        fs::create_dir_all(&current).unwrap();
        let config = current.join("config.toml");
        fs::write(&config, format!("data_dir = \"{}/data\"\n", earlier.display())).unwrap();
        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Rewritten);
        let table: toml::Table = fs::read_to_string(&config).unwrap().parse().unwrap();
        assert_eq!(table["data_dir"].as_str(), Some(format!("{}/data", current.display()).as_str()));
    }

    /// REV-CX-M08-T12A finding 1: escaped and multi-line strings decode to the
    /// earlier folder without spelling it plainly; they are found and rewritten.
    #[test]
    fn escaped_and_multi_line_paths_are_rewritten_by_their_decoded_value() {
        let root = tempfile::tempdir().unwrap();
        let earlier = root.path().join("Earlier Hub");
        let current = root.path().join("Current Hub");
        fs::create_dir_all(&current).unwrap();
        let config = current.join("config.toml");
        let base = root.path().to_str().unwrap();
        let escaped = format!("\\u002f{}", &base[1..]);
        let text = format!(
            "# a comment\n\
             data_dir = \"{escaped}/Earlier Hub/data\"\n\
             [tls]\n\
             certificate_path = \"\"\"\n{base}/Earlier Hub/tls/hub.pem\"\"\"\n\
             private_key_path = '''\n{base}/Earlier Hub/tls/hub.key'''\n\
             [backup]\n\
             password_file = \"{base}/Earlier\\u0020Hub/backup-password\"\n\
             destination = \"{base}/Earlier Hub Backup\"\n"
        );
        fs::write(&config, &text).unwrap();
        let before: toml::Table = text.parse().unwrap();
        assert_eq!(before["data_dir"].as_str(), Some(format!("{base}/Earlier Hub/data").as_str()));
        assert!(!text.contains(&format!("\"{base}/Earlier Hub/")), "no path is written plainly after a quote");

        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Rewritten);
        let after: toml::Table = fs::read_to_string(&config).unwrap().parse().unwrap();
        let under = |path: &str| Some(format!("{base}/{path}"));
        assert_eq!(after["data_dir"].as_str().map(str::to_owned), under("Current Hub/data"));
        assert_eq!(after["tls"]["certificate_path"].as_str().map(str::to_owned), under("Current Hub/tls/hub.pem"));
        assert_eq!(after["tls"]["private_key_path"].as_str().map(str::to_owned), under("Current Hub/tls/hub.key"));
        assert_eq!(after["backup"]["password_file"].as_str().map(str::to_owned), under("Current Hub/backup-password"));
        assert_eq!(after["backup"]["destination"].as_str().map(str::to_owned), under("Earlier Hub Backup"));
        assert!(!names_root(&after, earlier.to_str().unwrap()));
        // A second run (or a retry after a crash) finds nothing left to move.
        let written = fs::read_to_string(&config).unwrap();
        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Current);
        assert_eq!(fs::read_to_string(&config).unwrap(), written);
    }

    #[test]
    fn a_configuration_that_does_not_parse_is_left_to_the_loader() {
        let root = tempfile::tempdir().unwrap();
        let earlier = root.path().join("Earlier Hub");
        let current = root.path().join("Current Hub");
        fs::create_dir_all(&current).unwrap();
        let config = current.join("config.toml");
        let text = format!("data_dir = \"{}/data\"\nbroken = [\n", earlier.display());
        fs::write(&config, &text).unwrap();
        assert_eq!(rewrite_moved_root(&config, &earlier, &current).unwrap(), ConfigOutcome::Current);
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
