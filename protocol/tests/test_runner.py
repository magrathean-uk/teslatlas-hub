"""Tests of the conformance runner: the hub-sync-v1@1.3.0 fixture run and the hub-http-v1@1.2.0 branch."""
from __future__ import annotations

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from threading import Event
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from conformance import runner  # noqa: E402
from conformance.runner import ConformanceError, run_actual_hub  # noqa: E402

SYNC_PROFILE = "hub-sync-v1@1.3.0"
SUMMARY_KEYS = {"profiles", "runs", "passed", "failed", "failures"}


def run_runner(*arguments: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(ROOT / "conformance/runner.py"), *arguments],
        cwd=ROOT, capture_output=True, text=True, timeout=120,
    )


class RunnerTests(unittest.TestCase):
    def assert_full_sync_run(self, result: subprocess.CompletedProcess) -> None:
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        summary = json.loads(result.stdout)
        self.assertEqual(SUMMARY_KEYS, set(summary))
        self.assertEqual([SYNC_PROFILE], summary["profiles"])
        self.assertEqual(65, summary["runs"])
        self.assertEqual(65, summary["passed"])
        self.assertEqual(0, summary["failed"])
        self.assertEqual([], summary["failures"])

    def test_default_run_is_hub_sync_1_3_0_only(self) -> None:
        self.assert_full_sync_run(run_runner("--json"))

    def test_hub_sync_profile_runs_by_name(self) -> None:
        self.assert_full_sync_run(run_runner("--profile", SYNC_PROFILE, "--json"))

    def test_adapter_is_refused(self) -> None:
        refusal = {"error": "conformance runs use no adapter executable"}
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "adapter-ran"
            adapter = Path(directory) / "marker_adapter.py"
            adapter.write_text(
                f"#!{sys.executable}\nfrom pathlib import Path\nPath({str(marker)!r}).write_text('ran')\n"
            )
            adapter.chmod(0o700)
            for arguments in (
                ("--adapter", str(adapter), "--json"),
                ("--profile", SYNC_PROFILE, "--adapter", str(adapter), "--json"),
            ):
                with self.subTest(arguments=arguments):
                    result = run_runner(*arguments)
                    self.assertEqual(2, result.returncode, result.stdout + result.stderr)
                    self.assertEqual(refusal, json.loads(result.stdout))
            self.assertFalse(marker.exists(), "the refused adapter was executed")

    def test_unknown_profile_is_refused(self) -> None:
        for profile in ("hub-http-v1@1.0.0", "hub-http-v1@1.1.0"):
            with self.subTest(profile=profile):
                result = run_runner("--profile", profile, "--json")
                self.assertEqual(2, result.returncode, result.stdout + result.stderr)
                self.assertEqual({"error": f"unknown profile: {profile}"}, json.loads(result.stdout))

    @unittest.skipUnless(os.name == "posix", "owned process-group deadline requires POSIX")
    def test_actual_hub_branch_enforces_total_process_deadline(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stub = root / "conformance/hub_http_12.py"
            stub.parent.mkdir(parents=True)
            # A real child that waits for a signal, requiring no network/config
            # and no timed race to demonstrate the outer-process deadline.
            stub.write_text("import signal\nsignal.pause()\n")
            argv = ["runner", "--profile", "hub-http-v1@1.2.0", "--timeout-seconds", "0.1"]
            started = time.monotonic()
            with patch.object(runner, "ROOT", root), patch.object(sys, "argv", argv):
                with self.assertRaisesRegex(ConformanceError, "total process deadline"):
                    runner.run()
            self.assertLess(time.monotonic() - started, 2.0)
            stub.write_text("raise SystemExit(0)\n")
            with patch.object(runner, "ROOT", root), patch.object(sys, "argv", argv):
                self.assertEqual(0, runner.run())

    @unittest.skipUnless(os.name == "posix", "owned process-group cleanup requires POSIX")
    def test_actual_hub_cleans_ready_descendant_after_leader_exits(self) -> None:
        child_source = (
            "import signal,sys; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            "sys.stdout.write('ready'); sys.stdout.flush(); signal.pause()"
        )
        with tempfile.TemporaryDirectory() as directory:
            pid_file = Path(directory) / "owned-child.pid"
            for exit_status in (0, 3):
                with self.subTest(exit_status=exit_status):
                    leader_source = (
                        "import subprocess,sys; from pathlib import Path; "
                        f"child=subprocess.Popen([sys.executable,'-c',{child_source!r}],stdout=subprocess.PIPE); "
                        "assert child.stdout.read(5)==b'ready'; "
                        f"Path({str(pid_file)!r}).write_text(str(child.pid)); "
                        f"raise SystemExit({exit_status})"
                    )
                    try:
                        self.assertEqual(exit_status, run_actual_hub([sys.executable, "-c", leader_source], 2))
                        child_pid = int(pid_file.read_text())
                        deadline = time.monotonic() + 2
                        while True:
                            try:
                                os.kill(child_pid, 0)
                            except ProcessLookupError:
                                break
                            if time.monotonic() >= deadline:
                                self.fail("owned descendant remained after leader settlement")
                            Event().wait(0.01)
                    finally:
                        if pid_file.exists():
                            try:
                                os.kill(int(pid_file.read_text()), signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                            pid_file.unlink()


if __name__ == "__main__":
    unittest.main()
