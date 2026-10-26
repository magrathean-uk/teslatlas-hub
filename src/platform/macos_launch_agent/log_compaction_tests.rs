// SPDX-License-Identifier: AGPL-3.0-only

use super::super::*;
use InheritedLogCompaction::*;
use std::{
    fs::OpenOptions,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
};

struct Logs {
    _root: tempfile::TempDir,
    paths: [PathBuf; 2],
    original: Vec<u8>,
}
impl Logs {
    fn new(len: usize) -> Self {
        let root = crate::private_tempdir().unwrap();
        let directory = root.path().join("Library/Logs/Teslatlas Hub");
        fs::create_dir_all(&directory).unwrap();
        let paths = [directory.join("hub.out.log"), directory.join("hub.err.log")];
        let mut original = Vec::with_capacity(len);
        for line in 0.. {
            original
                .extend_from_slice(format!("{line:010}: numbered diagnostic line\n").as_bytes());
            if original.len() >= len {
                break;
            }
        }
        original.truncate(len);
        fs::write(&paths[0], &original).unwrap();
        fs::set_permissions(&paths[0], fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            _root: root,
            paths,
            original,
        }
    }
    fn refs(&self) -> Option<[&Path; 2]> {
        Some([&self.paths[0], &self.paths[1]])
    }
    fn open(&self, append: bool, read: bool) -> File {
        let mut file = OpenOptions::new()
            .write(true)
            .read(read)
            .append(append)
            .mode(0o600)
            .open(&self.paths[0])
            .unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        file
    }
    fn tail(&self) -> &[u8] {
        &self.original[self.original.len() - 512 * 1024..]
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.paths[0]).unwrap()
    }
}
fn identity(file: &File) -> (u64, u64) {
    let metadata = file.metadata().unwrap();
    (metadata.dev(), metadata.ino())
}
fn compact_stdout(file: &File, paths: Option<[&Path; 2]>) -> InheritedLogCompaction {
    let null = File::open("/dev/null").unwrap();
    compact_inherited_log_descriptors(file.as_fd(), null.as_fd(), paths)[0]
}
fn assert_append(readable: bool) {
    let logs = Logs::new(2 * 1024 * 1024);
    let mut file = logs.open(true, readable);
    let before = identity(&file);
    assert_eq!(compact_stdout(&file, logs.refs()), Compacted);
    assert_eq!(identity(&File::open(&logs.paths[0]).unwrap()), before);
    assert!(logs.bytes() == logs.tail(), "exact log contents differ");
    file.write_all(b"after\n").unwrap();
    assert!(
        logs.bytes() == [logs.tail(), b"after\n"].concat(),
        "exact log contents differ"
    );
}
#[test]
fn inherited_log_write_only_append_keeps_last_512_kib_in_place() {
    assert_append(false);
}
#[test]
fn inherited_log_read_write_append_keeps_last_512_kib_in_place() {
    assert_append(true);
}
#[test]
fn inherited_log_without_append_continues_after_kept_bytes() {
    let logs = Logs::new(2 * 1024 * 1024);
    let mut file = logs.open(false, false);
    assert_eq!(file.stream_position().unwrap(), 2 * 1024 * 1024);
    assert_eq!(compact_stdout(&file, logs.refs()), Compacted);
    file.write_all(b"following\n").unwrap();
    assert!(
        logs.bytes() == [logs.tail(), b"following\n"].concat(),
        "exact log contents differ"
    );
}
#[test]
fn inherited_log_at_or_below_threshold_is_untouched() {
    for len in [1024 * 1024, 10] {
        let logs = Logs::new(len);
        let file = logs.open(true, false);
        let before = file.metadata().unwrap();
        assert_eq!(compact_stdout(&file, logs.refs()), BelowThreshold);
        let after = file.metadata().unwrap();
        assert!(logs.bytes() == logs.original, "exact log contents differ");
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mtime(),
                before.mtime_nsec()
            ),
            (after.dev(), after.ino(), after.mtime(), after.mtime_nsec())
        );
    }
}
#[test]
fn non_regular_descriptors_are_untouched() {
    let (pipe, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let null = File::open("/dev/null").unwrap();
    assert_eq!(
        compact_inherited_log_descriptors(pipe.as_fd(), null.as_fd(), None),
        [NotRegularFile, NotRegularFile]
    );
    peer.write_all(b"still open").unwrap();
    // Also exercise an actual pipe, without a rustix feature addition or unsafe.
    let mut child = std::process::Command::new("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let result = compact_inherited_log_descriptors(input.as_fd(), output.as_fd(), None);
    drop(input);
    drop(output);
    let status = child.wait().unwrap();
    assert!(status.success());
    assert_eq!(result, [NotRegularFile, NotRegularFile]);
}
#[test]
fn shared_log_descriptor_is_compacted_once() {
    let logs = Logs::new(2 * 1024 * 1024);
    let stdout = logs.open(true, false);
    let stderr = logs.open(true, false);
    assert_eq!(
        compact_inherited_log_descriptors(stdout.as_fd(), stderr.as_fd(), logs.refs()),
        [Compacted, SameFileAsStdout]
    );
    assert!(logs.bytes() == logs.tail(), "exact log contents differ");
}
#[test]
fn shared_log_non_append_descriptors_resume_at_compacted_eof() {
    let logs = Logs::new(2 * 1024 * 1024);
    let mut stdout = logs.open(false, false);
    let mut stderr = logs.open(false, false);
    let before = identity(&stdout);
    assert_eq!(
        compact_inherited_log_descriptors(stdout.as_fd(), stderr.as_fd(), logs.refs()),
        [Compacted, SameFileAsStdout]
    );
    assert_eq!(stdout.stream_position().unwrap(), 512 * 1024);
    assert_eq!(stderr.stream_position().unwrap(), 512 * 1024);
    assert_eq!(identity(&File::open(&logs.paths[0]).unwrap()), before);
    assert!(logs.bytes() == logs.tail(), "exact log contents differ");
    stdout.write_all(b"OUT!\n").unwrap();
    assert!(
        logs.bytes() == [logs.tail(), b"OUT!\n"].concat(),
        "exact log contents differ"
    );
    stderr.write_all(b"ERR!\n").unwrap();
    assert!(
        logs.bytes() == [logs.tail(), b"ERR!\n"].concat(),
        "exact log contents differ"
    );
}
#[test]
fn shared_log_append_stdout_aligns_non_append_stderr() {
    let logs = Logs::new(2 * 1024 * 1024);
    let stdout = logs.open(true, false);
    let mut stderr = logs.open(false, false);
    assert_eq!(
        compact_inherited_log_descriptors(stdout.as_fd(), stderr.as_fd(), logs.refs()),
        [Compacted, SameFileAsStdout]
    );
    assert_eq!(stderr.stream_position().unwrap(), 512 * 1024);
    stderr.write_all(b"ERR\n").unwrap();
    assert!(
        logs.bytes() == [logs.tail(), b"ERR\n"].concat(),
        "exact log contents differ"
    );
    // A duplicated descriptor also shares the repaired offset.
    let mut duplicate = stderr.try_clone().unwrap();
    assert_eq!(duplicate.stream_position().unwrap(), 512 * 1024 + 4);
}
#[test]
fn shared_log_alignment_failure_is_reported_after_compaction() {
    for fail_flags in [true, false] {
        let logs = Logs::new(2 * 1024 * 1024);
        let stdout = logs.open(true, false);
        let stderr = logs.open(false, false);
        let mut calls = Vec::new();
        let result = compact_inherited_log_descriptors_with(
            stdout.as_fd(),
            stderr.as_fd(),
            logs.refs(),
            &mut |len, file| {
                calls.push("compact");
                compact_in_place(len, file)
            },
            &mut |file| {
                align_shared_log_stderr_with(
                    file,
                    |_| {
                        if fail_flags {
                            Err(io::Error::other("flag inspection"))
                        } else {
                            Ok(false)
                        }
                    },
                    |_| Err(io::Error::other("alignment seek")),
                )
            },
        );
        assert_eq!(result, [Compacted, FailedAfterTruncate]);
        assert!(logs.bytes() == logs.tail(), "exact log contents differ");
        assert_eq!(calls, ["compact"]);
    }
}

struct FaultingFile<'a> {
    file: &'a mut InheritedLogFile,
    fail: &'static str,
    calls: Vec<&'static str>,
}
impl InheritedLogIo for FaultingFile<'_> {
    fn read_tail(&mut self, kept: &mut [u8], offset: u64) -> io::Result<()> {
        self.calls.push("read");
        self.file.read_tail(kept, offset)
    }
    fn prepare_write(&mut self) -> io::Result<()> {
        self.calls.push("prepare");
        self.file.prepare_write()
    }
    fn truncate(&mut self) -> io::Result<()> {
        self.calls.push("truncate");
        if self.fail == "truncate" {
            return Err(io::Error::other("truncate failed"));
        }
        self.file.truncate()
    }
    fn reposition_after_failed_truncate(&mut self) {
        self.calls.push("reposition");
        self.file.reposition_after_failed_truncate();
    }
    fn write_kept(&mut self, kept: &[u8]) -> io::Result<()> {
        self.calls.push("write");
        if self.fail == "write" {
            self.file.log.write_all(&kept[..257])?;
            return Err(io::Error::other("partial write"));
        }
        self.file.write_kept(kept)
    }
    fn sync(&mut self) -> io::Result<()> {
        self.calls.push("sync");
        if self.fail == "sync" {
            return Err(io::Error::other("sync failed"));
        }
        self.file.sync()
    }
}
fn shared_stdout_fault(fail: &'static str, retained: usize) {
    let logs = Logs::new(2 * 1024 * 1024);
    let stdout = logs.open(false, false);
    let mut stderr = logs.open(false, false);
    let mut calls = Vec::new();
    let result = compact_inherited_log_descriptors_with(
        stdout.as_fd(),
        stderr.as_fd(),
        logs.refs(),
        &mut |len, file| {
            let mut fault = FaultingFile {
                file,
                fail,
                calls: vec![],
            };
            let result = compact_in_place(len, &mut fault);
            calls.push(fault.calls);
            result
        },
        &mut align_shared_log_stderr,
    );
    assert_eq!(result, [FailedAfterTruncate, FailedAfterTruncate]);
    assert_eq!(calls.len(), 1, "no second compaction or retry");
    assert_eq!(stderr.stream_position().unwrap(), retained as u64);
    assert!(
        logs.bytes() == logs.tail()[..retained],
        "exact log contents differ"
    );
    stderr.write_all(b"after-error\n").unwrap();
    assert_eq!(
        logs.bytes(),
        [&logs.tail()[..retained], b"after-error\n"].concat()
    );
}
#[test]
fn shared_log_partial_stdout_write_aligns_non_append_stderr() {
    shared_stdout_fault("write", 257);
}
#[test]
fn shared_log_stdout_sync_failure_aligns_non_append_stderr() {
    shared_stdout_fault("sync", 512 * 1024);
}
#[test]
fn only_agent_logs_are_compacted() {
    let logs = Logs::new(2 * 1024 * 1024);
    let file = logs.open(true, false);
    assert_eq!(compact_stdout(&file, None), NotAgentLog);
    let unrelated = logs._root.path().join("unrelated.log");
    let unrelated_bytes = vec![b'X'; 2 * 1024 * 1024];
    fs::write(&unrelated, &unrelated_bytes).unwrap();
    assert_eq!(
        compact_stdout(&file, Some([&unrelated, &logs.paths[1]])),
        NotAgentLog
    );
    assert!(logs.bytes() == logs.original, "exact log contents differ");
    assert_eq!(fs::read(&unrelated).unwrap(), unrelated_bytes);
    assert_eq!(
        compact_stdout(&file, Some([&logs.paths[1], &logs.paths[0]])),
        Compacted
    );
}
#[test]
fn symlinked_agent_log_is_not_followed() {
    let logs = Logs::new(2 * 1024 * 1024);
    let file = logs.open(true, false);
    let target = logs._root.path().join("target.log");
    fs::rename(&logs.paths[0], &target).unwrap();
    symlink(&target, &logs.paths[0]).unwrap();
    assert_eq!(compact_stdout(&file, logs.refs()), NotAgentLog);
    assert_eq!(fs::read(target).unwrap(), logs.original);
}
#[test]
fn fifo_at_agent_log_path_does_not_block() {
    let logs = Logs::new(2 * 1024 * 1024);
    let file = logs.open(true, false);
    let fifo = logs._root.path().join("fifo");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .args(["-m", "600"])
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        compact_stdout(&file, Some([&fifo, &logs.paths[1]])),
        NotAgentLog
    );
    assert!(logs.bytes() == logs.original, "exact log contents differ");
}
#[test]
fn truncate_failure_after_seek_returns_to_end() {
    let logs = Logs::new(2 * 1024 * 1024);
    let mut original = logs.open(false, false);
    let mut file = InheritedLogFile {
        log: original.try_clone().unwrap(),
        reader: File::open(&logs.paths[0]).unwrap(),
        append: false,
        position_for_write: |file| file.seek(SeekFrom::Start(0)).map(|_| ()),
    };
    let mut fault = FaultingFile {
        file: &mut file,
        fail: "truncate",
        calls: vec![],
    };
    assert_eq!(
        compact_in_place(2 * 1024 * 1024, &mut fault),
        FailedBeforeChange
    );
    assert_eq!(fault.calls, ["read", "prepare", "truncate", "reposition"]);
    assert!(logs.bytes() == logs.original, "exact log contents differ");
    assert_eq!(original.stream_position().unwrap(), 2 * 1024 * 1024);
    original.write_all(b"after\n").unwrap();
    assert!(
        logs.bytes() == [&logs.original[..], b"after\n"].concat(),
        "exact log contents differ"
    );
}
#[test]
fn shared_log_truncate_failure_restores_duplicated_offsets() {
    let logs = Logs::new(2 * 1024 * 1024);
    let mut stdout = logs.open(false, false);
    let mut stderr = stdout.try_clone().unwrap();
    let mut calls = Vec::new();
    let result = compact_inherited_log_descriptors_with(
        stdout.as_fd(),
        stderr.as_fd(),
        logs.refs(),
        &mut |len, file| {
            let mut fault = FaultingFile {
                file,
                fail: "truncate",
                calls: vec![],
            };
            let result = compact_in_place(len, &mut fault);
            calls.push(fault.calls);
            result
        },
        &mut align_shared_log_stderr,
    );
    assert_eq!(result, [FailedBeforeChange, SameFileAsStdout]);
    assert_eq!(calls, [vec!["read", "prepare", "truncate", "reposition"]]);
    assert_eq!(stdout.stream_position().unwrap(), 2 * 1024 * 1024);
    assert_eq!(stderr.stream_position().unwrap(), 2 * 1024 * 1024);
    assert!(logs.bytes() == logs.original, "exact log contents differ");
    stdout.write_all(b"OUT!\n").unwrap();
    stderr.write_all(b"ERR!\n").unwrap();
    let bytes = logs.bytes();
    assert!(
        bytes == [&logs.original[..], b"OUT!\n", b"ERR!\n"].concat(),
        "exact log contents differ"
    );
    assert!(!bytes.contains(&0), "zero-filled hole");
}
#[test]
fn read_only_descriptor_leaves_log_unchanged() {
    let logs = Logs::new(2 * 1024 * 1024);
    let mut file = File::open(&logs.paths[0]).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    assert_eq!(compact_stdout(&file, logs.refs()), FailedBeforeChange);
    assert!(logs.bytes() == logs.original, "exact log contents differ");
    assert_eq!(file.stream_position().unwrap(), 2 * 1024 * 1024);
}

struct RecordingIo {
    contents: Vec<u8>,
    calls: Vec<&'static str>,
    fail: Option<&'static str>,
    offset: Option<u64>,
}
impl RecordingIo {
    fn new(fail: Option<&'static str>) -> Self {
        Self {
            contents: vec![b'x'; 2 * 1024 * 1024],
            calls: vec![],
            fail,
            offset: None,
        }
    }
    fn step(&mut self, name: &'static str) -> io::Result<()> {
        self.calls.push(name);
        if self.fail == Some(name) {
            Err(io::Error::other(name))
        } else {
            Ok(())
        }
    }
}
impl InheritedLogIo for RecordingIo {
    fn read_tail(&mut self, kept: &mut [u8], offset: u64) -> io::Result<()> {
        self.offset = Some(offset);
        assert_eq!(kept.len(), 512 * 1024);
        self.step("ReadTail")?;
        kept.copy_from_slice(&self.contents[offset as usize..]);
        Ok(())
    }
    fn prepare_write(&mut self) -> io::Result<()> {
        self.step("PrepareWrite")
    }
    fn truncate(&mut self) -> io::Result<()> {
        self.step("Truncate")?;
        self.contents.clear();
        Ok(())
    }
    fn reposition_after_failed_truncate(&mut self) {
        self.calls.push("Reposition");
    }
    fn write_kept(&mut self, kept: &[u8]) -> io::Result<()> {
        if self.fail == Some("WriteKept") {
            self.contents.extend_from_slice(&kept[..257]);
        }
        self.step("WriteKept")?;
        self.contents.extend_from_slice(kept);
        Ok(())
    }
    fn sync(&mut self) -> io::Result<()> {
        self.step("Sync")
    }
}
#[test]
fn compaction_steps_run_in_order() {
    let mut file = RecordingIo::new(None);
    assert_eq!(compact_in_place(2 * 1024 * 1024, &mut file), Compacted);
    assert_eq!(
        file.calls,
        ["ReadTail", "PrepareWrite", "Truncate", "WriteKept", "Sync"],
        "successful compaction must not reposition"
    );
    assert_eq!(file.offset, Some(1536 * 1024));
    assert!(
        file.contents == vec![b'x'; 512 * 1024],
        "exact file state differs"
    );
}
#[test]
fn compaction_read_failure_changes_nothing() {
    let mut file = RecordingIo::new(Some("ReadTail"));
    assert_eq!(
        compact_in_place(2 * 1024 * 1024, &mut file),
        FailedBeforeChange
    );
    assert_eq!(file.calls, ["ReadTail"], "failed read must not reposition");
    assert!(
        file.contents == vec![b'x'; 2 * 1024 * 1024],
        "exact file state differs"
    );
}
#[test]
fn compaction_non_append_seek_failure_precedes_truncation() {
    let logs = Logs::new(2 * 1024 * 1024);
    let file = logs.open(false, false);
    let mut io = InheritedLogFile {
        log: file,
        reader: File::open(&logs.paths[0]).unwrap(),
        append: false,
        position_for_write: |file| {
            assert_eq!(file.stream_position().unwrap(), 2 * 1024 * 1024);
            Err(io::Error::other("seek failed"))
        },
    };
    assert_eq!(io.log.stream_position().unwrap(), 2 * 1024 * 1024);
    let mut fault = FaultingFile {
        file: &mut io,
        fail: "position",
        calls: vec![],
    };
    assert_eq!(
        compact_in_place(2 * 1024 * 1024, &mut fault),
        FailedBeforeChange
    );
    assert_eq!(
        fault.calls,
        ["read", "prepare"],
        "failed seek must not reposition"
    );
    assert!(logs.bytes() == logs.original, "exact log contents differ");
    assert_eq!(io.log.stream_position().unwrap(), 2 * 1024 * 1024);
}
#[test]
fn compaction_truncate_failure_changes_nothing() {
    let mut file = RecordingIo::new(Some("Truncate"));
    assert_eq!(
        compact_in_place(2 * 1024 * 1024, &mut file),
        FailedBeforeChange
    );
    assert_eq!(
        file.calls,
        ["ReadTail", "PrepareWrite", "Truncate", "Reposition"]
    );
    assert!(
        file.contents == vec![b'x'; 2 * 1024 * 1024],
        "exact file state differs"
    );
}
#[test]
fn compaction_write_failure_is_reported_after_truncation() {
    let mut file = RecordingIo::new(Some("WriteKept"));
    assert_eq!(
        compact_in_place(2 * 1024 * 1024, &mut file),
        FailedAfterTruncate
    );
    assert_eq!(
        file.calls,
        ["ReadTail", "PrepareWrite", "Truncate", "WriteKept"]
    );
    assert!(file.contents == vec![b'x'; 257], "exact file state differs");
}
#[test]
fn compaction_sync_failure_is_reported_after_truncation() {
    let mut file = RecordingIo::new(Some("Sync"));
    assert_eq!(
        compact_in_place(2 * 1024 * 1024, &mut file),
        FailedAfterTruncate
    );
    assert_eq!(
        file.calls,
        ["ReadTail", "PrepareWrite", "Truncate", "WriteKept", "Sync"]
    );
    assert!(
        file.contents == vec![b'x'; 512 * 1024],
        "exact file state differs"
    );
}
#[test]
fn agent_log_paths_are_fixed() {
    assert_eq!(
        inherited_agent_log_paths(Some(Path::new("/Users/example"))),
        Some([
            PathBuf::from("/Users/example/Library/Logs/Teslatlas Hub/hub.out.log"),
            PathBuf::from("/Users/example/Library/Logs/Teslatlas Hub/hub.err.log"),
        ])
    );
    assert_eq!(inherited_agent_log_paths(None), None);
}
