"""Fixture runner checks; the native-process smoke is a separate gate."""
import importlib.util
import hashlib
import subprocess
import json
import os
from pathlib import Path
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
        profile = MODULE.resolve().parents[3] / "teslatlas-protocol/profiles/hub-http-v1/1.0.0"
        self.config = {
            "profile_id":"hub-http-v1@1.0.0",
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

    def test_edge_collector_overlay_emits_exclusive_fleet_pull_config(self):
        private = {}
        for name in ("ca.pem", "client.pem", "client-key.pem", "bearer"):
            path = self.root / name
            path.write_text(name)
            path.chmod(0o600)
            private[name] = str(path)
        self.config["edge_collector"] = {
            "base_url": "https://127.0.0.1:18510/",
            "ca_certificate_path": private["ca.pem"],
            "client_certificate_path": private["client.pem"],
            "client_private_key_path": private["client-key.pem"],
            "bearer_token_path": private["bearer"],
            "installation_id": "edge-b2-20260909",
            "lineage": "edge-v2-primary",
            "source_id": "11111111-1111-4111-8111-111111111111",
            "vehicle_id": "22222222-2222-4222-8222-222222222222",
            "vin": "5YJ3E1EA7KF000001",
            "car_id": 9,
        }
        loaded = self.load()
        path = self.root / "config.toml"
        path.write_text(
            '[collector]\ninterval_seconds = 0\nowner_api_base_url = "https://127.0.0.1:1/"\n'
            '[collector.legacy_auth]\nenabled = false\n[terrain]\nenabled = false\n'
        )

        fixture.configure_edge_collector(path, loaded["edge_collector"])

        rendered = path.read_text()
        self.assertIn('[collector]\nprovider = "fleet"\ninterval_seconds = 0', rendered)
        self.assertIn('[collector.edge]\nbase_url = "https://127.0.0.1:18510/"', rendered)
        self.assertIn('bearer_token_path = ' + json.dumps(private["bearer"]), rendered)
        self.assertNotIn("[collector.fleet_telemetry]", rendered)

    def test_edge_collector_accepts_fresh_docker_delivery_endpoints_and_rejects_receiver_ports(self):
        private = {}
        for name in ("ca.pem", "client.pem", "client-key.pem", "bearer"):
            path = self.root / name
            path.write_text(name)
            path.chmod(0o600)
            private[name] = str(path)
        self.config["edge_collector"] = {
            "base_url": "https://127.0.0.1:20443/",
            "ca_certificate_path": private["ca.pem"],
            "client_certificate_path": private["client.pem"],
            "client_private_key_path": private["client-key.pem"],
            "bearer_token_path": private["bearer"],
            "installation_id": "edge-b2-20260909",
            "lineage": "edge-spool-b2-20260909",
            "source_id": "04d1bc2f-0e9a-4f84-9ac5-492955dd8d5e",
            "vehicle_id": "11111111-1111-4111-8111-111111111111",
            "vin": "5YJ3E1EA7KF000001",
            "car_id": 9,
        }

        for delivery_endpoint, receiver_endpoint in (
            ("https://127.0.0.1:20443/", "https://127.0.0.1:20444/"),
            ("https://127.0.0.1:20543/", "https://127.0.0.1:20544/"),
            ("https://127.0.0.1:20743/", "https://127.0.0.1:20744/"),
            ("https://127.0.0.1:20843/", "https://127.0.0.1:20844/"),
            ("https://127.0.0.1:20943/", "https://127.0.0.1:20944/"),
        ):
            self.config["edge_collector"]["base_url"] = delivery_endpoint
            self.assertEqual(self.load()["edge_collector"]["base_url"], delivery_endpoint)

            self.config["edge_collector"]["base_url"] = receiver_endpoint
            with self.assertRaises(ValueError):
                self.load()

    def test_seeded_config_applies_edge_collector_before_http_origins(self):
        private = {}
        for name in ("ca.pem", "client.pem", "client-key.pem", "bearer"):
            path = self.root / name
            path.write_text(name)
            path.chmod(0o600)
            private[name] = str(path)
        self.config["edge_collector"] = {
            "base_url": "https://127.0.0.1:18510/",
            "ca_certificate_path": private["ca.pem"],
            "client_certificate_path": private["client.pem"],
            "client_private_key_path": private["client-key.pem"],
            "bearer_token_path": private["bearer"],
            "installation_id": "edge-b2-20260909",
            "lineage": "edge-v2-primary",
            "source_id": "11111111-1111-4111-8111-111111111111",
            "vehicle_id": "22222222-2222-4222-8222-222222222222",
            "vin": "5YJ3E1EA7KF000001",
            "car_id": 9,
        }
        self.config["allowed_origins"] = ["http://127.0.0.1:4173"]
        loaded = self.load()
        path = self.root / "config.toml"
        path.write_text(
            '[collector]\ninterval_seconds = 0\nowner_api_base_url = "https://127.0.0.1:1/"\n'
            '[collector.legacy_auth]\nenabled = false\n[terrain]\nenabled = false\n'
        )

        fixture.configure_seeded_config(path, loaded)

        rendered = path.read_text()
        self.assertLess(rendered.index("[collector.edge]"), rendered.index("[http]"))
        self.assertIn('allowed_origins = ["http://127.0.0.1:4173"]', rendered)

    def test_edge_collector_rejects_a_group_readable_credential_path(self):
        private = {}
        for name in ("ca.pem", "client.pem", "client-key.pem", "bearer"):
            path = self.root / name
            path.write_text(name)
            path.chmod(0o600)
            private[name] = str(path)
        Path(private["bearer"]).chmod(0o640)
        self.config["edge_collector"] = {
            "base_url": "https://127.0.0.1:18510/",
            "ca_certificate_path": private["ca.pem"],
            "client_certificate_path": private["client.pem"],
            "client_private_key_path": private["client-key.pem"],
            "bearer_token_path": private["bearer"],
            "installation_id": "edge-b2-20260909",
            "lineage": "edge-v2-primary",
            "source_id": "11111111-1111-4111-8111-111111111111",
            "vehicle_id": "22222222-2222-4222-8222-222222222222",
            "vin": "5YJ3E1EA7KF000001",
            "car_id": 9,
        }

        with self.assertRaises(ValueError):
            self.load()

    def test_edge_collector_seed_command_binds_the_sealed_source_identity(self):
        private = {}
        for name in ("ca.pem", "client.pem", "client-key.pem", "bearer"):
            path = self.root / name
            path.write_text(name)
            path.chmod(0o600)
            private[name] = str(path)
        self.config["edge_collector"] = {
            "base_url": "https://127.0.0.1:18510/",
            "ca_certificate_path": private["ca.pem"],
            "client_certificate_path": private["client.pem"],
            "client_private_key_path": private["client-key.pem"],
            "bearer_token_path": private["bearer"],
            "installation_id": "edge-b2-20260909",
            "lineage": "edge-spool-b2-20260909",
            "source_id": "04d1bc2f-0e9a-4f84-9ac5-492955dd8d5e",
            "vehicle_id": "11111111-1111-4111-8111-111111111111",
            "vin": "5YJ3E1EA7KF000001",
            "car_id": 9,
        }
        loaded = self.load()

        command = fixture.seed_command(loaded, self.executable, self.root / "new", 18480)

        self.assertEqual(
            command,
            [str(self.executable), "--output", str(self.root / "new"), "--port", "18480",
             "--source-id", "04d1bc2f-0e9a-4f84-9ac5-492955dd8d5e",
             "--vehicle-id", "11111111-1111-4111-8111-111111111111",
             "--vin", "5YJ3E1EA7KF000001", "--car-id", "9"],
        )

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


if __name__ == "__main__":
    unittest.main()
