#![forbid(unsafe_code)]

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use tempfile::TempDir;

    fn guard() -> String {
        format!(
            "{}/scripts/run-with-spool-format-guard.sh",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn launch_guard_blocks_pre_v2_binary_from_v2_spool() {
        let temp = TempDir::new().unwrap();
        let spool = temp.path().join("spool");
        fs::create_dir(&spool).unwrap();
        fs::write(spool.join("FORMAT"), b"2\n").unwrap();
        let launched = temp.path().join("launched");
        let legacy = temp.path().join("legacy-edge");
        fs::write(
            &legacy,
            b"#!/bin/sh\nif [ \"$1\" = storage-format ]; then exit 64; fi\nprintf launched > \"$1\"\n",
        )
        .unwrap();
        fs::set_permissions(&legacy, fs::Permissions::from_mode(0o755)).unwrap();

        let output = Command::new(guard())
            .arg(&legacy)
            .arg(&spool)
            .arg("--")
            .arg(&launched)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!launched.exists());

        fs::remove_file(spool.join("FORMAT")).unwrap();
        let output = Command::new(guard())
            .arg(&legacy)
            .arg(&spool)
            .arg("--")
            .arg(&launched)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(fs::read(launched).unwrap(), b"launched");
    }

    #[test]
    fn current_binary_reports_and_accepts_v2_spool_format() {
        let temp = TempDir::new().unwrap();
        let spool = temp.path().join("spool");
        fs::create_dir(&spool).unwrap();
        fs::write(spool.join("FORMAT"), b"2\n").unwrap();

        let output = Command::new(guard())
            .arg(env!("CARGO_BIN_EXE_teslatlas-edge"))
            .arg(&spool)
            .arg("--")
            .arg("storage-format")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"2\n");
        assert!(output.stderr.is_empty());
    }
}
