// SPDX-License-Identifier: AGPL-3.0-only

// M06-T14: REV-CX-M06-T08 findings 1, 2, 3, 5 and 6, recovery side. These
// tests use only interfaces that existed at 07c87b9 and fail there when run:
// the password-file mode (finding 1), a stable destination link (finding 2),
// an overlong terminal line, Ctrl-C and SIGTERM at the prompt (findings 3 and
// 6) and a line sent through a pipe that stays open (finding 5). Synthetic
// passwords only; no real password, terminal or person is involved.
//
// The terminal tests run this test binary on a pseudo-terminal through
// `/usr/bin/script` (no window, nothing takes focus). The child reports what
// it read as a SHA-256 digest, never as text, and the parent asserts that the
// typed password never appears in the terminal output, that the next reader
// of the terminal receives none of a refused line, and that `stty -a` shows
// echo, canonical input and signals back on after the child has gone.

use std::{
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use sha2::Sha256;

use super::*;
use crate::{
    data_recovery::{
        EncryptedBackupRequest, archive_crypto::KdfParams, create_encrypted_backup,
        encrypted_backup::clear_hook, read_recovery_password,
    },
    runtime::backup_schedule::password::read_password_file,
};

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};
/// The first line that selects the password-file mode, written out so the
/// tests state the contract rather than borrowing it.
const FILE_HEADER: &[u8] = b"#pw-file\n";

fn password(bytes: &[u8]) -> Result<String, String> {
    recovery_password_from(bytes)
        .map(|password| password.as_str().to_owned())
        .map_err(|error| error.to_string())
}

fn owner_only_file(path: &Path, body: &[u8]) {
    fs::write(path, body).expect("password file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("mode 0600");
}

/// Passwords the scheduled backup accepts today that the line mode cannot
/// carry: line ends and NUL inside, a kept trailing line end, a bare CR.
pub(super) const SCHEDULED_PASSWORD_FILES: [&[u8]; 6] = [
    b"first line of it\nsecond line of it\n",
    b"carriage\rreturn inside, no line end",
    b"nul\0byte inside the password\r\n",
    b"keeps one line end of two\n\n",
    b"ends in a bare carriage return\r",
    "non-ASCII \u{00e9}t\u{00e9} \u{2603} password\n".as_bytes(),
];

/// Finding 1: every password file the schedule reads gives the same password
/// through the password-file mode of standard input, byte for byte.
#[test]
fn data_recovery_f1_password_file_mode_reads_what_the_schedule_accepts() {
    let directory = crate::private_tempdir().expect("private directory");
    let longest = vec![b'p'; RECOVERY_PASSWORD_MAX_BYTES];
    let mut bodies: Vec<&[u8]> = SCHEDULED_PASSWORD_FILES.to_vec();
    bodies.push(&longest);
    for (index, body) in bodies.into_iter().enumerate() {
        let file = directory.path().join(format!("password-{index}"));
        owner_only_file(&file, body);
        let scheduled = read_password_file(&file).expect("the schedule accepts this file");
        for header in [FILE_HEADER, b"#pw-file\r\n".as_slice()] {
            let mut input = header.to_vec();
            input.extend_from_slice(body);
            let recovered = recovery_password_from(input.as_slice())
                .unwrap_or_else(|error| panic!("password file {index} refused: {error}"));
            assert!(
                recovered.as_bytes() == scheduled.as_bytes(),
                "password file {index} must give the schedule's password byte for byte"
            );
        }
    }
    // The schedule's cap applies: a file over 1 KiB is refused by both.
    let over = directory.path().join("over");
    owner_only_file(&over, &vec![b'p'; RECOVERY_PASSWORD_MAX_BYTES + 1]);
    read_password_file(&over).expect_err("the schedule refuses it");
    let mut input = FILE_HEADER.to_vec();
    input.extend(std::iter::repeat_n(b'p', RECOVERY_PASSWORD_MAX_BYTES + 1));
    let error = password(&input).expect_err("over the cap");
    assert!(error.contains("1 KiB"), "{error}");
    // Nothing after the header is no password.
    let error = password(FILE_HEADER).expect_err("empty");
    assert!(error.contains("no recovery password"), "{error}");
}

/// Reads a password from a pipe whose writer stays open after `bytes`.
/// `Err` when the reader is still waiting after ten seconds.
fn read_from_open_pipe(bytes: &[u8]) -> Result<Result<String, String>, &'static str> {
    let (reader, mut writer) = std::io::pipe().expect("pipe");
    writer.write_all(bytes).expect("written");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = recovery_password_from(reader)
            .map(|password| password.as_str().to_owned())
            .map_err(|error| error.to_string());
        let _ = sender.send(outcome);
    });
    let outcome = receiver.recv_timeout(Duration::from_secs(10));
    drop(writer);
    outcome.map_err(|_| "still waiting for the end of standard input")
}

/// Finding 5: one line through a pipe that stays open is read as soon as its
/// line end arrives, also when the line is empty; input that arrived with the
/// line after its end is refused (the stated trailing-input rule).
#[test]
fn data_recovery_f5_piped_line_is_read_while_the_writer_stays_open() {
    assert_eq!(
        read_from_open_pipe(b"correct horse battery staple\n"),
        Ok(Ok(PASSWORD.to_owned()))
    );
    assert_eq!(
        read_from_open_pipe(b"correct horse battery staple\r\n"),
        Ok(Ok(PASSWORD.to_owned()))
    );
    let empty = read_from_open_pipe(b"\n").expect("an empty line is answered at once");
    assert!(empty.expect_err("refused").contains("no recovery password"));
    let trailing = read_from_open_pipe(b"correct horse battery staple\nmore input\n")
        .expect("answered at once")
        .expect_err("input after the line is refused");
    assert!(trailing.contains("after the password line"), "{trailing}");
    assert!(!trailing.contains("correct horse"), "the password is never echoed");
    let long = read_from_open_pipe(&vec![b'x'; RECOVERY_PASSWORD_MAX_BYTES + 100])
        .expect("an overlong line is refused without waiting")
        .expect_err("refused");
    assert!(long.contains("longer than 1 KiB"), "{long}");
}

/// Finding 2: a destination that is itself a symbolic link to a private
/// folder is refused, and nothing is written behind it.
#[test]
fn data_recovery_f2_stable_destination_link_is_refused() {
    clear_hook();
    let (temporary, store) = create_fixture();
    let target = temporary.path().join("private-target");
    fs::create_dir(&target).expect("target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).expect("private");
    let link = temporary.path().join("destination-link");
    std::os::unix::fs::symlink(&target, &link).expect("link");
    let mut request = EncryptedBackupRequest::new(&link, PASSWORD);
    request.kdf = TEST_KDF;
    let refused = create_encrypted_backup(&store, &request);
    assert!(refused.is_err(), "a destination link must be refused: {refused:?}");
    assert_eq!(fs::read_dir(&target).expect("listing").count(), 0, "nothing written");
    // The folder itself is admitted.
    let mut request = EncryptedBackupRequest::new(&target, PASSWORD);
    request.kdf = TEST_KDF;
    create_encrypted_backup(&store, &request).expect("the folder itself");
}

// The terminal tests (macOS: `/usr/bin/script -q /dev/null command...`).

/// Names the scenario for the child; holds no password.
pub(super) const PTY_SCENARIO: &str = "TESLATLAS_M06_T14_PTY_SCENARIO";
/// How long the parent waits for any one step of the child.
const PTY_STEP: Duration = Duration::from_secs(60);
/// Run by the shell on the pseudo-terminal: the child, then its exit status,
/// the terminal settings it left and one line read by the next reader.
const PTY_SHELL: &str = "\"$@\"; status=$?; printf '\\nCHILD-STATUS %s\\n' \"$status\"; \
    /bin/stty -a; printf 'NEXT-READ\\n'; IFS= read -r line; printf 'NEXT-LINE[%s]\\n' \"$line\"";

/// The child: reads one recovery password from its terminal through the
/// shipped entry point and reports the outcome. Outside the harness (no
/// scenario variable) it does nothing.
#[test]
fn data_recovery_pty_child_reads_one_terminal_password() {
    let Ok(scenario) = std::env::var(PTY_SCENARIO) else {
        return;
    };
    println!("CHILD-PID {}", std::process::id());
    pty_child_report(read_recovery_password(scenario == "confirm"));
}

pub(super) fn pty_child_report(outcome: Result<zeroize::Zeroizing<String>, DataRecoveryError>) {
    match outcome {
        Ok(password) => println!(
            "OUTCOME accepted {} {}",
            password.len(),
            hex::encode(Sha256::digest(password.as_bytes()))
        ),
        Err(error) => println!("OUTCOME refused {error}"),
    }
}

pub(super) struct Pty {
    script: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
}

impl Pty {
    /// Starts the child test `child` (its path below the data-recovery
    /// `tests` module) with `scenario` on a fresh pseudo-terminal.
    pub(super) fn start(child: &str, scenario: &str) -> Self {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, rest)| rest);
        let parent = module.rsplit_once("::").map_or(module, |(parent, _)| parent);
        let test = format!("{parent}::{child}");
        let mut script = Command::new("/usr/bin/script")
            .args(["-q", "/dev/null", "/bin/sh", "-c", PTY_SHELL, "sh"])
            .arg(std::env::current_exe().expect("this test binary"))
            .args(["--exact", &test, "--nocapture", "--test-threads", "1"])
            .env(PTY_SCENARIO, scenario)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("script(1) starts the pseudo-terminal");
        let input = script.stdin.take();
        let mut stdout = script.stdout.take().expect("terminal output");
        let (sender, output) = mpsc::channel();
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(count) = stdout.read(&mut chunk) {
                if count == 0 || sender.send(chunk[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            script,
            input,
            output,
            seen: Vec::new(),
        }
    }

    pub(super) fn send(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().expect("terminal input");
        input.write_all(bytes).and_then(|()| input.flush()).expect("typed");
    }

    /// Waits until the terminal output holds `needle` after byte `from`;
    /// returns where it ends, or `None` after [`PTY_STEP`].
    pub(super) fn wait_for(&mut self, needle: &str, from: usize) -> Option<usize> {
        let deadline = Instant::now() + PTY_STEP;
        loop {
            if let Some(at) = find(&self.seen[from.min(self.seen.len())..], needle.as_bytes()) {
                return Some(from + at + needle.len());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            match self.output.recv_timeout(left) {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(_) => return None,
            }
        }
    }

    /// The text after `marker ` up to the line end, once it has arrived.
    pub(super) fn line_after(&mut self, marker: &str) -> Option<String> {
        let start = self.wait_for(marker, 0)?;
        let end = self.wait_for("\n", start)?;
        Some(String::from_utf8_lossy(&self.seen[start..end]).trim().to_owned())
    }

    /// Closes the terminal input and collects the rest of the output, for
    /// at most `within`.
    fn drain(&mut self, within: Duration) -> String {
        drop(self.input.take());
        let deadline = Instant::now() + within;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.output.recv_timeout(left) {
                Ok(chunk) => self.seen.extend_from_slice(&chunk),
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    /// After the child and its shell are done: everything shown.
    pub(super) fn finish(mut self) -> String {
        self.drain(PTY_STEP)
    }

    /// Fails the test with what the terminal showed so far.
    pub(super) fn fail(&mut self, step: &str) -> ! {
        let _ = self.script.kill();
        let shown = self.drain(Duration::from_secs(5));
        panic!("{step}; the terminal showed:\n{shown}");
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.script.kill();
        let _ = self.script.wait();
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// After the child has gone: echo, canonical input and signals are on again.
pub(super) fn assert_terminal_restored(shown: &str) {
    let settings: Vec<&str> = shown.split_whitespace().collect();
    for setting in ["echo", "icanon", "isig"] {
        assert!(
            settings.contains(&setting) && !settings.contains(&format!("-{setting}").as_str()),
            "`{setting}` must be restored; the terminal showed:\n{shown}"
        );
    }
}

/// Lets the shell's next reader finish, then returns everything shown.
pub(super) fn finish_with_next_line(mut pty: Pty) -> String {
    if pty.wait_for("NEXT-READ", 0).is_none() {
        pty.fail("the child did not finish");
    }
    pty.send(b"after\n");
    if pty.wait_for("NEXT-LINE[", 0).is_none() {
        pty.fail("the next reader did not finish");
    }
    pty.finish()
}

pub(super) fn wait_for_prompt(pty: &mut Pty) {
    if pty.wait_for("Recovery password: ", 0).is_none() {
        pty.fail("no password prompt");
    }
}

/// The child test of this module.
const CHILD: &str = "m06_t14::data_recovery_pty_child_reads_one_terminal_password";

/// Finding 3: a line longer than the cap is read to its end and discarded
/// with echo off, then refused; the next reader of the terminal gets none of
/// it, and nothing of it was shown.
#[cfg(target_os = "macos")]
#[test]
fn data_recovery_f3_pty_overlong_line_is_discarded_with_echo_off() {
    let mut pty = Pty::start(CHILD, "single");
    wait_for_prompt(&mut pty);
    let line = "Qz7-".repeat(450);
    pty.send(line.as_bytes());
    pty.send(b"\n");
    let shown = finish_with_next_line(pty);
    assert!(shown.contains("OUTCOME refused"), "{shown}");
    assert!(shown.contains("the recovery password is longer than 1 KiB"), "{shown}");
    assert!(!shown.contains("Qz7-Qz7-"), "nothing of the line is echoed:\n{shown}");
    assert!(
        shown.contains("NEXT-LINE[after]"),
        "no byte of the refused line is left for the next reader:\n{shown}"
    );
    assert_terminal_restored(&shown);
}

/// Finding 3: the longest password the cap allows is read whole on a
/// terminal, never truncated by the terminal's line limit.
#[cfg(target_os = "macos")]
#[test]
fn data_recovery_f3_pty_longest_password_is_read_whole() {
    let mut pty = Pty::start(CHILD, "single");
    wait_for_prompt(&mut pty);
    let longest = "k".repeat(RECOVERY_PASSWORD_MAX_BYTES);
    pty.send(longest.as_bytes());
    pty.send(b"\n");
    let shown = finish_with_next_line(pty);
    let expected = format!(
        "OUTCOME accepted {} {}",
        longest.len(),
        hex::encode(Sha256::digest(longest.as_bytes()))
    );
    assert!(shown.contains(&expected), "{shown}");
    assert!(!shown.contains("kkkkkkkk"), "nothing is echoed:\n{shown}");
    assert_terminal_restored(&shown);
}

/// Finding 6: Ctrl-C at the prompt cancels it and restores the terminal.
#[cfg(target_os = "macos")]
#[test]
fn data_recovery_f6_pty_ctrl_c_restores_the_terminal() {
    let mut pty = Pty::start(CHILD, "single");
    wait_for_prompt(&mut pty);
    pty.send(b"partial secret");
    pty.send(&[0x03]);
    let shown = finish_with_next_line(pty);
    assert!(shown.contains("OUTCOME refused"), "{shown}");
    assert!(shown.contains("cancelled"), "{shown}");
    assert!(!shown.contains("partial secret"), "nothing is echoed:\n{shown}");
    assert!(shown.contains("NEXT-LINE[after]"), "{shown}");
    assert_terminal_restored(&shown);
}

/// Finding 6: SIGTERM while the prompt waits restores the terminal before
/// the process ends.
#[cfg(target_os = "macos")]
#[test]
fn data_recovery_f6_pty_termination_restores_the_terminal() {
    let mut pty = Pty::start(CHILD, "single");
    let Some(pid) = pty.line_after("CHILD-PID ") else {
        pty.fail("no child process identifier");
    };
    wait_for_prompt(&mut pty);
    pty.send(b"partial secret");
    let pid: i32 = pid.parse().expect("process identifier");
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid).expect("positive"),
        rustix::process::Signal::TERM,
    )
    .expect("SIGTERM sent");
    let Some(status) = pty.line_after("CHILD-STATUS ") else {
        pty.fail("the child did not end");
    };
    assert_eq!(status, "143", "ended by SIGTERM");
    let shown = finish_with_next_line(pty);
    assert!(!shown.contains("partial secret"), "nothing is echoed:\n{shown}");
    assert_terminal_restored(&shown);
}
