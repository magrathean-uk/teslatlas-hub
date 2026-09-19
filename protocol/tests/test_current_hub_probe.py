"""Executable tests for the bounded, read-only current-Hub curl probe."""
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
PROBE = ROOT / "tools" / "check-current-hub"
PROFILE = ROOT / "profiles" / "hub-http-v1" / "1.0.0"
SECRET = "Bearer private-token-that-must-never-appear"
HUB_ID = "11111111-1111-4111-8111-111111111111"


class CurrentHubProbeTests(unittest.TestCase):
    """The fake curl is the controlled transport boundary; the probe stays real."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.private = self.root / "private"
        self.private.mkdir(mode=0o700)
        self.header = self.private / "authorization.header"
        self.header.write_text(f"Authorization: {SECRET}\n", encoding="utf-8")
        self.header.chmod(0o600)
        self.ca = self.private / "ca.pem"
        self.ca.write_text("synthetic CA only\n", encoding="utf-8")
        self.ca.chmod(0o600)
        self.config = self.private / "probe.json"
        self.write_config()
        self.log = self.root / "curl-log.jsonl"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.force_cursor = False
        self.reduced_capabilities = False
        self.write_fake_curl()

    def write_config(self, **updates):
        config = {
            "endpoint": "https://synthetic.example.test:8443",
            "expected_hub_id": HUB_ID,
            "ca_path": str(self.ca),
            "authorization_header_path": str(self.header),
            "vehicle_id": HUB_ID,
            "from_ms": 0,
            "to_ms": 9223372036854775807,
            "limit": 2,
            "max_pages": 3,
            "timeout_seconds": 30,
        }
        config.update(updates)
        self.config.write_text(json.dumps(config), encoding="utf-8")
        self.config.chmod(0o600)

    def write_fake_curl(self):
        script = self.bin / "curl"
        script.write_text(
            f"""#!{sys.executable}
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
Path(os.environ['PROBE_CURL_LOG']).open('a', encoding='utf-8').write(json.dumps(args) + '\\n')
def value(flag): return args[args.index(flag) + 1]
output = Path(value('--output')); headers = Path(value('--dump-header'))
url = args[-1]
root = Path(os.environ['PROBE_PROFILE'])
if '/.well-known/' in url:
    body = (root / 'examples/discovery.json').read_bytes()
    if os.environ.get('PROBE_REDUCED_CAPABILITIES') == '1':
        discovery = json.loads(body)
        discovery['capabilities'] = ['query.vehicles', 'query.current']
        body = json.dumps(discovery).encode()
    kind = 'discovery'; status = 200
elif url.endswith('/v1/vehicles'): body = (root / 'examples/vehicles.json').read_bytes(); kind = 'vehicles'; status = 200
elif '/current' in url: body = (root / 'examples/current.json').read_bytes(); kind = 'current'; status = 200
elif '/drives' in url:
    if '--header' in args and args[args.index('--header') + 1].startswith('If-None-Match:'):
        body = b''; status = 304
    else:
        body = (root / 'examples/drives.json').read_bytes()
        if os.environ.get('PROBE_FORCE_CURSOR') == '1':
            page = json.loads(body); page['next_cursor'] = 'opaque-cursor'; body = json.dumps(page).encode()
        status = 200
    kind = 'drives'
else: body = b''; kind = 'unknown'; status = 404
headers.write_text('HTTP/1.1 %s synthetic\\r\\nContent-Type: application/json\\r\\n' % status + ('ETag: \\\"probe-page\\\"\\r\\nCache-Control: no-store\\r\\n' if kind == 'drives' else '') + '\\r\\n', encoding='ascii')
output.write_bytes(body)
sys.stdout.write(str(status))
""",
            encoding="utf-8",
        )
        script.chmod(script.stat().st_mode | stat.S_IXUSR)

    def run_probe(self):
        environment = dict(os.environ)
        environment.update(
            {
                "PATH": str(self.bin) + os.pathsep + environment["PATH"],
                "PROBE_CURL_LOG": str(self.log),
                "PROBE_PROFILE": str(PROFILE),
                "PROBE_FORCE_CURSOR": "1" if self.force_cursor else "0",
                "PROBE_REDUCED_CAPABILITIES": "1" if self.reduced_capabilities else "0",
            }
        )
        environment["PATH"] = str(ROOT / ".venv" / "bin") + os.pathsep + environment["PATH"]
        return subprocess.run(
            [str(PROBE), "--config", str(self.config)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            env=environment,
            timeout=10,
        )

    def test_complete_smoke_uses_curl_header_file_and_redacts_private_values(self):
        """Removing profile validation or expanding the bearer into argv breaks this."""
        result = self.run_probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads(result.stdout)
        self.assertEqual(receipt["status"], "passed")
        self.assertEqual(receipt["evidence_kind"], "remote-wire-smoke")
        self.assertEqual(receipt["operations"], ["discovery", "vehicles", "current", "drives", "drives-if-none-match"])
        self.assertNotIn("private-token", result.stdout + result.stderr)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(calls), 5)
        self.assertNotIn("--header", calls[0])
        for call in calls[1:]:
            self.assertIn("@" + str(self.header), call)
            self.assertNotIn("private-token", " ".join(call))
            self.assertEqual(call[call.index("--retry") + 1], "0")
            self.assertNotIn("--location", call)

    def test_unknown_config_member_fails_before_any_transport(self):
        """Accepting arbitrary options would permit uncontrolled curl behavior."""
        self.write_config(curl_option="--insecure")
        result = self.run_probe()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout), {"status": "failed", "error": "probe configuration invalid"})
        self.assertFalse(self.log.exists())
        self.assertNotIn("private-token", result.stdout + result.stderr)

    def test_reduced_discovery_capabilities_skip_optional_drives(self):
        """A Hub without query.drives still supports the minimal current-Hub slice."""
        self.reduced_capabilities = True
        result = self.run_probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads(result.stdout)
        self.assertEqual(receipt["status"], "passed")
        self.assertEqual(receipt["operations"], ["discovery", "vehicles", "current"])
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(calls), 3)
        self.assertTrue(all("/drives" not in call[-1] for call in calls))

    def test_equal_drive_bounds_fail_before_any_transport(self):
        """Current-Hub requires a half-open drive window with from_ms < to_ms."""
        self.write_config(from_ms=50, to_ms=50)
        result = self.run_probe()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout), {"status": "failed", "error": "probe configuration invalid"})
        self.assertFalse(self.log.exists())

    def test_page_limit_reports_partial_without_claiming_history_complete(self):
        """Treating a non-null cursor at the configured page bound as success is unsafe."""
        self.write_config(max_pages=1)
        self.force_cursor = True
        result = self.run_probe()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout)["status"], "partial")
        self.assertEqual(json.loads(result.stdout)["operations"], ["discovery", "vehicles", "current", "drives", "drives-if-none-match"])


if __name__ == "__main__":
    unittest.main()
