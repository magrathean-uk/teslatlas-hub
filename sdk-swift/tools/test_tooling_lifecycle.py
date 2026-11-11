"""Pure regressions for repaired tooling functions; no child/socket/Git runs."""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import matrix_wire
import owned_command
import platform_gate
import source_handoff


class PublicationTests(unittest.TestCase):
    def test_final_name_appears_only_after_complete_json_and_cannot_replace(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            root.chmod(0o700)
            target = root / "ready.json"
            original_write = os.write
            observations = []

            def write_part(descriptor, data):
                observations.append(target.exists())
                return original_write(descriptor, data[:3])

            parent = lambda path, label: (os.open(root, os.O_RDONLY | os.O_DIRECTORY), target.name)
            with mock.patch.object(matrix_wire, "_open_private_parent", side_effect=parent), \
                 mock.patch.object(matrix_wire.os, "write", side_effect=write_part):
                binding = matrix_wire.write_exclusive_json(target, {"status": "complete", "sequence": 1})
            self.assertGreater(len(observations), 1)
            self.assertFalse(any(observations))
            self.assertEqual({"status": "complete", "sequence": 1}, json.loads(target.read_bytes()))
            self.assertEqual(str(target.resolve()), binding["path"])
            original = target.read_bytes()
            with mock.patch.object(matrix_wire, "_open_private_parent", side_effect=parent):
                with self.assertRaises(matrix_wire.MatrixWireError):
                    matrix_wire.write_exclusive_json(target, {"sequence": 2})
            self.assertEqual(original, target.read_bytes())
            self.assertEqual([target], list(root.iterdir()))

    def test_failed_write_publishes_nothing_and_removes_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            root.chmod(0o700)
            target = root / "ready.json"
            with mock.patch.object(matrix_wire, "_open_private_parent", side_effect=lambda path, label: (
                     os.open(root, os.O_RDONLY | os.O_DIRECTORY), target.name)), \
                 mock.patch.object(matrix_wire.os, "write", side_effect=OSError("write failed")):
                with self.assertRaisesRegex(OSError, "write failed"):
                    matrix_wire.write_exclusive_json(target, {"sequence": 1})
            self.assertEqual([], list(root.iterdir()))


class HistoricalSourceTests(unittest.TestCase):
    def historical_files(self):
        files = {name: name.encode() for name in source_handoff.LEGACY_ROOT_FILES + source_handoff.PUBLIC_DOCUMENTS}
        files["Sources/Historical.swift"] = b"historical source"
        files["Tests/HistoricalTests.swift"] = b"historical tests"
        return files

    def run_fixture(self, files):
        def run(command, **kwargs):
            self.assertGreater(kwargs["timeout"], 0)
            self.assertEqual("git", command[0])
            if command[1] == "ls-tree":
                payload = b"".join(b"100644 blob ignored\t" + name.encode() + b"\0" for name in sorted(files))
                return subprocess.CompletedProcess(command, 0, payload, b"")
            name = command[2].split(":", 1)[1]
            return subprocess.CompletedProcess(command, 0 if name in files else 1, files.get(name, b""), b"")
        return run

    def test_current_additions_and_deletions_do_not_change_historical_handoff(self):
        files = self.historical_files()
        contract = platform_gate.load_contract()
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            current = base / "current"
            (current / "Sources").mkdir(parents=True)
            present = current / "Sources/CurrentOnly.swift"
            present.write_text("new source")
            with mock.patch.object(owned_command, "run", side_effect=self.run_fixture(files)):
                first = platform_gate._prepare_accepted_handoff(current, base / "first", contract)
                present.unlink()
                second = platform_gate._prepare_accepted_handoff(current, base / "second", contract)
            self.assertEqual(first, second)
            names = {item["path"] for item in first["source_package"]["files"]}
            self.assertIn("Sources/Historical.swift", names)
            self.assertNotIn("Sources/CurrentOnly.swift", names)
            self.assertNotIn("NOTICE", names)
            source_handoff.verify(base / "first", allow_legacy=True)
            with self.assertRaisesRegex(source_handoff.HandoffError, "NOTICE"):
                source_handoff.verify(base / "first")

    def test_missing_historical_member_fails_closed(self):
        files = self.historical_files()
        del files["Package.swift"]
        with mock.patch.object(owned_command, "run", side_effect=self.run_fixture(files)):
            with self.assertRaisesRegex(platform_gate.PlatformGateError, "missing required"):
                platform_gate._accepted_members(Path("/unused"), platform_gate.load_contract())

    def test_ios_host_binds_accepted_package_and_historical_tests(self):
        files = self.historical_files()
        files[platform_gate.IOS_PROJECT + "/project.pbxproj"] = b"relativePath = ..;"
        files["iOSRuntimeHost/Sources/Host.swift"] = b"historical host"
        with tempfile.TemporaryDirectory() as temporary:
            scratch = Path(temporary)
            package = scratch / "handoff" / source_handoff.CANONICAL_ROOT
            (package / "Tests").mkdir(parents=True)
            (package / "Tests/HistoricalTests.swift").write_bytes(b"historical tests")
            with mock.patch.object(owned_command, "run", side_effect=self.run_fixture(files)):
                platform_gate._prepare_ios_host(Path("/unused-current"), scratch, platform_gate.load_contract())
            project = scratch / "ios-root" / platform_gate.IOS_PROJECT
            self.assertIn(json.dumps(str(package.resolve())), (project / "project.pbxproj").read_text())
            self.assertEqual(b"historical tests", (scratch / "ios-root/Tests/HistoricalTests.swift").read_bytes())
            command = platform_gate.command_for("ios17", scratch=scratch)
            self.assertEqual(str(project), command[command.index("-project") + 1])
            self.assertNotIn("/unused-current", " ".join(command))

    def test_current_handoff_requires_and_copies_notice_unchanged(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            current = base / "current"
            for name in source_handoff.ROOT_FILES + source_handoff.PUBLIC_DOCUMENTS:
                path = current / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"notice original bytes" if name == "NOTICE" else name.encode())
            for name in source_handoff.SOURCE_DIRECTORIES:
                (current / name).mkdir(parents=True, exist_ok=True)
            source_handoff.prepare(current, base / "handoff")
            self.assertEqual((current / "NOTICE").read_bytes(),
                             (base / "handoff" / source_handoff.CANONICAL_ROOT / "NOTICE").read_bytes())
            (current / "NOTICE").unlink()
            with self.assertRaisesRegex(source_handoff.HandoffError, "NOTICE"):
                source_handoff.prepare(current, base / "missing")


class CommandBudgetTests(unittest.TestCase):
    def test_success_returns_complete_bounded_output_without_signaling(self):
        process = mock.Mock(pid=430)
        process.stdout.fileno.return_value = 10
        process.stderr.fileno.return_value = 11
        process.wait.return_value = 0
        selector = mock.Mock()
        pending = {}
        selector.register.side_effect = lambda pipe, event, index: pending.update({pipe: index})
        selector.get_map.side_effect = lambda: pending
        selector.unregister.side_effect = lambda pipe: pending.pop(pipe)
        selector.select.side_effect = [
            [(SimpleNamespace(fileobj=process.stdout, data=0), 1)],
            [(SimpleNamespace(fileobj=process.stderr, data=1), 1)],
            [(SimpleNamespace(fileobj=process.stdout, data=0), 1)],
            [(SimpleNamespace(fileobj=process.stderr, data=1), 1)],
        ]
        with mock.patch.object(owned_command.subprocess, "Popen", return_value=process), \
             mock.patch.object(owned_command.selectors, "DefaultSelector", return_value=selector), \
             mock.patch.object(owned_command.os, "set_blocking"), \
             mock.patch.object(owned_command.os, "read", side_effect=[b"out", b"err", b"", b""]), \
             mock.patch.object(owned_command.signal, "getsignal", return_value=signal.SIG_DFL), \
             mock.patch.object(owned_command.time, "monotonic", return_value=0), \
             mock.patch.object(owned_command, "_signal_group") as signal_group:
            result = owned_command.run(["mock-command"], timeout=60, text=True)
        self.assertEqual((0, "out", "err"), (result.returncode, result.stdout, result.stderr))
        process.wait.assert_called_once_with(timeout=60)
        process.poll.assert_not_called()
        signal_group.assert_not_called()

    def test_timeout_escalates_before_reaping_and_preserves_failure(self):
        events = []
        process = mock.Mock()
        process.pid = 431
        process.wait.side_effect = lambda **kwargs: events.append(("wait", kwargs["timeout"])) or 0
        selector = mock.Mock()
        selector.get_map.return_value = {1: "pending pipe"}
        with mock.patch.object(owned_command.subprocess, "Popen", return_value=process), \
             mock.patch.object(owned_command.selectors, "DefaultSelector", return_value=selector), \
             mock.patch.object(owned_command.os, "set_blocking"), \
             mock.patch.object(owned_command.signal, "getsignal", return_value=signal.SIG_DFL), \
             mock.patch.object(owned_command.time, "monotonic", side_effect=[0, 61]), \
             mock.patch.object(owned_command.time, "sleep") as sleep, \
             mock.patch.object(owned_command, "_signal_group", side_effect=lambda p, s: events.append(("signal", s))):
            with self.assertRaisesRegex(owned_command.CommandError, "exceeded"):
                owned_command.run(["mock-command"], timeout=60)
        self.assertEqual([("signal", signal.SIGTERM), ("signal", signal.SIGKILL),
                          ("wait", owned_command.SHUTDOWN_SECONDS)], events)
        process.poll.assert_not_called()
        sleep.assert_called_once_with(owned_command.SHUTDOWN_SECONDS)
        process.stdout.close.assert_called_once()
        process.stderr.close.assert_called_once()

    def test_output_limit_prevents_success(self):
        process = mock.Mock(pid=432)
        selector = mock.Mock()
        selector.get_map.return_value = {1: "pending pipe"}
        selector.select.return_value = [(SimpleNamespace(fileobj=process.stdout, data=0), 1)]
        with mock.patch.object(owned_command.subprocess, "Popen", return_value=process), \
             mock.patch.object(owned_command.selectors, "DefaultSelector", return_value=selector), \
             mock.patch.object(owned_command.os, "set_blocking"), \
             mock.patch.object(owned_command.os, "read", return_value=b"12345"), \
             mock.patch.object(owned_command.signal, "getsignal", return_value=signal.SIG_DFL), \
             mock.patch.object(owned_command.time, "monotonic", return_value=0), \
             mock.patch.object(owned_command.time, "sleep"), \
             mock.patch.object(owned_command, "_signal_group") as signal_group:
            with self.assertRaisesRegex(owned_command.CommandError, "output exceeds"):
                owned_command.run(["mock-command"], maximum_output=4)
        self.assertEqual(2, signal_group.call_count)

    def test_nondefault_sigchld_rejected_before_spawn(self):
        with mock.patch.object(owned_command.signal, "getsignal", return_value=signal.SIG_IGN), \
             mock.patch.object(owned_command.subprocess, "Popen") as spawn:
            with self.assertRaisesRegex(owned_command.CommandError, "SIGCHLD"):
                owned_command.run(["mock-command"])
            spawn.assert_not_called()

    def test_handoff_timeout_and_cleanup_failure_preserve_original(self):
        with mock.patch.object(source_handoff, "verify", return_value={}), \
             mock.patch.object(owned_command, "run", side_effect=owned_command.CommandError("original timeout")), \
             mock.patch.object(source_handoff, "_remove_swiftpm_generated_paths", side_effect=OSError("cleanup failed")):
            with self.assertRaisesRegex(owned_command.CommandError, "original timeout") as caught:
                source_handoff.smoke(Path("/unused-handoff"))
        self.assertTrue(any("cleanup failed" in note for note in caught.exception.__notes__))

    def test_linux_cleanup_has_independent_finite_budgets_and_checks_owned_resources(self):
        def run(command, **kwargs):
            self.assertEqual(owned_command.CLEANUP_SECONDS, kwargs["timeout"])
            return subprocess.CompletedProcess(command, 0, "" if kwargs.get("text") else b"", "")
        with mock.patch.object(owned_command, "run", side_effect=run) as runner:
            platform_gate._cleanup_linux("owned-container", "owned-image", Path("/unused"))
        self.assertEqual(4, runner.call_count)
        calls = [call.args[0] for call in runner.call_args_list]
        self.assertEqual("owned-container", calls[0][-1])
        self.assertIn("name=^/owned-container$", calls[1])
        self.assertEqual("owned-image", calls[2][-1])
        self.assertIn("reference=owned-image", calls[3])

    def test_docker_daemon_failure_cannot_prove_cleanup(self):
        with mock.patch.object(owned_command, "run", return_value=subprocess.CompletedProcess([], 1, "", "daemon unavailable")):
            with self.assertRaisesRegex(platform_gate.PlatformGateError, "absence could not be established"):
                platform_gate._cleanup_linux("owned-container", "owned-image", Path("/unused"))

    def test_linux_gate_binds_unique_build_runtime_and_cleanup_resources(self):
        def command(command, **kwargs):
            self.assertGreater(kwargs["timeout"], 0)
            if command[1] == "info":
                return subprocess.CompletedProcess(command, 0, "linux/arm64", "")
            if "--format" in command:
                return subprocess.CompletedProcess(command, 0, "sha256:" + "a" * 64 + "|linux/arm64|swiftuser", "")
            return subprocess.CompletedProcess(command, 1, b"", b"")

        with mock.patch.object(platform_gate.platform, "system", return_value="Linux"), \
             mock.patch.object(platform_gate.platform, "machine", return_value="arm64"), \
             mock.patch.object(platform_gate.shutil, "which", return_value="mock-docker"), \
             mock.patch.object(platform_gate, "verify_repository"), \
             mock.patch.object(platform_gate, "_prepare_accepted_handoff"), \
             mock.patch.object(owned_command, "run", side_effect=command), \
             mock.patch.object(platform_gate, "_run") as build, \
             mock.patch.object(platform_gate, "_run_capture", return_value=platform_gate.LINUX_CONSUMER_OUTPUT) as runtime, \
             mock.patch.object(platform_gate, "_cleanup_linux") as cleanup:
            first = platform_gate.run_gate("linux-arm64")
            second = platform_gate.run_gate("linux-arm64")
        self.assertNotEqual(first["owned_image_tag"], second["owned_image_tag"])
        for index, result in enumerate((first, second)):
            build_command = build.call_args_list[index].args[0]
            self.assertEqual(result["owned_image_tag"], build_command[build_command.index("--tag") + 1])
            runtime_command = runtime.call_args_list[index].args[0]
            self.assertEqual(result["owned_image_tag"], runtime_command[-1])
            self.assertEqual(result["owned_container_name"], runtime_command[runtime_command.index("--name") + 1])
            self.assertEqual((result["owned_container_name"], result["owned_image_tag"]), cleanup.call_args_list[index].args[:2])

    def test_unavailable_gate_rejected_before_any_verification_execution(self):
        with mock.patch.object(platform_gate.platform, "system", return_value="Linux"), \
             mock.patch.object(platform_gate.platform, "machine", return_value="x86_64"), \
             mock.patch.object(platform_gate, "verify_repository") as verify, \
             mock.patch.object(owned_command, "run") as run:
            with self.assertRaises(platform_gate.PlatformGateError):
                platform_gate.run_gate("linux-arm64")
            verify.assert_not_called()
            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
