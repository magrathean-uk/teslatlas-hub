// SPDX-License-Identifier: AGPL-3.0-only

use std::ffi::OsString;

#[derive(Debug, Eq, PartialEq)]
struct CompanionHelperLayout {
    helper: PathBuf,
    catalog: PathBuf,
}

fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn resolve_helper_layout(
    executable: &Path,
    source_root: &Path,
) -> Result<CompanionHelperLayout, String> {
    let executable_parent = executable
        .parent()
        .ok_or_else(|| "cannot resolve the Hub executable directory".to_owned())?;
    let mut candidates = Vec::new();
    if executable_parent.file_name().and_then(|name| name.to_str()) == Some("bin") {
        let prefix = executable_parent
            .parent()
            .ok_or_else(|| "cannot resolve the installed Hub prefix".to_owned())?;
        if prefix.file_name().and_then(|name| name.to_str()) == Some("usr") {
            candidates.push(CompanionHelperLayout {
                helper: prefix.join("lib/teslatlas-hub/bootstrap-companions.py"),
                catalog: prefix.join("share/teslatlas-hub/companions/catalog-current.json"),
            });
        }
        candidates.push(CompanionHelperLayout {
            helper: prefix.join("libexec/bootstrap-companions.py"),
            catalog: prefix.join("share/companions/catalog-current.json"),
        });
    }
    candidates.push(CompanionHelperLayout {
        helper: source_root.join("scripts/bootstrap-companions.py"),
        catalog: source_root.join("tools/companions/catalog-current.json"),
    });

    candidates
        .into_iter()
        .find(|candidate| regular_file(&candidate.helper) && regular_file(&candidate.catalog))
        .ok_or_else(|| {
            "the audited companion helper/catalog is missing from the Hub installation".to_owned()
        })
}

fn push_path(arguments: &mut Vec<OsString>, option: &str, path: &Path) {
    arguments.push(option.into());
    arguments.push(path.as_os_str().to_owned());
}

fn operation_arguments(
    action: &str,
    options: &CompanionOperationArgs,
    shipped_catalog: &Path,
) -> Result<Vec<OsString>, String> {
    if !options.prefix.is_absolute() {
        return Err("--prefix must be an absolute path".to_owned());
    }
    if options.components.is_empty()
        || options.components.iter().any(|component| component.is_empty())
        || options.components.iter().collect::<std::collections::HashSet<_>>().len()
            != options.components.len()
    {
        return Err("--components must be a unique nonempty set".to_owned());
    }
    match options.mode {
        CompanionMode::Production => {
            if options.catalog.is_some() || options.local_sources.is_some() {
                return Err(
                    "production mode uses only the fixed shipped/cached catalog and Git sources"
                        .to_owned(),
                );
            }
        }
        CompanionMode::LocalCandidate => {
            if options.catalog.is_none() || options.local_sources.is_none() {
                return Err(
                    "local-candidate mode requires both --catalog and --local-sources".to_owned(),
                );
            }
        }
    }

    let mut arguments = vec![OsString::from(action), OsString::from("--components")];
    arguments.push(options.components.join(",").into());
    push_path(&mut arguments, "--prefix", &options.prefix);
    arguments.push("--hub-version".into());
    arguments.push(env!("CARGO_PKG_VERSION").into());
    arguments.push("--mode".into());
    arguments.push(options.mode.as_str().into());
    push_path(
        &mut arguments,
        "--catalog",
        options.catalog.as_deref().unwrap_or(shipped_catalog),
    );
    if let Some(path) = &options.local_sources {
        push_path(&mut arguments, "--local-sources", path);
    }
    if let Some(path) = &options.node_bin {
        push_path(&mut arguments, "--node-bin", path);
    }
    if let Some(path) = &options.ha_config {
        push_path(&mut arguments, "--ha-config", path);
    }
    if let Some(target) = &options.edge_target {
        arguments.push("--edge-target".into());
        arguments.push(target.into());
    }
    if let Some(path) = &options.edge_go_binary {
        push_path(&mut arguments, "--edge-go-binary", path);
    }
    if let Some(path) = &options.edge_tool_root {
        push_path(&mut arguments, "--edge-tool-root", path);
    }
    arguments.push("--timeout-seconds".into());
    arguments.push(options.timeout_seconds.to_string().into());
    Ok(arguments)
}

fn prefix_arguments(action: &str, options: &CompanionPrefixArgs) -> Result<Vec<OsString>, String> {
    if !options.prefix.is_absolute() {
        return Err("--prefix must be an absolute path".to_owned());
    }
    let mut arguments = vec![OsString::from(action)];
    push_path(&mut arguments, "--prefix", &options.prefix);
    arguments.push("--hub-version".into());
    arguments.push(env!("CARGO_PKG_VERSION").into());
    Ok(arguments)
}

fn command_arguments(
    command: &CompanionCommand,
    shipped_catalog: &Path,
) -> Result<Vec<OsString>, String> {
    match command {
        CompanionCommand::Install(options) => {
            operation_arguments("install", options, shipped_catalog)
        }
        CompanionCommand::Update(options) => {
            operation_arguments("update", options, shipped_catalog)
        }
        CompanionCommand::Status(options) => prefix_arguments("status", options),
        CompanionCommand::Rollback(options) => prefix_arguments("rollback", options),
        CompanionCommand::DryRun(options) => {
            operation_arguments("dry-run", options, shipped_catalog)
        }
    }
}

fn supported_python_version(output: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(output) else {
        return false;
    };
    let Some(version) = text.trim().strip_prefix("Python ") else {
        return false;
    };
    let mut numbers = version.split('.');
    let (Some(major), Some(minor)) = (numbers.next(), numbers.next()) else {
        return false;
    };
    let (Ok(major), Ok(minor)) = (major.parse::<u16>(), minor.parse::<u16>()) else {
        return false;
    };
    major > 3 || (major == 3 && minor >= 10)
}

fn supported_python() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &[
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "/usr/bin/python3",
    ];
    #[cfg(not(target_os = "macos"))]
    const CANDIDATES: &[&str] = &["/usr/bin/python3"];

    for candidate in CANDIDATES {
        let path = Path::new(candidate);
        let Ok(path) = fs::canonicalize(path) else {
            continue;
        };
        if !regular_file(&path) {
            continue;
        }
        let Ok(output) = std::process::Command::new(&path).arg("--version").output() else {
            continue;
        };
        let version = if output.stdout.is_empty() {
            &output.stderr
        } else {
            &output.stdout
        };
        if output.status.success() && supported_python_version(version) {
            return Ok(path);
        }
    }
    Err("Python 3.10 or newer is required at a supported system installation path".to_owned())
}

fn companion_error(message: &str) -> ExitCode {
    println!(
        "{}",
        serde_json::json!({
            "status": "error",
            "error": message,
        })
    );
    ExitCode::from(2)
}

async fn run_companion_command(command: &CompanionCommand) -> ExitCode {
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => return companion_error(&format!("cannot resolve Hub executable: {error}")),
    };
    let layout = match resolve_helper_layout(&executable, Path::new(env!("CARGO_MANIFEST_DIR"))) {
        Ok(layout) => layout,
        Err(error) => return companion_error(&error),
    };
    let python = match supported_python() {
        Ok(python) => python,
        Err(error) => return companion_error(&error),
    };
    let arguments = match command_arguments(command, &layout.catalog) {
        Ok(arguments) => arguments,
        Err(error) => return companion_error(&error),
    };
    let output = match std::process::Command::new(python)
        .args(["-I", "-B"])
        .arg(&layout.helper)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            return companion_error(&format!("cannot start companion helper: {error}"));
        }
    };
    if let Err(error) = std::io::stdout().write_all(&output.stdout) {
        return companion_error(&format!("cannot write companion result: {error}"));
    }
    if let Err(error) = std::io::stderr().write_all(&output.stderr) {
        return companion_error(&format!("cannot write companion diagnostic: {error}"));
    }
    match output.status.code() {
        Some(code @ 0..=255) => ExitCode::from(code as u8),
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod companion_delegation_tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, "fixture\n").expect("write fixture");
    }

    #[test]
    fn installed_layout_wins_when_source_checkout_is_also_available() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let package_root = temporary.path().join("usr");
        let executable = package_root.join("bin/teslatlas-hub");
        let helper = package_root.join("lib/teslatlas-hub/bootstrap-companions.py");
        let catalog = package_root.join("share/teslatlas-hub/companions/catalog-current.json");
        touch(&executable);
        touch(&helper);
        touch(&catalog);

        let source = temporary.path().join("source");
        touch(&source.join("scripts/bootstrap-companions.py"));
        touch(&source.join("tools/companions/catalog-current.json"));

        let layout = resolve_helper_layout(&executable, &source).expect("installed layout");
        assert_eq!(layout.helper, helper);
        assert_eq!(layout.catalog, catalog);
    }

    #[test]
    fn structured_arguments_bind_the_compiled_hub_version() {
        let arguments = CompanionOperationArgs {
            components: vec!["protocol".to_owned(), "sdk-typescript".to_owned()],
            prefix: PathBuf::from("/private/companions"),
            mode: CompanionMode::LocalCandidate,
            catalog: Some(PathBuf::from("/private/catalog.json")),
            local_sources: Some(PathBuf::from("/private/sources.json")),
            node_bin: Some(PathBuf::from("/private/node/bin")),
            ha_config: None,
            edge_target: None,
            edge_go_binary: None,
            edge_tool_root: None,
            timeout_seconds: 90,
        };

        let actual = operation_arguments("dry-run", &arguments, Path::new("/shipped.json"))
            .expect("arguments");
        assert_eq!(
            actual,
            [
                "dry-run",
                "--components",
                "protocol,sdk-typescript",
                "--prefix",
                "/private/companions",
                "--hub-version",
                env!("CARGO_PKG_VERSION"),
                "--mode",
                "local-candidate",
                "--catalog",
                "/private/catalog.json",
                "--local-sources",
                "/private/sources.json",
                "--node-bin",
                "/private/node/bin",
                "--timeout-seconds",
                "90",
            ]
        );
    }

    #[test]
    fn local_candidate_requires_both_explicit_source_inputs() {
        let arguments = CompanionOperationArgs {
            components: vec!["protocol".to_owned()],
            prefix: PathBuf::from("/private/companions"),
            mode: CompanionMode::LocalCandidate,
            catalog: None,
            local_sources: None,
            node_bin: None,
            ha_config: None,
            edge_target: None,
            edge_go_binary: None,
            edge_tool_root: None,
            timeout_seconds: 300,
        };
        let error = operation_arguments("install", &arguments, Path::new("/shipped.json"))
            .expect_err("unbound local source must fail");
        assert!(error.contains("--catalog"));
        assert!(error.contains("--local-sources"));
    }

    #[test]
    fn offline_status_forwards_only_prefix_and_compiled_version() {
        let command = CompanionCommand::Status(CompanionPrefixArgs {
            prefix: PathBuf::from("/private/companions"),
        });
        let actual = command_arguments(&command, Path::new("/shipped.json"))
            .expect("status arguments");
        assert_eq!(
            actual,
            [
                "status",
                "--prefix",
                "/private/companions",
                "--hub-version",
                env!("CARGO_PKG_VERSION"),
            ]
        );
    }

    #[test]
    fn python_310_is_the_minimum_supported_helper_runtime() {
        assert!(!supported_python_version(b"Python 3.9.18\n"));
        assert!(supported_python_version(b"Python 3.10.0\n"));
        assert!(supported_python_version(b"Python 3.14.7\n"));
        assert!(!supported_python_version(b"not python\n"));
    }

    #[test]
    fn production_update_delegates_the_shipped_catalog_to_the_helper() {
        let command = CompanionCommand::Update(CompanionOperationArgs {
            components: vec!["protocol".to_owned()],
            prefix: PathBuf::from("/private/companions"),
            mode: CompanionMode::Production,
            catalog: None,
            local_sources: None,
            node_bin: None,
            ha_config: None,
            edge_target: None,
            edge_go_binary: None,
            edge_tool_root: None,
            timeout_seconds: 300,
        });
        let actual = command_arguments(&command, Path::new("/shipped.json"))
            .expect("update arguments");
        assert_eq!(
            actual,
            [
                "update",
                "--components",
                "protocol",
                "--prefix",
                "/private/companions",
                "--hub-version",
                env!("CARGO_PKG_VERSION"),
                "--mode",
                "production",
                "--catalog",
                "/shipped.json",
                "--timeout-seconds",
                "300",
            ]
        );
    }
}
