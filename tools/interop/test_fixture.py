"""Fixture runner checks; the native-process smoke is a separate gate."""
import contextlib
import importlib.util
import hashlib
import io
import subprocess
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

MODULE = Path(__file__).with_name("fixture.py")
spec = importlib.util.spec_from_file_location("interop_fixture", MODULE)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class ConfigTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.executable = self.root / "binary"
        self.executable.write_text("fixture")
        self.executable.chmod(0o700)
        profile = MODULE.resolve().parents[2] / "protocol/profiles/hub-http-v1/1.2.0"
        self.config = {
            "profile_id":fixture.HTTP_PROFILE_ID,
            "profile_path":str(profile),
            "profile_sha256":hashlib.sha256((profile / "SHA256SUMS").read_bytes()).hexdigest(),
            "binary": str(self.executable),
            "seed_binary": str(self.executable),
            "output_dir": str(self.root / "new"),
            "lifetime_seconds": 30,
        }

    def load(self, mode=0o600):
        path = self.root / "run.json"
        path.write_text(json.dumps(self.config))
        path.chmod(mode)
        return fixture.load_config(path)

    def test_valid_config_reserves_new_private_fixture(self):
        result = self.load()
        self.assertEqual(result["output_dir"], self.config["output_dir"])
        self.assertFalse(Path(result["output_dir"]).exists())

    def test_default_scenario_accepts_hub_http_1_2_0(self):
        result = self.load()
        self.assertEqual(result["profile_id"], "hub-http-v1@1.2.0")
        self.assertEqual(fixture.HTTP_PROFILE_ID, "hub-http-v1@1.2.0")

    def test_default_scenario_refuses_hub_http_1_0_0(self):
        self.config["profile_id"] = "hub-http-v1@1.0.0"
        with self.assertRaisesRegex(ValueError, "scenario profile binding"):
            self.load()

    def test_staged_executable_survives_source_replacement(self):
        original = b"#!/bin/sh\nprintf staged-original"
        self.executable.write_bytes(original)
        self.assertTrue(hasattr(fixture, "stage_executable"), "fixture must stage executed bytes before launch")
        staged, digest = fixture.stage_executable(self.executable, self.root / "staged")
        process = subprocess.Popen([str(staged)], stdout=subprocess.PIPE)
        replacement = self.root / "replacement"
        replacement.write_text("#!/bin/sh\nprintf replaced")
        replacement.chmod(0o700)
        replacement.replace(self.executable)
        output, _ = process.communicate(timeout=5)
        self.assertEqual(output, b"staged-original")
        self.assertEqual(digest, hashlib.sha256(original).hexdigest())
        self.assertEqual(hashlib.sha256(staged.read_bytes()).hexdigest(), digest)
        self.assertEqual(staged.stat().st_mode & 0o777, 0o500)

    def test_subprocess_diagnostic_reports_exit_without_child_stderr(self):
        result = fixture.subprocess_diagnostic(
            "seed", ["/bin/sh", "-c", "printf never-persist-this >&2; exit 7"], 5
        )

        self.assertEqual(result["stage"], "seed")
        self.assertEqual(result["termination"], "exit")
        self.assertEqual(result["exit_code"], 7)
        self.assertIsInstance(result["elapsed_ms"], int)
        self.assertGreaterEqual(result["elapsed_ms"], 0)
        self.assertNotIn("stderr", result)
        self.assertNotIn("never-persist-this", json.dumps(result))

    def test_subprocess_diagnostic_reports_signal_without_child_stderr(self):
        result = fixture.subprocess_diagnostic(
            "seed", ["/bin/sh", "-c", "kill -TERM $$"], 5
        )

        self.assertEqual(result["stage"], "seed")
        self.assertEqual(result["termination"], "signal")
        self.assertEqual(result["signal"], "SIGTERM")
        self.assertNotIn("stderr", result)

    def test_subprocess_diagnostic_reports_timeout_without_child_stderr(self):
        result = fixture.subprocess_diagnostic(
            "seed", ["/bin/sh", "-c", "sleep 1"], 0.01
        )

        self.assertEqual(result["stage"], "seed")
        self.assertEqual(result["termination"], "timeout")
        self.assertNotIn("exit_code", result)
        self.assertNotIn("stderr", result)

    def test_subprocess_diagnostic_runs_child_with_private_umask(self):
        previous = os.umask(0o002)
        try:
            result = fixture.subprocess_diagnostic(
                "seed", ["/bin/sh", "-c", 'test "$(umask)" = 0077'], 5
            )
        finally:
            os.umask(previous)

        self.assertEqual(result["termination"], "exit")
        self.assertEqual(result["exit_code"], 0)

    def test_development_opt_in_is_confined_to_owned_serve_child(self):
        parent_value = "parent-must-remain-unchanged"
        sentinel = "preserved-parent-value"
        with mock.patch.dict(os.environ, {
            fixture.DEVELOPMENT_SERVE_ENV: parent_value,
            "TESLATLAS_FIXTURE_PARENT_SENTINEL": sentinel,
        }, clear=False):
            child = object()
            with mock.patch.object(fixture.subprocess, "Popen", return_value=child) as popen:
                observed = fixture.start_owned_synthetic_serve(
                    self.executable, self.root / "config.toml", object()
                )

            self.assertIs(observed, child)
            command = popen.call_args.args[0]
            options = popen.call_args.kwargs
            self.assertEqual(command[-1], "serve")
            self.assertEqual(options["env"][fixture.DEVELOPMENT_SERVE_ENV], "1")
            self.assertEqual(
                options["env"][fixture.DEVELOPMENT_SERVE_MODE_ENV],
                "fixture",
            )
            self.assertEqual(options["env"]["TESLATLAS_FIXTURE_PARENT_SENTINEL"], sentinel)
            self.assertIsNot(options["env"], os.environ)
            self.assertEqual(os.environ[fixture.DEVELOPMENT_SERVE_ENV], parent_value)

    def test_seed_and_pair_subprocesses_do_not_receive_development_opt_in(self):
        completed = mock.Mock(returncode=0)
        with mock.patch.object(fixture.subprocess, "run", return_value=completed) as run:
            diagnostic = fixture.subprocess_diagnostic("seed", [str(self.executable)], 5)
            self.assertEqual(diagnostic["exit_code"], 0)
            self.assertNotIn("env", run.call_args.kwargs)

        with mock.patch.object(fixture.subprocess, "run", return_value=completed) as run:
            fixture.run_pair_command(
                self.executable, self.root / "config.toml", object()
            )
            self.assertEqual(run.call_args.args[0][3], "pair")
            self.assertNotIn("env", run.call_args.kwargs)

    def test_seed_failure_leaves_private_sanitized_diagnostic(self):
        self.executable.write_text("#!/bin/sh\nprintf never-persist-this >&2\nexit 7\n")
        self.executable.chmod(0o700)
        config = self.load()

        with self.assertRaisesRegex(RuntimeError, "synthetic seed failed"):
            fixture.run(config)

        diagnostic = self.root / "fixture-diagnostic.json"
        saved = json.loads(diagnostic.read_text())
        self.assertEqual(diagnostic.stat().st_mode & 0o777, 0o600)
        self.assertEqual(saved["stage"], "seed")
        self.assertEqual(saved["termination"], "exit")
        self.assertEqual(saved["exit_code"], 7)
        self.assertIn("elapsed_ms", saved)
        self.assertNotIn("never-persist-this", json.dumps(saved))
        self.assertNotIn("stderr", saved)

    def test_missing_profile_binding_fails_before_state_creation(self):
        del self.config["profile_id"]
        with self.assertRaises(ValueError):
            self.load()
        self.assertFalse(Path(self.config["output_dir"]).exists())

    def test_readiness_witness_identifies_owned_live_child_and_rejects_exit(self):
        child=subprocess.Popen(['/bin/sleep','30'])
        try:
            self.assertTrue(hasattr(fixture,'ready_process_fields'))
            witness=fixture.ready_process_fields(child,self.root)
            self.assertEqual(witness['hub_pid'],child.pid)
            self.assertEqual(witness['launcher_pid'],os.getpid())
            self.assertEqual(witness['ready_path'],str(self.root/'ready.json'))
            self.assertTrue(witness['hub_started_at'])
            child.terminate();child.wait(timeout=5)
            with self.assertRaises((ValueError,RuntimeError)):
                fixture.ready_process_fields(child,self.root)
        finally:
            if child.poll() is None:child.terminate()
            child.wait(timeout=5)

    def test_unknown_config_fields_rejected(self):
        self.config["ignore_tls_errors"] = True
        with self.assertRaises(ValueError):
            self.load()

    def test_allowed_origins_are_exact_canonical_http_origins(self):
        self.config["allowed_origins"] = ["http://localhost:43123", "https://example.test"]
        self.assertEqual(self.load()["allowed_origins"], self.config["allowed_origins"])
        for origin in ("http://localhost:43123/", "HTTP://localhost:43123", "https://user@example.test", "file:///tmp/x"):
            self.config["allowed_origins"] = [origin]
            with self.assertRaises(ValueError):
                self.load()

    def test_viewer_r1_selector_emits_fixed_seed_argument_and_static_scenario(self):
        self.config["scenario_id"] = "viewer-r1-51-drives"

        loaded = self.load()

        self.assertEqual(
            fixture.seed_command(loaded, self.executable, self.root / "new", 18480)[-2:],
            ["--scenario", "viewer-r1-51-drives"],
        )
        scenario = fixture.scenario_source(loaded)
        self.assertEqual(scenario.name, "scenario-viewer-r1-51-drives.json")
        expected = json.loads(scenario.read_text())
        self.assertEqual(expected["drive_pages_at_limit_25"], [
            list(range(1051, 1026, -1)),
            list(range(1026, 1001, -1)),
            [1001],
        ])

    def test_physical_v3_selector_requires_exact_profile_port_and_seed_receipt(self):
        profile = MODULE.resolve().parents[2] / "protocol/profiles/hub-sync-v1/1.3.0"
        self.config.update({
            "scenario_id": fixture.PHYSICAL_V3_SCENARIO_ID,
            "profile_id": fixture.PHYSICAL_V3_PROFILE_ID,
            "profile_path": str(profile),
            "profile_sha256": hashlib.sha256((profile / "SHA256SUMS").read_bytes()).hexdigest(),
            "port": fixture.PHYSICAL_V3_PORT,
        })

        loaded = self.load()
        self.assertEqual(
            fixture.seed_command(
                loaded,
                self.executable,
                self.root / "new",
                fixture.PHYSICAL_V3_PORT,
            )[-2:],
            ["--scenario", fixture.PHYSICAL_V3_SCENARIO_ID],
        )
        descriptor = {
            "endpoint": "https://127.0.0.1:21445",
            "vehicle_ids": ["11111111-1111-4111-8111-111111111111"],
            "physical_v3_admission": {
                "profile_id": fixture.PHYSICAL_V3_PROFILE_ID,
                "vehicle_id": "11111111-1111-4111-8111-111111111111",
                "snapshot_id": fixture.PHYSICAL_V3_SNAPSHOT_ID,
                "head_sequence": 3,
                "receipt_id": "pv3_" + "a" * 64,
                "chunk_count": 513,
                "logical_source_rows": 1027,
                "drive_ids": [301],
                "position_ids": [401, 402],
                "charging_process_ids": [501],
                "charge_sample_ids": [601, 602],
                "address_rows": 0,
                "geofence_rows": 0,
                "collector_enabled": False,
            },
        }
        fixture.validate_seeded_descriptor(loaded, descriptor, 21445)
        descriptor["physical_v3_admission"]["chunk_count"] = 512
        with self.assertRaisesRegex(ValueError, "admission receipt"):
            fixture.validate_seeded_descriptor(loaded, descriptor, 21445)

        self.config["port"] = 21443
        with self.assertRaisesRegex(ValueError, "21445"):
            self.load()
        self.config["port"] = 21445
        self.config["profile_id"] = "hub-http-v1@1.0.0"
        with self.assertRaisesRegex(ValueError, "profile"):
            self.load()

    def test_physical_rotation_trigger_is_empty_private_and_consumed_once(self):
        request = self.root / fixture.PHYSICAL_ROTATION_REQUEST
        request.write_bytes(b"")
        request.chmod(0o600)

        self.assertTrue(fixture.consume_owned_empty_request(
            self.root,
            fixture.PHYSICAL_ROTATION_REQUEST,
            fixture.PHYSICAL_ROTATION_CONSUMED,
        ))
        self.assertFalse(request.exists())
        consumed = self.root / fixture.PHYSICAL_ROTATION_CONSUMED
        self.assertTrue(consumed.is_file())
        self.assertEqual(consumed.stat().st_mode & 0o777, 0o600)
        with self.assertRaisesRegex(ValueError, "already consumed"):
            fixture.consume_owned_empty_request(
                self.root,
                fixture.PHYSICAL_ROTATION_REQUEST,
                fixture.PHYSICAL_ROTATION_CONSUMED,
            )

        other_root = self.root / "payload"
        other_root.mkdir(mode=0o700)
        payload = other_root / fixture.PHYSICAL_ROTATION_REQUEST
        payload.write_text("no paths or commands")
        payload.chmod(0o600)
        with self.assertRaisesRegex(ValueError, "empty owner-only"):
            fixture.consume_owned_empty_request(
                other_root,
                fixture.PHYSICAL_ROTATION_REQUEST,
                fixture.PHYSICAL_ROTATION_CONSUMED,
            )

    def test_physical_rotation_runs_fixed_owned_command_and_validates_receipt(self):
        prior = {
            "profile_id": fixture.PHYSICAL_V3_PROFILE_ID,
            "vehicle_id": "11111111-1111-4111-8111-111111111111",
            "snapshot_id": fixture.PHYSICAL_V3_SNAPSHOT_ID,
            "head_sequence": 3,
            "receipt_id": "pv3_" + "a" * 64,
        }
        descriptor = {"physical_v3_admission": prior}
        receipt = {
            "status": "rotated-and-activated",
            "profile_id": fixture.PHYSICAL_V3_PROFILE_ID,
            "vehicle_id": prior["vehicle_id"],
            "retained_snapshot_id": prior["snapshot_id"],
            "retained_head_sequence": prior["head_sequence"],
            "retained_receipt_id": prior["receipt_id"],
            "retained_at_ms": 1788566400000,
            "retained_expires_at_ms": 1788652800000,
            "active_snapshot_id": fixture.PHYSICAL_V3_SUCCESSOR_SNAPSHOT_ID,
            "active_head_sequence": 4,
            "active_receipt_id": "pv3_" + "b" * 64,
            "active_chunk_count": 513,
            "active_first_chunk_sha256": "c" * 64,
            "active_last_chunk_sha256": "d" * 64,
            "collector_enabled": False,
        }
        receipt_path = self.root / fixture.PHYSICAL_ROTATION_RECEIPT
        fixture.write_private_json(receipt_path, receipt)
        completed = mock.Mock(returncode=0)

        with mock.patch.object(fixture.subprocess, "run", return_value=completed) as run:
            actual = fixture.run_owned_physical_rotation(
                self.executable, self.root, descriptor
            )

        self.assertEqual(actual, receipt)
        self.assertEqual(
            run.call_args.args[0],
            [str(self.executable), "--rotate-physical", str(self.root)],
        )
        self.assertIs(run.call_args.kwargs["stdin"], fixture.subprocess.DEVNULL)
        self.assertIs(run.call_args.kwargs["stdout"], fixture.subprocess.DEVNULL)
        self.assertEqual(receipt_path.stat().st_mode & 0o777, 0o600)
        receipt["active_chunk_count"] = 512
        with self.assertRaisesRegex(ValueError, "rotation receipt"):
            fixture.validate_physical_rotation_receipt(descriptor, receipt)

    def test_unknown_scenario_selector_fails_before_state_creation(self):
        self.config["scenario_id"] = "../../untrusted"

        with self.assertRaisesRegex(ValueError, "scenario"):
            self.load()

        self.assertFalse(Path(self.config["output_dir"]).exists())

    def test_viewer_r1_scenario_copy_is_private_and_exact(self):
        self.config["scenario_id"] = "viewer-r1-51-drives"
        loaded = self.load()

        copied, digest = fixture.copy_selected_scenario(self.root, loaded)

        expected = fixture.scenario_source(loaded).read_bytes()
        self.assertEqual(copied, self.root / "scenario.json")
        self.assertEqual(copied.read_bytes(), expected)
        self.assertEqual(digest, hashlib.sha256(expected).hexdigest())
        self.assertEqual(copied.stat().st_mode & 0o777, 0o600)

    def test_allowed_origins_are_appended_to_seeded_config(self):
        path = self.root / "config.toml"
        path.write_text('bind = "127.0.0.1:1234"\n')
        fixture.configure_allowed_origins(path, ["http://localhost:43123"])
        self.assertEqual(
            path.read_text(),
            'bind = "127.0.0.1:1234"\n[http]\nallowed_origins = ["http://localhost:43123"]\n',
        )

    def test_edge_collector_key_is_refused(self):
        # The retired Edge collector binding is an unknown config field.
        self.config["edge_collector"] = {"base_url": "https://127.0.0.1:18510/"}
        with self.assertRaisesRegex(ValueError, "unknown config fields"):
            self.load()
        # Any other unknown key is refused the same way.
        del self.config["edge_collector"]
        self.config["mystery_collector"] = {}
        with self.assertRaisesRegex(ValueError, "unknown config fields"):
            self.load()

    def test_shared_readable_config_rejected(self):
        with self.assertRaises(ValueError):
            self.load(0o644)

    def test_existing_directory_never_reused(self):
        Path(self.config["output_dir"]).mkdir()
        with self.assertRaises(ValueError):
            self.load()

    def test_relative_paths_rejected(self):
        self.config["output_dir"] = "relative"
        with self.assertRaises(ValueError):
            self.load()

    def test_invalid_ports_and_lifetimes_rejected(self):
        for value in (0, -1, 65536, True):
            self.config["port"] = value
            with self.assertRaises(ValueError):
                self.load()
        self.config.pop("port")
        self.config["lifetime_seconds"] = 0
        with self.assertRaises(ValueError):
            self.load()

    def test_symlink_config_rejected(self):
        path = self.root / "real.json"
        path.write_text(json.dumps(self.config))
        path.chmod(0o600)
        alias = self.root / "alias.json"
        alias.symlink_to(path)
        with self.assertRaises(ValueError):
            fixture.load_config(alias)


PASS = {
    "profile_id": "hub-http-v1@1.2.0",
    "complete": True,
    "runs": 61,
    "passed": 61,
    "failed": 0,
    "not_run": 0,
    "profile_sha256": hashlib.sha256(b"profile").hexdigest(),
}


class SmokeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.interop_dir = str(MODULE.parent)
        cls.had_fixture_module = "fixture" in sys.modules
        sys.path.insert(0, cls.interop_dir)
        smoke_spec = importlib.util.spec_from_file_location(
            "interop_smoke", MODULE.with_name("smoke.py")
        )
        cls.smoke = importlib.util.module_from_spec(smoke_spec)
        smoke_spec.loader.exec_module(cls.smoke)

    @classmethod
    def tearDownClass(cls):
        sys.path.remove(cls.interop_dir)
        if not cls.had_fixture_module:
            sys.modules.pop("fixture", None)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.descriptor = self.root / "descriptor.json"
        self.descriptor.write_text("{}")
        self.descriptor.chmod(0o600)
        self.output = self.root / "receipt.json"

    def run_smoke(self, summary=PASS, returncode=0):
        completed = subprocess.CompletedProcess(
            args=[],
            returncode=returncode,
            stdout=json.dumps(summary).encode(),
            stderr=b"runner-stderr-secret",
        )
        out, err = io.StringIO(), io.StringIO()
        with mock.patch.object(self.smoke.subprocess, "run", return_value=completed) as runner:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                status = self.smoke.main(
                    ["--descriptor", str(self.descriptor), "--output", str(self.output)]
                )
        return status, out.getvalue(), err.getvalue(), runner

    def assert_refused(self, summary=PASS, returncode=0):
        status, out, err, runner = self.run_smoke(summary, returncode)
        runner.assert_called_once()
        self.assertEqual(status, 1)
        self.assertEqual(out, "")
        self.assertEqual(err, self.smoke.REFUSAL + "\n")
        self.assertFalse(self.output.exists())

    def test_passes_a_complete_run(self):
        status, out, err, _ = self.run_smoke(PASS)
        self.assertEqual(status, 0)
        self.assertEqual(err, "")
        self.assertEqual(json.loads(self.output.read_text()), PASS)
        self.assertEqual(self.output.stat().st_mode & 0o777, 0o600)
        printed = json.loads(out)
        self.assertEqual(set(printed), {"status", "runs", "profile_sha256", "receipt"})
        self.assertEqual(printed["status"], "passed")
        self.assertEqual(printed["runs"], 61)
        self.assertEqual(printed["profile_sha256"], PASS["profile_sha256"])
        self.assertEqual(printed["receipt"], str(self.output))

    def test_runs_the_1_2_0_profile_without_an_adapter(self):
        _, _, _, runner = self.run_smoke(PASS)
        runner.assert_called_once()
        args, kwargs = runner.call_args
        runner_path = MODULE.resolve().parents[2] / "protocol/conformance/run"
        self.assertEqual(args[0], [
            str(runner_path),
            "--profile", "hub-http-v1@1.2.0",
            "--config", str(self.descriptor),
            "--timeout-seconds", "600",
            "--json",
        ])
        self.assertNotIn("--adapter", args[0])
        self.assertEqual(kwargs["timeout"], 660)
        self.assertIs(kwargs["stdin"], subprocess.DEVNULL)
        self.assertIs(kwargs["stdout"], subprocess.PIPE)
        self.assertIs(kwargs["stderr"], subprocess.PIPE)

    def test_refuses_nonzero_exit(self):
        self.assert_refused(PASS, returncode=3)

    def test_refuses_cases_not_run(self):
        self.assert_refused(dict(PASS, not_run=1))

    def test_refuses_the_offline_preflight_summary(self):
        self.assert_refused(dict(PASS, complete=False, passed=11, not_run=50))

    def test_refuses_incomplete_run(self):
        self.assert_refused(dict(PASS, complete=False))

    def test_refuses_another_profile(self):
        self.assert_refused(dict(PASS, profile_id="hub-http-v1@1.1.0"))

    def test_refuses_failures(self):
        self.assert_refused(dict(PASS, failed=1, detail="bearer-secret-value"))

    def test_refuses_empty_runs(self):
        self.assert_refused(dict(PASS, runs=0, passed=0))

    def test_refuses_a_pass_count_short_of_runs(self):
        self.assert_refused(dict(PASS, passed=60))

    def test_refuses_non_integer_runs(self):
        self.assert_refused(dict(PASS, runs="61", passed="61"))

    def test_refuses_a_shared_descriptor_before_running(self):
        self.descriptor.chmod(0o644)
        out, err = io.StringIO(), io.StringIO()
        with mock.patch.object(self.smoke.subprocess, "run") as runner:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                status = self.smoke.main(
                    ["--descriptor", str(self.descriptor), "--output", str(self.output)]
                )
        runner.assert_not_called()
        self.assertEqual(status, 1)
        self.assertEqual(out.getvalue(), "")
        self.assertEqual(err.getvalue(), self.smoke.REFUSAL + "\n")
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
