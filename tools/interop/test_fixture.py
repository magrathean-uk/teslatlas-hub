"""Fixture runner checks; the native-process smoke is a separate gate."""
import importlib.util
import hashlib
import subprocess
import json
import os
from pathlib import Path
import tempfile
import unittest

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

    def test_allowed_origins_are_appended_to_seeded_config(self):
        path = self.root / "config.toml"
        path.write_text('bind = "127.0.0.1:1234"\n')
        fixture.configure_allowed_origins(path, ["http://localhost:43123"])
        self.assertEqual(
            path.read_text(),
            'bind = "127.0.0.1:1234"\n[http]\nallowed_origins = ["http://localhost:43123"]\n',
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
