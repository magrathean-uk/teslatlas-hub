#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Git-history controls, additional to the candidate-manifest hermetic suite."""
import base64
import json
import importlib.util
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('name_policy_history_gate', ROOT / 'scripts/verify-name-policy.py')
gate = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(gate)


class GitHistoryTests(unittest.TestCase):
    def test_committed_history_and_explicit_growth(self):
        with tempfile.TemporaryDirectory(prefix='name-policy-history-') as temporary:
            root = Path(temporary)
            def git(*args):
                subprocess.run(['git', '-C', str(root), *args], check=True, capture_output=True)
            git('init', '-q')
            git('config', 'user.name', 'Synthetic test')
            git('config', 'user.email', 'synthetic@example.invalid')
            (root / 'sample.txt').write_text('Neutral sample.\n')
            git('add', 'sample.txt')
            git('commit', '-qm', 'Synthetic predecessor')
            for name in ['scripts/verify-name-policy.py', 'scripts/test-name-policy.sh']:
                destination = root / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, destination)
            data = root / 'scripts/name-policy'
            data.mkdir()
            allow = {'schema': 'teslatlas.name-policy-allow/v1', 'entries': [],
                     'extra_patterns_b64': [], 'references': {prefix: [
                         'scripts/verify-name-policy.py', 'scripts/name-policy/allow.json'
                     ] for prefix in gate.REQUIRED_REFERENCES}}
            (data / 'allow.json').write_text(json.dumps(allow))
            (data / 'pins.json').write_text(json.dumps({'files': []}))
            (data / 'milestones.json').write_text(json.dumps({
                'enabled_classes': ['frozen-history', 'g2-4-recorded', 'read-sql'], 'closed': []}))
            git('add', '.')
            git('commit', '-qm', 'Introduce synthetic naming gate')
            def run(expected, *args):
                result = subprocess.run([sys.executable, '-B', str(root / 'scripts/verify-name-policy.py'),
                    '--repo', str(root), '--strict', *args], capture_output=True)
                self.assertEqual(result.returncode, expected, result.stderr.decode())
            run(0, '--base', 'HEAD^')
            run(0, '--base', 'HEAD')
            name = 'tesl' + 'amate'
            (root / 'sample.txt').write_text(name)
            run(1, '--base', 'HEAD')
            allow['entries'] = [{'id': 'synthetic-history', 'class': 'frozen-history',
                'paths': ['sample.txt'], 'match_b64': base64.b64encode(name.encode()).decode(),
                'max_count': 1, 'expires': 'G2-4', 'review_by': '9999-12-31',
                'reason': 'Synthetic ratchet control', 'authority': 'Hermetic fixture'}]
            (data / 'allow.json').write_text(json.dumps(allow))
            run(1, '--base', 'HEAD')
            run(0, '--base', 'HEAD', '--accept-growth', 'Synthetic explicit growth witness')


if __name__ == '__main__':
    unittest.main(verbosity=2)
