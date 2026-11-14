// SPDX-License-Identifier: AGPL-3.0-only

//! M06-T08: the recovery password read by the shipped commands, and the
//! admission that refuses an older plaintext directory backup unless that
//! format is named explicitly.

use super::*;

fn password(bytes: &[u8]) -> Result<String, DataRecoveryError> {
    recovery_password_from(bytes).map(|password| password.as_str().to_owned())
}

#[test]
fn data_recovery_password_takes_one_line_and_ignores_one_line_end() {
    assert_eq!(password(b"correct horse\n").unwrap(), "correct horse");
    assert_eq!(password(b"correct horse\r\n").unwrap(), "correct horse");
    assert_eq!(password(b"correct horse").unwrap(), "correct horse");
    assert_eq!(password(b"  spaced  \n").unwrap(), "  spaced  ", "nothing else is trimmed");
    let longest = "x".repeat(RECOVERY_PASSWORD_MAX_BYTES);
    assert_eq!(password(format!("{longest}\n").as_bytes()).unwrap(), longest);
}

#[test]
fn data_recovery_password_refuses_empty_long_multiline_and_non_text_input() {
    let inputs: [&[u8]; 7] = [
        b"",
        b"\n",
        b"\r\n",
        b"one\ntwo\n",
        b"one\rtwo",
        b"nul\0byte",
        &[0xff, 0xfe, b'\n'],
    ];
    for input in inputs {
        let error = password(input).expect_err("refused");
        assert!(
            matches!(error, DataRecoveryError::PasswordInput(_)),
            "{input:?}: {error}"
        );
    }
    let too_long = "x".repeat(RECOVERY_PASSWORD_MAX_BYTES + 1);
    let error = password(too_long.as_bytes()).expect_err("over the cap");
    assert!(error.to_string().contains("longer than 1 KiB"), "{error}");
    assert!(!error.to_string().contains(&too_long), "the password is never echoed");
}

#[test]
fn data_recovery_plaintext_directory_source_needs_the_explicit_format() {
    let root = tempfile::tempdir().expect("temporary root");
    let directory = root.path().join("generation");
    fs::create_dir(&directory).expect("directory");
    let bundle = root.path().join("teslatlas-hub-backup-x.tlbk");
    fs::write(&bundle, b"bundle").expect("bundle");

    let error = admit_recovery_source(&directory, false).expect_err("refused by default");
    assert!(
        matches!(error, DataRecoveryError::PlaintextDirectoryRefused { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("--plaintext-directory"), "{error}");
    assert_eq!(
        admit_recovery_source(&directory, true).expect("named explicitly"),
        BackupSourceFormat::PlaintextDirectory
    );
    assert_eq!(
        admit_recovery_source(&bundle, false).expect("encrypted by default"),
        BackupSourceFormat::Encrypted
    );
    admit_recovery_source(&bundle, true).expect_err("the flag never names a bundle");
    admit_recovery_source(&root.path().join("absent"), false).expect_err("absent source");
}
