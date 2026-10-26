// SPDX-License-Identifier: AGPL-3.0-only
#![cfg(target_os = "macos")]

use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Reap(Child);
impl Drop for Reap {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn startup_compaction(readable: bool) {
    let home = tempfile::tempdir().unwrap();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let logs = home.path().join("Library/Logs/Teslatlas Hub");
    fs::create_dir_all(&logs).unwrap();
    fs::set_permissions(&logs, fs::Permissions::from_mode(0o700)).unwrap();
    let paths = [logs.join("hub.out.log"), logs.join("hub.err.log")];
    let mut expected = Vec::new();
    let mut identities = Vec::new();
    let mut descriptors = Vec::new();
    for (index, (path, len)) in paths
        .iter()
        .zip([1024 * 1024 + 300 * 1024, 2 * 1024 * 1024])
        .enumerate()
    {
        let mut bytes = Vec::with_capacity(len);
        for number in 0.. {
            bytes.extend_from_slice(format!("log-{index}-{number:010}\n").as_bytes());
            if bytes.len() >= len {
                break;
            }
        }
        bytes.truncate(len);
        fs::write(path, &bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        expected.push(bytes[len - 512 * 1024..].to_vec());
        let metadata = fs::metadata(path).unwrap();
        identities.push((metadata.dev(), metadata.ino()));
        descriptors.push(
            OpenOptions::new()
                .read(readable)
                .append(true)
                .mode(0o600)
                .open(path)
                .unwrap(),
        );
    }
    let stderr = descriptors.pop().unwrap();
    let stdout = descriptors.pop().unwrap();
    let mut child = Reap(
        Command::new(env!("CARGO_BIN_EXE_teslatlas-hub"))
            .args(["--config"])
            .arg(home.path().join("absent/config.toml"))
            .arg("serve")
            .env("HOME", home.path())
            .env_remove("TESLATLAS_HUB_DEVELOPMENT")
            .env_remove("TESLATLAS_HUB_DEVELOPMENT_MODE")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .unwrap(),
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "startup timed out"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(1));
    for (index, path) in paths.iter().enumerate() {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!((metadata.dev(), metadata.ino()), identities[index]);
        let bytes = fs::read(path).unwrap();
        assert!(
            bytes.starts_with(&expected[index]),
            "exact retained tail missing"
        );
        assert!(bytes.len() < 1024 * 1024);
        assert!(!bytes.contains(&0), "zero-filled hole");
        if index == 1 {
            let diagnostics = String::from_utf8_lossy(&bytes[512 * 1024..]);
            assert_eq!(
                diagnostics, "teslatlas-hub: cannot safely read configuration\n",
                "ordinary missing-config failure"
            );
        }
    }
    assert!(!home.path().join("absent").exists());
    assert!(!home.path().join("Library/Application Support").exists());
}

#[test]
fn serve_start_compacts_both_oversized_write_only_agent_logs() {
    startup_compaction(false);
}

#[test]
fn serve_start_compacts_both_oversized_read_write_agent_logs() {
    startup_compaction(true);
}
