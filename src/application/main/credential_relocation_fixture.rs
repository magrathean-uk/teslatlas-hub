// SPDX-License-Identifier: AGPL-3.0-only

//! The feature boundary runs before configuration discovery and operational
//! dispatch. All paths and the service identity come from the explicit context.

use super::*;
use rustix::fs::{fstat as descriptor_stat, openat};
use serde::Deserialize;
use std::{ffi::OsString, fs::File, os::fd::OwnedFd};
use teslatlas_hub::platform::r78_recording::{
    FixtureBackendGuard, RecordingBackend, RecordingState, SyntheticServiceIdentity,
    with_fixture_backend,
};

const CONTRACT_DIGEST: &str = "607c465b60da6019ae61e93c794d61e12aaa0e2cab762b283004ee1ff9f363ed";
const CONTEXT_CAP: u64 = 65_536;

#[derive(Debug)]
pub(super) struct Refusal(pub &'static str);
impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for Refusal {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    contract: String,
    contract_version: u32,
    contract_digest: String,
    run_id: String,
    executable_kind: String,
    synthetic_root: PathBuf,
    data_dir: PathBuf,
    config_path: PathBuf,
    session_dir: PathBuf,
    recording_state_path: PathBuf,
    trace_path: PathBuf,
    service_identity: SyntheticServiceIdentity,
    permitted_endpoints: Vec<String>,
    permitted_faults: Vec<String>,
}

pub(super) enum Prepared {
    Capability,
    Operation(Operation),
}

pub(super) struct Operation {
    cli: Cli,
    context: Context,
    backend: RecordingBackend,
    root_descriptor: OwnedFd,
}

fn reject(code: &'static str) -> Refusal {
    Refusal(code)
}
fn malformed<T>(_: T) -> Refusal {
    reject("fixture_context_malformed")
}
fn unsafe_context<T>(_: T) -> Refusal {
    reject("fixture_context_unsafe")
}
fn path_escape<T>(_: T) -> Refusal {
    reject("fixture_path_escape")
}

fn lexical(path: &Path) -> Result<(), Refusal> {
    let text = path.to_str().ok_or_else(|| reject("fixture_path_escape"))?;
    if !path.is_absolute() || text.split('/').any(|part| part == "." || part == "..") {
        return Err(reject("fixture_path_escape"));
    }
    Ok(())
}

fn root_descriptor(root: &Path) -> Result<OwnedFd, Refusal> {
    lexical(root)?;
    if root == Path::new("/") {
        return Err(reject("fixture_context_unsafe"));
    }
    let mut directory = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(unsafe_context)?;
    for component in root.components().skip(1) {
        directory = openat(
            &directory,
            component.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(path_escape)?;
    }
    let stat = descriptor_stat(&directory).map_err(unsafe_context)?;
    if stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o777 != 0o700
        || fs::canonicalize(root).map_err(path_escape)?.as_os_str() != root.as_os_str()
    {
        return Err(reject("fixture_context_unsafe"));
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if home.starts_with(root) || root.starts_with(home.join("Library")) {
            return Err(reject("fixture_context_unsafe"));
        }
    }
    Ok(directory)
}

fn parent_beneath(
    root: &Path,
    descriptor: &OwnedFd,
    path: &Path,
) -> Result<(OwnedFd, OsString), Refusal> {
    lexical(path)?;
    let relative = path.strip_prefix(root).map_err(path_escape)?;
    let mut components = relative.components().peekable();
    let mut directory = rustix::io::dup(descriptor).map_err(path_escape)?;
    loop {
        let component = components
            .next()
            .ok_or_else(|| reject("fixture_path_escape"))?;
        if components.peek().is_none() {
            return Ok((directory, component.as_os_str().to_owned()));
        }
        directory = openat(
            &directory,
            component.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(path_escape)?;
    }
}

fn contained_directory(root: &Path, descriptor: &OwnedFd, path: &Path) -> Result<(), Refusal> {
    let (parent, name) = parent_beneath(root, descriptor, path)?;
    let child = openat(
        &parent,
        &name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(path_escape)?;
    let stat = descriptor_stat(&child).map_err(path_escape)?;
    if stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(reject("fixture_context_unsafe"));
    }
    Ok(())
}

fn contained_file(
    root: &Path,
    descriptor: &OwnedFd,
    path: &Path,
    writable: bool,
    exclusive: bool,
) -> Result<File, Refusal> {
    let (parent, name) = parent_beneath(root, descriptor, path)?;
    let flags = (if writable {
        OFlags::RDWR
    } else {
        OFlags::RDONLY
    }) | OFlags::NOFOLLOW
        | OFlags::NONBLOCK
        | OFlags::CLOEXEC
        | if exclusive {
            OFlags::CREATE | OFlags::EXCL
        } else {
            OFlags::empty()
        };
    let file = openat(&parent, &name, flags, Mode::RUSR | Mode::WUSR).map_err(path_escape)?;
    let stat = descriptor_stat(&file).map_err(path_escape)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_nlink != 1
        || stat.st_uid != rustix::process::geteuid().as_raw()
    {
        return Err(reject("fixture_context_unsafe"));
    }
    Ok(File::from(file))
}

fn bounded_bytes(mut file: File) -> Result<Vec<u8>, Refusal> {
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(CONTEXT_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(malformed)?;
    if bytes.len() as u64 > CONTEXT_CAP {
        return Err(reject("fixture_context_malformed"));
    }
    Ok(bytes)
}

pub(super) fn prepare(arguments: Vec<OsString>) -> Result<Prepared, Refusal> {
    prepare_for_kind(arguments, "fixture")
}

fn prepare_for_kind(arguments: Vec<OsString>, kind: &str) -> Result<Prepared, Refusal> {
    if arguments.len() == 2 && arguments[1] == "--fixture-capability" {
        return Ok(Prepared::Capability);
    }
    if arguments.len() < 4 || arguments[1] != "--fixture-context" {
        return Err(reject("fixture_context_missing"));
    }
    let path = PathBuf::from(&arguments[2]);
    lexical(&path)?;
    // Determine only the explicit context's parent. No home/config discovery.
    let root = path
        .parent()
        .ok_or_else(|| reject("fixture_context_unsafe"))?;
    if path.file_name() != Some(std::ffi::OsStr::new("fixture-context.json")) {
        return Err(reject("fixture_context_mismatch"));
    }
    let descriptor = root_descriptor(root)?;
    if let Err(error) = fs::symlink_metadata(&path) {
        return Err(reject(if error.kind() == std::io::ErrorKind::NotFound {
            "fixture_context_missing"
        } else {
            "fixture_context_unsafe"
        }));
    }
    let file = contained_file(root, &descriptor, &path, false, false)?;
    let mode = file.metadata().map_err(unsafe_context)?.mode() & 0o777;
    if mode != 0o600 && mode != 0o400 {
        return Err(reject("fixture_context_unsafe"));
    }
    let context: Context = serde_json::from_slice(&bounded_bytes(file)?).map_err(malformed)?;
    if context.contract != "teslatlas-hub credential-relocation-fixture"
        || context.contract_version != 1
        || context.contract_digest != CONTRACT_DIGEST
        || context.synthetic_root != root
        || context.executable_kind != kind
    {
        return Err(reject("fixture_context_mismatch"));
    }
    if context.run_id.len() != 32
        || !context
            .run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !context.permitted_endpoints.is_empty()
        || (kind == "fixture" && !context.permitted_faults.is_empty())
        || (kind == "fault" && context.permitted_faults != ["durability_probe_failure"])
    {
        return Err(reject("fixture_context_malformed"));
    }
    if context.service_identity.label != format!("fixture.teslatlas-relocation.{}", context.run_id)
        || context.service_identity.domain != format!("fixture/{}", context.run_id)
        || context.service_identity.plist_path
            != root
                .join("service")
                .join(format!("{}.plist", context.service_identity.label))
    {
        return Err(reject("fixture_owner_identity_refused"));
    }
    let marker = contained_file(root, &descriptor, &root.join(".fixture-run"), false, false)?;
    if bounded_bytes(marker)? != format!("{}\n", context.run_id).as_bytes() {
        return Err(reject("fixture_context_mismatch"));
    }
    contained_directory(root, &descriptor, &context.data_dir)?;
    contained_directory(root, &descriptor, &context.session_dir)?;
    let _config_file = contained_file(root, &descriptor, &context.config_path, false, false)?;
    let _plist_file = contained_file(
        root,
        &descriptor,
        &context.service_identity.plist_path,
        false,
        false,
    )?;
    let state_file = contained_file(
        root,
        &descriptor,
        &context.recording_state_path,
        true,
        false,
    )?;
    let state: RecordingState =
        serde_json::from_slice(&bounded_bytes(state_file.try_clone().map_err(malformed)?)?)
            .map_err(malformed)?;
    if state
        .pid
        .is_some_and(|pid| !(900_000_000..=999_999_999).contains(&pid))
    {
        return Err(reject("fixture_owner_identity_refused"));
    }
    let tail: Vec<&std::ffi::OsStr> = arguments[3..].iter().map(OsString::as_os_str).collect();
    let allowed = tail == [std::ffi::OsStr::new("status")]
        || tail == [std::ffi::OsStr::new("bootstrap")]
        || tail
            == [
                std::ffi::OsStr::new("control"),
                std::ffi::OsStr::new("sign-out"),
            ];
    if !allowed {
        return Err(reject("fixture_command_refused"));
    }
    let config = HubConfig::load(&context.config_path).map_err(malformed)?;
    if config.data_dir != context.data_dir {
        return Err(reject("fixture_context_mismatch"));
    }
    let mut parse_arguments = vec![arguments[0].clone()];
    parse_arguments.extend_from_slice(&arguments[3..]);
    let mut cli = Cli::try_parse_from(parse_arguments).map_err(malformed)?;
    cli.config = Some(context.config_path.clone());
    let trace = contained_file(root, &descriptor, &context.trace_path, true, true)?;
    let backend = RecordingBackend::open(
        &context.data_dir,
        context.service_identity.clone(),
        state,
        state_file,
        trace,
        &context.run_id,
    )
    .map_err(|_| reject("fixture_owner_identity_refused"))?;
    Ok(Prepared::Operation(Operation {
        cli,
        context,
        backend,
        root_descriptor: descriptor,
    }))
}

thread_local! {
    static ACTIVE_PATHS: std::cell::RefCell<Option<(PathBuf, PathBuf)>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn require_dispatch(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    with_fixture_backend(|_| Ok(()))?;
    let valid = ACTIVE_PATHS.with(|paths| {
        paths
            .borrow()
            .as_ref()
            .is_some_and(|(_, config)| cli.config.as_ref() == Some(config))
    });
    if !valid
        || !matches!(
            &cli.command,
            Command::Status
                | Command::Bootstrap
                | Command::Control {
                    command: ControlCommand::SignOut,
                    vehicle_id: None
                }
        )
    {
        return Err(reject("fixture_command_refused").into());
    }
    Ok(())
}

pub(super) fn require_entry_path(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    with_fixture_backend(|_| Ok(()))?;
    if !ACTIVE_PATHS.with(|paths| {
        paths
            .borrow()
            .as_ref()
            .is_some_and(|(data, _)| data == path)
    }) {
        return Err(reject("fixture_context_mismatch").into());
    }
    Ok(())
}

pub(super) async fn execute(operation: Operation) -> Result<(), Box<dyn std::error::Error>> {
    let Operation {
        cli,
        context,
        backend,
        root_descriptor,
    } = operation;
    contained_directory(&context.synthetic_root, &root_descriptor, &context.data_dir)?;
    let _config = contained_file(
        &context.synthetic_root,
        &root_descriptor,
        &context.config_path,
        false,
        false,
    )?;
    let mut guard = FixtureBackendGuard::install(backend)?;
    struct PathsReset;
    impl Drop for PathsReset {
        fn drop(&mut self) {
            ACTIVE_PATHS.with(|paths| paths.borrow_mut().take());
        }
    }
    ACTIVE_PATHS.with(|paths| *paths.borrow_mut() = Some((context.data_dir, context.config_path)));
    let _paths = PathsReset;
    let trace_failure = std::rc::Rc::new(std::cell::Cell::new(false));
    let failed = std::rc::Rc::clone(&trace_failure);
    let _faults = teslatlas_hub::CredentialVaultEntry::test_faults(
        context.permitted_faults == ["durability_probe_failure"],
        None,
        None,
        Box::new(move |point| {
            if point == "exclusion_verified"
                && with_fixture_backend(|backend| backend.observe("secrets_exclusion")).is_err()
            {
                failed.set(true);
            }
        }),
    );
    let result = run(cli).await;
    guard.finish()?;
    if trace_failure.get() {
        return Err(reject("fixture_backend_missing").into());
    }
    result
}

pub(super) async fn entry() -> ExitCode {
    match prepare(std::env::args_os().collect()) {
        Ok(Prepared::Capability) => {
            println!(
                "{}",
                serde_json::json!({"fixture": "credential-relocation-fixture", "contractVersion": 1, "backend": "recording", "executableKind": "fixture"})
            );
            ExitCode::SUCCESS
        }
        Ok(Prepared::Operation(operation)) => {
            let result = execute(operation).await;
            let refusal = result.as_ref().err().and_then(|error| {
                if let Some(refusal) = error.downcast_ref::<Refusal>() {
                    return Some(refusal.0);
                }
                match error.to_string().as_str() {
                    "fixture_backend_missing" => Some("fixture_backend_missing"),
                    "fixture_command_refused" => Some("fixture_command_refused"),
                    "fixture_child_refused" => Some("fixture_child_refused"),
                    _ => None,
                }
            });
            if let Some(reason) = refusal {
                println!(
                    "{}",
                    serde_json::json!({"status": "fixture_refused", "reason": reason})
                );
                ExitCode::from(78)
            } else {
                ExitCode::from(report_outcome(result, &mut std::io::stderr()))
            }
        }
        Err(refusal) => {
            println!(
                "{}",
                serde_json::json!({"status": "fixture_refused", "reason": refusal.0})
            );
            ExitCode::from(78)
        }
    }
}

#[cfg(test)]
pub(super) fn prepare_fault_for_test(arguments: Vec<OsString>) -> Result<Prepared, Refusal> {
    prepare_for_kind(arguments, "fault")
}
