// SPDX-License-Identifier: AGPL-3.0-only

//! The recovery password for the shipped `backup`, `verify-backup` and
//! `restore-data` commands (M06-T08). It is never an argument or an
//! environment variable: on a terminal it is typed at a prompt with echo off,
//! otherwise it is read from standard input. It is held in wiped memory and
//! never logged or printed.

use std::{
    fs::{File, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    path::Path,
    process::{Command, Stdio},
};

use zeroize::Zeroizing;

use super::DataRecoveryError;

/// Longest recovery password read, in bytes (the backup password file's cap).
pub const RECOVERY_PASSWORD_MAX_BYTES: usize = 1024;

const TERMINAL: &str = "/dev/tty";
const STTY: &str = "/bin/stty";

fn refused(message: &'static str) -> DataRecoveryError {
    DataRecoveryError::PasswordInput(message)
}

/// Reads the recovery password for one command: a no-echo prompt when
/// standard input is a terminal, otherwise one line from standard input.
/// With `confirm` (a new backup) the prompt asks twice and the two must match.
pub fn read_recovery_password(confirm: bool) -> Result<Zeroizing<String>, DataRecoveryError> {
    if io::stdin().is_terminal() {
        prompt_without_echo(confirm)
    } else {
        recovery_password_from(io::stdin().lock())
    }
}

/// One password from a non-terminal stream: UTF-8, one line, at most
/// [`RECOVERY_PASSWORD_MAX_BYTES`]; one trailing line end is ignored and
/// nothing else is trimmed.
pub fn recovery_password_from(reader: impl Read) -> Result<Zeroizing<String>, DataRecoveryError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(RECOVERY_PASSWORD_MAX_BYTES + 2));
    reader
        .take(u64::try_from(RECOVERY_PASSWORD_MAX_BYTES + 2).expect("cap fits"))
        .read_to_end(&mut bytes)
        .map_err(|_| refused("standard input cannot be read"))?;
    password_from_line(bytes)
}

fn password_from_line(mut bytes: Zeroizing<Vec<u8>>) -> Result<Zeroizing<String>, DataRecoveryError> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > RECOVERY_PASSWORD_MAX_BYTES {
        return Err(refused("the recovery password is longer than 1 KiB"));
    }
    if bytes.is_empty() {
        return Err(refused(
            "no recovery password was given: type it at the prompt, or send it on standard input",
        ));
    }
    if bytes.iter().any(|byte| matches!(byte, b'\n' | b'\r' | 0)) {
        return Err(refused("the recovery password must be one line"));
    }
    match String::from_utf8(std::mem::take(&mut *bytes)) {
        Ok(text) => Ok(Zeroizing::new(text)),
        Err(error) => {
            // Wipe the bytes the failed conversion hands back.
            drop(Zeroizing::new(error.into_bytes()));
            Err(refused("the recovery password is not UTF-8 text"))
        }
    }
}

/// Turns terminal echo off for its lifetime and restores the saved terminal
/// settings when dropped, also on an error path.
struct EchoOff<'a> {
    terminal: &'a File,
    saved: String,
}

impl<'a> EchoOff<'a> {
    fn new(terminal: &'a File) -> Result<Self, DataRecoveryError> {
        let saved = stty(terminal, &["-g"])?;
        let saved = String::from_utf8(saved)
            .map_err(|_| refused("the terminal settings cannot be read"))?
            .trim()
            .to_owned();
        if saved.is_empty() {
            return Err(refused("the terminal settings cannot be read"));
        }
        stty(terminal, &["-echo"])?;
        Ok(Self { terminal, saved })
    }
}

impl Drop for EchoOff<'_> {
    fn drop(&mut self) {
        let _ = stty(self.terminal, &[self.saved.as_str()]);
    }
}

fn stty(terminal: &File, arguments: &[&str]) -> Result<Vec<u8>, DataRecoveryError> {
    let input = terminal
        .try_clone()
        .map_err(|_| refused("the terminal cannot be used for the password prompt"))?;
    let output = Command::new(Path::new(STTY))
        .args(arguments)
        .stdin(Stdio::from(input))
        .stderr(Stdio::null())
        .output()
        .map_err(|_| refused("terminal echo cannot be turned off for the password prompt"))?;
    if !output.status.success() {
        return Err(refused("terminal echo cannot be turned off for the password prompt"));
    }
    Ok(output.stdout)
}

fn prompt_without_echo(confirm: bool) -> Result<Zeroizing<String>, DataRecoveryError> {
    let terminal = OpenOptions::new()
        .read(true)
        .write(true)
        .open(TERMINAL)
        .map_err(|_| refused("the terminal cannot be used for the password prompt"))?;
    let echo = EchoOff::new(&terminal)?;
    let first = prompt_line(&terminal, "Recovery password: ")?;
    if confirm {
        let second = prompt_line(&terminal, "Repeat the recovery password: ")?;
        if first.as_bytes() != second.as_bytes() {
            return Err(refused("the two recovery passwords differ"));
        }
    }
    drop(echo);
    Ok(first)
}

fn prompt_line(terminal: &File, prompt: &str) -> Result<Zeroizing<String>, DataRecoveryError> {
    let mut terminal = terminal;
    terminal
        .write_all(prompt.as_bytes())
        .and_then(|()| terminal.flush())
        .map_err(|_| refused("the terminal cannot be used for the password prompt"))?;
    let mut line = Zeroizing::new(Vec::with_capacity(64));
    let mut byte = [0_u8; 1];
    loop {
        match terminal.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                line.push(byte[0]);
                byte[0] = 0;
                if line.last() == Some(&b'\n') || line.len() > RECOVERY_PASSWORD_MAX_BYTES + 1 {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(refused("the terminal cannot be read")),
        }
    }
    // Echo was off, so the line end the user typed was not shown.
    let _ = terminal.write_all(b"\n");
    password_from_line(line)
}
