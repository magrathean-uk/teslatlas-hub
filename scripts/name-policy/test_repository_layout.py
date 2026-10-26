#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Layout refusal controls over candidate manifests; no VCS commands."""
import hashlib
import json
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
VERIFIER = ROOT / 'scripts/verify-repository-layout.py'
REQUIRED = ['scripts/verify-name-policy.py', 'scripts/test-name-policy.sh'] + [
    'scripts/name-policy/' + name + '.json' for name in ('allow', 'pins', 'milestones')]


class LayoutTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='hub-layout-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'candidate'
        self.root.mkdir()
        for name in ['README', 'SECURITY', 'SUPPORT', 'CONTRIBUTING', 'CODE_OF_CONDUCT']:
            self.put('.github/' + name + '.md', '# ' + name + '\n')
        self.put('docs/index.md', '# Documentation\n[Guide](guides/operator-guide.md)\n')
        self.put('docs/guides/operator-guide.md', '# Guide\n[Security](../../.github/SECURITY.md)\n')
        for domain in ['api', 'application', 'auth', 'collection', 'geo', 'import', 'platform', 'runtime', 'storage', 'sync']:
            self.put('src/' + domain + '/mod.rs', '// SPDX-License-Identifier: AGPL-3.0-only\n')
        self.put('src/lib.rs', '// SPDX-License-Identifier: AGPL-3.0-only\n')
        for path in REQUIRED:
            self.put(path, '{}\n' if path.endswith('.json') else '# Synthetic fixture\n')

    def put(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def check(self, expected):
        files = []
        for path in sorted(self.root.rglob('*')):
            if path.is_file():
                payload = path.read_bytes()
                files.append(dict(path=path.relative_to(self.root).as_posix(), sha256=hashlib.sha256(payload).hexdigest(),
                                  bytes=len(payload), mode=f'{stat.S_IMODE(path.stat().st_mode):04o}'))
        manifest = Path(self.temp.name) / 'candidate.json'
        manifest.write_text(json.dumps(dict(schema='teslatlas.candidate-source-manifest/v1', files=files)))
        result = subprocess.run([sys.executable, '-B', str(VERIFIER), '--repo', str(self.root), '--candidate-manifest', str(manifest)],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.assertEqual(result.returncode == 0, expected, result.stdout + result.stderr)

    def test_valid(self):
        self.check(True)

    def test_every_gate_file_is_required(self):
        for name in REQUIRED:
            path = self.root / name
            content = path.read_bytes()
            path.unlink()
            with self.subTest(path=name):
                self.check(False)
            path.write_bytes(content)

    def test_root_markdown(self):
        self.put('README.md', '# Disallowed\n')
        self.check(False)

    def test_document_name(self):
        self.put('docs/guides/Bad_Name.md', '# Disallowed\n')
        self.check(False)

    def test_broken_link(self):
        self.put('docs/index.md', '[Missing](missing.md)\n')
        self.check(False)

    def test_tool_metadata(self):
        self.put('.claude/settings.md', '# Local settings\n')
        self.check(False)

    def test_flat_source(self):
        self.put('src/server.rs', '// Flat\n')
        self.check(False)

    def test_rust_line_limit(self):
        self.put('src/api/oversized.rs', '// line\n' * 3001)
        self.check(False)

    def test_owner_lock(self):
        self.put('NOTICE', 'Approved notice\n')
        digest = hashlib.sha256((self.root / 'NOTICE').read_bytes()).hexdigest()
        self.put('docs/legal/owner-controlled-files.sha256', '# Owner-controlled legal files\n' + digest + '  NOTICE\n')
        self.check(True)
        self.put('NOTICE', 'Changed notice\n')
        self.check(False)


if __name__ == '__main__':
    unittest.main(verbosity=2)
