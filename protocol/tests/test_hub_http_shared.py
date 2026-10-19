"""Tests of the shared transport and evidence helpers kept for hub-http-v1@1.2.0 and the native fixture."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from conformance import hub_http  # noqa: E402

RETAINED = (
    'MAX_BYTES', 'AcceptanceError', '_RoundedFraction', 'strict_json', 'read_private', 'canonical_origin',
    '_remaining', '_DeadlineSocketReader', '_DeadlineResponseSocket', '_DeadlineHTTPSConnection')
RETIRED = (
    'PROFILE_ID', 'DEFAULT_PROFILE', '_native_spec', 'native_evidence', 'verify_native_fixture', 'load_profile',
    'validate_raw', 'validate_resource_identity', 'DenyRedirects', '_PinnedHTTPSConnection', '_PinnedHTTPSHandler',
    'run_network', 'main')


def load_native_evidence():
    spec = importlib.util.spec_from_file_location('hub_native_evidence_test', ROOT / 'conformance/hub_native_evidence.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def document(lexeme):
    return ('{"n":' + lexeme + '}').encode()


class StrictJsonTests(unittest.TestCase):
    def test_integral_spellings_keep_exact_signed64_values(self):
        for integer in (-(2 ** 63), 2 ** 53 + 1, 2 ** 63 - 1):
            for lexeme in (str(integer), str(integer) + '.0', str(integer) + 'e0'):
                with self.subTest(lexeme=lexeme):
                    value = hub_http.strict_json(document(lexeme))['n']
                    self.assertEqual(value, integer)
                    self.assertIs(type(value), int)
        self.assertEqual(hub_http.strict_json(document('1e100'))['n'], 10 ** 100)

    def test_fractional_lexemes_stay_floats(self):
        for lexeme in ('9007199254740990.25', '1.0000000000000000001', '1e-400'):
            with self.subTest(lexeme=lexeme):
                value = hub_http.strict_json(document(lexeme))['n']
                self.assertIsInstance(value, float)
                self.assertEqual(value, float(lexeme))
                # Each of these rounds to an integral binary64 value; the raw value was not integral.
                self.assertFalse(value.is_integer())
                self.assertEqual(json.loads(json.dumps(value)), float(lexeme))

    def test_rejects_duplicate_members(self):
        for raw in (b'{"a":1,"a":2}', b'{"a":{"b":1,"b":1}}'):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                hub_http.strict_json(raw)

    def test_rejects_non_finite_constants(self):
        for raw in (b'{"a":NaN}', b'{"a":Infinity}', b'{"a":-Infinity}'):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                hub_http.strict_json(raw)

    def test_rejects_overflow_and_trailing_bytes(self):
        for raw in (b'{"a":1e999}', b'{"a":-1e999}', b'{"a":[]} x'):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                hub_http.strict_json(raw)


class NativeEvidenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.native = load_native_evidence()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'fixture'
        self.root.mkdir(mode=0o700)
        stage = Path(self.temp.name) / 'staged'
        stage.mkdir(mode=0o700)
        self.binary = stage / 'hub'
        self.seed = stage / 'seed'
        source = stage / 'test.c'
        source.write_text('#include <unistd.h>\nint main(void) { sleep(60); return 0; }\n')
        compiled = subprocess.run(['cc', str(source), '-o', str(self.binary)], capture_output=True)
        self.assertEqual(compiled.returncode, 0, 'native test helper compile failed')
        self.seed.write_bytes(self.binary.read_bytes())
        self.binary.chmod(0o500)
        self.seed.chmod(0o500)
        self.config_path = self.root / 'config.toml'
        self.config_path.write_text(
            'data_dir = "' + str(self.root / 'hub') + '"\nbind = "127.0.0.1:18443"\n'
            '[tls]\npublic_url = "https://127.0.0.1:18443"\n')
        self.config_path.chmod(0o600)
        self.child = subprocess.Popen([str(self.binary), '--config', str(self.config_path), 'serve'])
        self.addCleanup(self.stop)
        hub = self.native.process_identity(self.child.pid)
        launcher = self.native.process_identity(os.getpid())
        self.config = {
            'binary_path': str(self.binary), 'seed_binary_path': str(self.seed),
            'binary_sha256': hashlib.sha256(self.binary.read_bytes()).hexdigest(),
            'seed_binary_sha256': hashlib.sha256(self.seed.read_bytes()).hexdigest(),
            'hub_pid': self.child.pid, 'launcher_pid': os.getpid(), 'hub_started_at': hub['started_at'],
            'launcher_started_at': launcher['started_at'], 'ready_path': str(self.root / 'ready.json'),
            'config_path': str(self.config_path), 'endpoint': 'https://127.0.0.1:18443',
            'status': 'ready', 'provenance': 'synthetic-real-process'}
        self.write_ready()

    def stop(self):
        if self.child.poll() is None:
            self.child.terminate()
        self.child.wait(timeout=5)

    def write_ready(self):
        path = Path(self.config['ready_path'])
        path.write_text(json.dumps(self.config))
        path.chmod(0o600)

    def verify(self):
        return self.native.verify(self.config)

    def test_live_owned_native_process_and_retained_hashes_verify(self):
        self.assertEqual(self.verify()['status'], 'verified')

    def test_missing_malformed_mismatched_binary_evidence_rejected(self):
        original = copy.deepcopy(self.config)
        for key in ('binary_path', 'seed_binary_path', 'binary_sha256', 'seed_binary_sha256', 'hub_pid', 'launcher_pid', 'ready_path'):
            self.config = copy.deepcopy(original)
            del self.config[key]
            with self.subTest(missing=key), self.assertRaises(ValueError):
                self.verify()
        for key, value in [('binary_sha256', None), ('seed_binary_sha256', True), ('binary_sha256', 'not-a-digest'),
                           ('binary_path', 'relative'), ('hub_pid', True), ('launcher_pid', 0),
                           ('binary_sha256', '0' * 64), ('seed_binary_sha256', '1' * 64)]:
            self.config = copy.deepcopy(original)
            self.config[key] = value
            self.write_ready()
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                self.verify()
        self.config = original
        self.write_ready()
        self.seed.chmod(0o700)
        self.seed.write_bytes(b'changed input')
        self.seed.chmod(0o500)
        with self.assertRaisesRegex(ValueError, 'digest'):
            self.verify()

    def test_stale_or_unbound_readiness_cannot_pass(self):
        original = dict(self.config)
        for key, value in [('hub_started_at', 'stale'), ('launcher_started_at', 'stale'), ('status', 'stopped'),
                           ('binary_path', str(self.seed))]:
            self.config = dict(original)
            self.config[key] = value
            self.write_ready()
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.verify()
        self.config = dict(original)
        self.write_ready()
        self.config['hub_started_at'] = 'descriptor disagrees with retained witness'
        with self.assertRaises(ValueError):
            self.verify()
        self.config = dict(original)
        self.write_ready()
        stopped = self.root / 'stopped.json'
        stopped.write_text('{}')
        stopped.chmod(0o600)
        with self.assertRaisesRegex(ValueError, 'stale'):
            self.verify()
        stopped.unlink()
        self.stop()
        with self.assertRaises(ValueError):
            self.verify()


class SharedSurfaceTests(unittest.TestCase):
    def test_hub_http_keeps_only_the_shared_surface(self):
        for name in RETAINED:
            with self.subTest(kept=name):
                self.assertTrue(hasattr(hub_http, name), name)
        for name in RETIRED:
            with self.subTest(retired=name):
                self.assertFalse(hasattr(hub_http, name), name)


if __name__ == '__main__':
    unittest.main()
