// SPDX-License-Identifier: AGPL-3.0-only

//! The recovery password for the shipped `backup`, `verify-backup` and
//! `restore-data` commands (M06-T08, repaired in M06-T14). It is never an
//! argument or an environment variable; it is held in wiped memory and never
//! logged, printed or written to a file.
//!
//! On a terminal it is typed at a prompt with echo off. The prompt reads the
//! terminal one byte at a time in non-canonical mode, so the terminal driver
//! can neither drop nor truncate a long line: a line over the cap is read to
//! its end and discarded while echo is still off, then refused. The usual
//! editing keys work (Delete or Backspace, Ctrl-W, Ctrl-U); Ctrl-C, Ctrl-\,
//! Ctrl-Z, Ctrl-Y and Ctrl-D cancel the prompt. The saved terminal settings
//! are restored on every return, and on SIGINT, SIGTERM, SIGHUP or SIGQUIT
//! before the process exits; a restoration that fails is reported.
//!
//! Otherwise the password is read from standard input, unbuffered, in one of
//! two modes:
//! - Line mode, the default: the first line is the password. It ends at the
//!   first LF (one CR before it is dropped) or at the end of the input, and is
//!   taken as soon as that LF arrives; nothing after it is waited for. Input
//!   that has already arrived after the LF is refused rather than ignored. At
//!   most 1 KiB; an empty line, a CR or NUL inside, or bytes that are not
//!   UTF-8 are refused.
//! - Password-file mode, selected by a first line that is exactly
//!   [`PASSWORD_FILE_HEADER`]: the rest of standard input, up to its end, is
//!   the content of a backup password file, read by the rule the service's
//!   schedule applies to that file: at most 1 KiB, one trailing LF (or CRLF)
//!   ignored, UTF-8, with line ends and NUL inside kept. Any password the
//!   schedule accepted can be given this way. The header has fewer than the
//!   12 characters every backup password has, so it is never a password.

use std::{
    fs::{File, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    os::fd::AsFd,
    path::Path,
    process::{Command, Stdio},
    sync::{Mutex, MutexGuard, OnceLock, PoisonError, mpsc},
};

use zeroize::{Zeroize, Zeroizing};

use super::DataRecoveryError;

/// Longest recovery password read, in bytes (the backup password file's cap).
pub const RECOVERY_PASSWORD_MAX_BYTES: usize = 1024;

/// The first line of standard input that selects the password-file mode.
pub const PASSWORD_FILE_HEADER: &str = "#pw-file";

/// Line mode reads at most the password, CR, LF and one byte more: the byte
/// that shows a line is too long.
const LINE_READ_BYTES: usize = RECOVERY_PASSWORD_MAX_BYTES + 3;
/// Password-file mode holds at most the file's cap and one byte more.
const FILE_READ_BYTES: usize = RECOVERY_PASSWORD_MAX_BYTES + 1;

const TERMINAL: &str = "/dev/tty";
const STTY: &str = "/bin/stty";
/// Echo off; no line editing, line limit or signals by the terminal driver;
/// each read returns as soon as one byte is there.
const PROMPT_MODE: [&str; 7] = ["-echo", "-icanon", "-isig", "min", "1", "time", "0"];

/// Delete and Backspace erase one character.
const ERASE: [u8; 2] = [0x7f, 0x08];
/// Ctrl-W erases one word.
const WORD_ERASE: u8 = 0x17;
/// Ctrl-U erases the line.
const KILL: u8 = 0x15;
/// Ctrl-R would reprint the line; with echo off there is nothing to show.
const REPRINT: u8 = 0x12;
/// Ctrl-C, Ctrl-\, Ctrl-Z, Ctrl-Y and Ctrl-D cancel the prompt.
const CANCEL: [u8; 5] = [0x03, 0x1c, 0x1a, 0x19, 0x04];

fn refused(message: &'static str) -> DataRecoveryError {
    DataRecoveryError::PasswordInput(message)
}

fn too_long() -> DataRecoveryError {
    refused("the recovery password is longer than 1 KiB")
}

fn not_restored() -> DataRecoveryError {
    refused(
        "terminal echo could not be restored after the password prompt; run `stty sane` \
         before typing anything else",
    )
}

/// Reads the recovery password for one command: a no-echo prompt when
/// standard input is a terminal, otherwise standard input as described in
/// the module documentation. With `confirm` (a new backup) the prompt asks
/// twice and the two must match.
pub fn read_recovery_password(confirm: bool) -> Result<Zeroizing<String>, DataRecoveryError> {
    if io::stdin().is_terminal() {
        prompt_on_terminal(confirm)
    } else {
        // Unbuffered: the standard library's buffered standard input would
        // keep a copy of the password that is never wiped.
        let input = io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|_| refused("standard input cannot be read"))?;
        recovery_password_from(File::from(input))
    }
}

/// One password from a non-terminal stream, in line mode or, after the
/// [`PASSWORD_FILE_HEADER`] line, in password-file mode.
pub fn recovery_password_from(mut reader: impl Read) -> Result<Zeroizing<String>, DataRecoveryError> {
    let mut buffer = Zeroizing::new(vec![0_u8; LINE_READ_BYTES]);
    let mut filled = 0;
    let line_end = loop {
        if let Some(end) = buffer[..filled].iter().position(|byte| *byte == b'\n') {
            break Some(end);
        }
        if filled == LINE_READ_BYTES {
            return Err(too_long());
        }
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break None,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(refused("standard input cannot be read")),
        }
    };
    let Some(end) = line_end else {
        // The input ended without a line end: all of it is the line.
        return password_from_line(&buffer[..filled]);
    };
    let line = buffer[..end].strip_suffix(b"\r").unwrap_or(&buffer[..end]);
    if line == PASSWORD_FILE_HEADER.as_bytes() {
        return password_file_from(reader, &buffer[end + 1..filled]);
    }
    if line.len() > RECOVERY_PASSWORD_MAX_BYTES {
        return Err(too_long());
    }
    if filled > end + 1 {
        return Err(refused(
            "standard input continues after the password line: send the password as one line, \
             or a backup password file after a `#pw-file` line",
        ));
    }
    password_from_line(line)
}

/// The password-file mode: `already` is what arrived after the header line;
/// the rest of the input follows, up to its end.
fn password_file_from(mut reader: impl Read, already: &[u8]) -> Result<Zeroizing<String>, DataRecoveryError> {
    let mut buffer = Zeroizing::new(vec![0_u8; FILE_READ_BYTES]);
    let mut filled = already.len().min(FILE_READ_BYTES);
    buffer[..filled].copy_from_slice(&already[..filled]);
    loop {
        if filled > RECOVERY_PASSWORD_MAX_BYTES {
            return Err(refused("the password file on standard input is longer than 1 KiB"));
        }
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(refused("standard input cannot be read")),
        }
    }
    let mut content = &buffer[..filled];
    if let Some(rest) = content.strip_suffix(b"\n") {
        content = rest.strip_suffix(b"\r").unwrap_or(rest);
    }
    if content.is_empty() {
        return Err(no_password());
    }
    password_text(content)
}

fn no_password() -> DataRecoveryError {
    refused("no recovery password was given: type it at the prompt, or send it on standard input")
}

/// The line-mode rules for one line without its line end.
fn password_from_line(line: &[u8]) -> Result<Zeroizing<String>, DataRecoveryError> {
    if line.len() > RECOVERY_PASSWORD_MAX_BYTES {
        return Err(too_long());
    }
    if line.is_empty() {
        return Err(no_password());
    }
    if line.iter().any(|byte| matches!(byte, b'\r' | 0)) {
        return Err(refused(
            "the recovery password must be one line without NUL bytes; a password with line \
             ends or NUL bytes is sent as a password file after a `#pw-file` line",
        ));
    }
    password_text(line)
}

/// The password as text, copied once into wiped memory.
fn password_text(bytes: &[u8]) -> Result<Zeroizing<String>, DataRecoveryError> {
    let text = std::str::from_utf8(bytes).map_err(|_| refused("the recovery password is not UTF-8 text"))?;
    let mut password = Zeroizing::new(String::with_capacity(text.len()));
    password.push_str(text);
    Ok(password)
}

fn prompt_on_terminal(confirm: bool) -> Result<Zeroizing<String>, DataRecoveryError> {
    let terminal = OpenOptions::new()
        .read(true)
        .write(true)
        .open(TERMINAL)
        .map_err(|_| refused("the terminal cannot be used for the password prompt"))?;
    let mode = PromptMode::engage(&terminal)?;
    let read = (|| {
        let first = prompt_line(&terminal, "Recovery password: ")?;
        if confirm {
            let second = prompt_line(&terminal, "Repeat the recovery password: ")?;
            if first.as_bytes() != second.as_bytes() {
                return Err(refused("the two recovery passwords differ"));
            }
        }
        Ok(first)
    })();
    // A password read before a failed restoration is wiped, not used.
    let restored = mode.restore();
    restored.and(read)
}

/// One line typed at the prompt, edited and checked; see the module
/// documentation for the keys.
fn prompt_line(terminal: &File, prompt: &str) -> Result<Zeroizing<String>, DataRecoveryError> {
    let mut terminal = terminal;
    terminal
        .write_all(prompt.as_bytes())
        .and_then(|()| terminal.flush())
        .map_err(|_| refused("the terminal cannot be used for the password prompt"))?;
    let mut line = Zeroizing::new(vec![0_u8; RECOVERY_PASSWORD_MAX_BYTES]);
    let mut length = 0_usize;
    let mut overlong = false;
    let mut byte = Zeroizing::new([0_u8; 1]);
    let ended = loop {
        match terminal.read(&mut byte[..]) {
            Ok(0) => break Err(refused("the terminal closed before the password was entered")),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break Err(refused("the terminal cannot be read")),
        }
        let typed = byte[0];
        byte[0] = 0;
        match typed {
            b'\n' | b'\r' => break Ok(()),
            _ if CANCEL.contains(&typed) => break Err(refused("the password prompt was cancelled")),
            KILL => {
                line[..length].zeroize();
                length = 0;
                overlong = false;
            }
            // The rest of an overlong line is read and dropped, unseen.
            _ if overlong => {}
            _ if ERASE.contains(&typed) => length = erase_character(&mut line, length),
            WORD_ERASE => length = erase_word(&mut line, length),
            REPRINT => {}
            _ if length == RECOVERY_PASSWORD_MAX_BYTES => {
                line[..].zeroize();
                length = 0;
                overlong = true;
            }
            _ => {
                line[length] = typed;
                length += 1;
            }
        }
    };
    // Echo is off, so the line end the user typed was not shown.
    let _ = terminal.write_all(b"\n");
    ended?;
    if overlong {
        return Err(too_long());
    }
    password_from_line(&line[..length])
}

/// Removes the last UTF-8 character of `line[..length]`; the new length.
fn erase_character(line: &mut [u8], mut length: usize) -> usize {
    while length > 0 {
        length -= 1;
        let removed = line[length];
        line[length] = 0;
        if removed & 0xC0 != 0x80 {
            break;
        }
    }
    length
}

/// Removes trailing blanks, then the word before them; the new length.
fn erase_word(line: &mut [u8], mut length: usize) -> usize {
    while length > 0 && matches!(line[length - 1], b' ' | b'\t') {
        length -= 1;
        line[length] = 0;
    }
    while length > 0 && !matches!(line[length - 1], b' ' | b'\t') {
        length -= 1;
        line[length] = 0;
    }
    length
}

/// The terminal settings to restore if the process is stopped by a signal
/// while a prompt is active.
struct ActivePrompt {
    terminal: File,
    saved: String,
}

static ACTIVE_PROMPT: Mutex<Option<ActivePrompt>> = Mutex::new(None);
static SIGNAL_WATCHER: OnceLock<bool> = OnceLock::new();

fn active_prompt() -> MutexGuard<'static, Option<ActivePrompt>> {
    ACTIVE_PROMPT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The prompt's terminal mode. Engaging saves the settings, makes sure a
/// stopping signal restores them, then turns echo off; [`Self::restore`]
/// puts the saved settings back and reports a failure. Dropping restores
/// too (on a panic), reporting a failure on standard error.
struct PromptMode<'a> {
    terminal: &'a File,
    saved: Option<String>,
}

impl<'a> PromptMode<'a> {
    fn engage(terminal: &'a File) -> Result<Self, DataRecoveryError> {
        watch_stopping_signals()?;
        let saved = stty(terminal, &["-g"])
            .ok()
            .and_then(|output| String::from_utf8(output).ok())
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
            .ok_or_else(|| refused("the terminal settings cannot be read"))?;
        let held = terminal
            .try_clone()
            .map_err(|_| refused("the terminal cannot be used for the password prompt"))?;
        #[cfg(test)]
        test_seam::before_activation_lock();
        // Held from publishing the saved settings until the mode-changing
        // child has exited: a stopping signal's restoration takes the same
        // lock, so it waits for that child and only then puts the saved
        // settings back. No mode change can land after the restoration, and
        // the process cannot end between the two. A signal handled first
        // keeps the lock until the process has ended, so this waits for
        // that end and no mode change starts at all.
        let mut active = active_prompt();
        *active = Some(ActivePrompt {
            terminal: held,
            saved: saved.clone(),
        });
        if change_to_prompt_mode(terminal).is_err() {
            let restored = restore_settings(terminal, &saved);
            *active = None;
            drop(active);
            restored?;
            return Err(refused("terminal echo cannot be turned off for the password prompt"));
        }
        drop(active);
        Ok(Self {
            terminal,
            saved: Some(saved),
        })
    }

    fn restore(mut self) -> Result<(), DataRecoveryError> {
        self.restore_now()
    }

    fn restore_now(&mut self) -> Result<(), DataRecoveryError> {
        let Some(saved) = self.saved.take() else {
            return Ok(());
        };
        // Held while restoring: a signal arriving now waits, then finds
        // nothing left to restore.
        let mut active = active_prompt();
        let restored = restore_settings(self.terminal, &saved);
        *active = None;
        restored
    }
}

impl Drop for PromptMode<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.restore_now() {
            let _ = writeln!(io::stderr(), "teslatlas-hub: {error}");
        }
    }
}

fn restore_settings(terminal: &File, saved: &str) -> Result<(), DataRecoveryError> {
    #[cfg(test)]
    let saved = if test_seam::restore_fails() {
        "teslatlas-test-unknown-setting"
    } else {
        saved
    };
    stty(terminal, &[saved]).map(drop).map_err(|()| not_restored())
}

/// Turns echo, canonical input and signals off; returns once the child that
/// changes the mode has exited.
fn change_to_prompt_mode(terminal: &File) -> Result<(), ()> {
    #[cfg(test)]
    if let Some(delay) = test_seam::activation_delay() {
        return test_seam::delayed_mode_change(terminal, delay, &PROMPT_MODE);
    }
    stty(terminal, &PROMPT_MODE).map(drop)
}

/// Runs `stty` on `terminal`; its output, or `Err` when it cannot run or
/// fails. Its error output is discarded.
fn stty(terminal: &File, arguments: &[&str]) -> Result<Vec<u8>, ()> {
    let input = terminal.try_clone().map_err(drop)?;
    let output = Command::new(Path::new(STTY))
        .args(arguments)
        .stdin(Stdio::from(input))
        .stderr(Stdio::null())
        .output()
        .map_err(drop)?;
    if output.status.success() { Ok(output.stdout) } else { Err(()) }
}

/// Starts, once per process, the thread that restores an active prompt's
/// terminal settings on SIGINT, SIGTERM, SIGHUP or SIGQUIT and then ends the
/// process with the shell's status for that signal (128 + its number). The
/// handlers are installed before the first prompt turns echo off, and they
/// stay for the life of the process (the runtime cannot remove them), so the
/// thread keeps ending the process on those signals after the prompt too.
fn watch_stopping_signals() -> Result<(), DataRecoveryError> {
    let ready = *SIGNAL_WATCHER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("recovery-password-signals".to_owned())
            .spawn(move || watch(&sender))
            .is_ok()
            && receiver.recv().unwrap_or(false)
    });
    if ready {
        Ok(())
    } else {
        Err(refused(
            "the password prompt cannot protect the terminal settings if the command is stopped",
        ))
    }
}

fn watch(ready: &mpsc::Sender<bool>) {
    use tokio::signal::unix::{SignalKind, signal};

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_io().build() else {
        let _ = ready.send(false);
        return;
    };
    runtime.block_on(async {
        let (Ok(mut interrupt), Ok(mut terminate), Ok(mut hangup), Ok(mut quit)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
            signal(SignalKind::quit()),
        ) else {
            let _ = ready.send(false);
            return;
        };
        let _ = ready.send(true);
        let status = tokio::select! {
            _ = interrupt.recv() => 130,
            _ = terminate.recv() => 143,
            _ = hangup.recv() => 129,
            _ = quit.recv() => 131,
        };
        restore_and_exit(status)
    });
}

/// Restores an active prompt's terminal settings, then ends the process
/// with `status`. The prompt lock is taken first and never released: the
/// process ends holding it, so a prompt that starts after the signal cannot
/// switch the hidden-typing mode on in the moment before the exit
/// (REV2-CX-M06-T10 finding 11). Its activation waits on the lock until the
/// process has gone.
fn restore_and_exit(status: i32) -> ! {
    let mut active = active_prompt();
    if let Some(prompt) = active.take() {
        let mut terminal = &prompt.terminal;
        let _ = terminal.write_all(b"\n");
        if let Err(error) = restore_settings(&prompt.terminal, &prompt.saved) {
            let _ = writeln!(io::stderr(), "teslatlas-hub: {error}");
        }
    }
    #[cfg(test)]
    test_seam::after_signal_restoration();
    // `exit` does not return, so `active` is never dropped and the lock
    // stays held until the process has ended.
    std::process::exit(status)
}

#[cfg(test)]
pub(crate) mod test_seam {
    use std::{
        fs::File,
        path::Path,
        process::{Command, Stdio},
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
        time::Duration,
    };

    static RESTORE_FAILS: AtomicBool = AtomicBool::new(false);
    static ACTIVATION_DELAY_MS: AtomicU64 = AtomicU64::new(0);
    static EXIT_DELAY_MS: AtomicU64 = AtomicU64::new(0);
    static SIGNAL_RESTORED: AtomicBool = AtomicBool::new(false);

    /// Raises SIGTERM after the prompt has read the terminal settings and
    /// before it takes the prompt lock, waits until the signal thread has
    /// finished its restoration, then lets the prompt take the lock; the
    /// signal thread waits `exit_delay` before it ends the process (the PTY
    /// child process only). REV2-CX-M06-T10 finding 11, made deterministic:
    /// a signal handled before activation must keep activation out until
    /// the process has ended.
    pub(crate) fn race_termination_before_activation_lock(exit_delay: Duration) {
        let millis = u64::try_from(exit_delay.as_millis()).unwrap_or(u64::MAX).max(1);
        EXIT_DELAY_MS.store(millis, Ordering::SeqCst);
    }

    pub(super) fn before_activation_lock() {
        if EXIT_DELAY_MS.load(Ordering::SeqCst) == 0 {
            return;
        }
        let _ = rustix::process::kill_process(
            rustix::process::getpid(),
            rustix::process::Signal::TERM,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !SIGNAL_RESTORED.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(super) fn after_signal_restoration() {
        SIGNAL_RESTORED.store(true, Ordering::SeqCst);
        match EXIT_DELAY_MS.load(Ordering::SeqCst) {
            0 => {}
            millis => std::thread::sleep(Duration::from_millis(millis)),
        }
    }

    /// Makes the prompt's mode change slow and sends this process SIGTERM
    /// while that change is pending (the PTY child process only): the
    /// activation race of REV2-CX-M06-T08 finding 6, made deterministic.
    pub(crate) fn race_activation_with_termination(delay: Duration) {
        let millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX).max(1);
        ACTIVATION_DELAY_MS.store(millis, Ordering::SeqCst);
    }

    pub(super) fn activation_delay() -> Option<Duration> {
        match ACTIVATION_DELAY_MS.load(Ordering::SeqCst) {
            0 => None,
            millis => Some(Duration::from_millis(millis)),
        }
    }

    /// Starts a shell that waits `delay`, then runs `stty` with `arguments`
    /// on `terminal`; signals SIGTERM to this process once the shell is
    /// running, then waits for the shell to exit.
    pub(super) fn delayed_mode_change(
        terminal: &File,
        delay: Duration,
        arguments: &[&str],
    ) -> Result<(), ()> {
        let input = terminal.try_clone().map_err(drop)?;
        let seconds = format!("{}.{:03}", delay.as_secs(), delay.subsec_millis());
        let mut child = Command::new(Path::new("/bin/sh"))
            .args(["-c", "sleep \"$1\"; shift; exec /bin/stty \"$@\"", "sh", &seconds])
            .args(arguments)
            .stdin(Stdio::from(input))
            .stderr(Stdio::null())
            .spawn()
            .map_err(drop)?;
        let _ = rustix::process::kill_process(
            rustix::process::getpid(),
            rustix::process::Signal::TERM,
        );
        let status = child.wait().map_err(drop)?;
        if status.success() { Ok(()) } else { Err(()) }
    }

    /// Makes every later restoration of saved terminal settings in this
    /// process fail (the PTY child process only).
    pub(crate) fn fail_restore() {
        RESTORE_FAILS.store(true, Ordering::SeqCst);
    }

    pub(super) fn restore_fails() -> bool {
        RESTORE_FAILS.load(Ordering::SeqCst)
    }
}
