#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Hermetic naming controls using content-bound mirror fixtures, without Git."""
import base64
import copy
import hashlib
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

SCRIPT = Path(__file__).with_name('verify-name-policy.py')
if not SCRIPT.is_file():
    SCRIPT = Path(__file__).resolve().parents[1] / 'verify-name-policy.py'
spec = importlib.util.spec_from_file_location('name_policy', SCRIPT)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)
FIRST = 'tesla'
LAST = 'mate'
NAME = FIRST + LAST
CLASS = 'frozen-history'


def encoded(text):
    return base64.b64encode(text.encode()).decode()


class MatcherTests(unittest.TestCase):
    def test_normalization_separators_longer_words_and_mapping(self):
        samples = [NAME, NAME.upper(), 'my' + NAME, FIRST + '\n' + LAST,
                   FIRST + '\u200d' + LAST, FIRST + '\u00ad' + LAST,
                   FIRST + '\u00a0' + LAST, FIRST + '    --' + LAST,
                   ''.join(chr(ord(c) + 0xFEE0) for c in NAME)]
        samples += [FIRST + separator + LAST for separator in [' ', '.', '/', '_', '-', '\u2010', '\u2013', '\u2212', ':', '\u00b7', '+', "'", '\\']]
        samples += [FIRST + '%20' + LAST, FIRST + '%2D' + LAST,
                    FIRST + r'\u0020' + LAST, FIRST + r'\n' + LAST,
                    FIRST + '&#32;' + LAST, FIRST + '&#x20;' + LAST,
                    FIRST + '</b>' + LAST, FIRST + '<br\n/>' + LAST,
                    FIRST + '**' + LAST, FIRST + '_' + LAST,
                    FIRST[:4] + '\u0430' + LAST,
                    FIRST + LAST[:2] + '\u03c4' + LAST[3:]]
        for sample in samples:
            with self.subTest(sample=repr(sample)):
                matches = gate.find_matches('escaped %20 prefix\n' + sample)
                self.assertEqual(len(matches), 1)
                self.assertEqual(matches[0]['line'], 2)
                self.assertEqual(matches[0]['offset'], len('escaped %20 prefix\n') + (2 if sample.startswith('my') else 0))
        self.assertEqual(gate.find_matches(FIRST + '-------' + LAST), [])

    def test_raw_decoded_duplicates_count_once(self):
        self.assertEqual(len(gate.find_matches(NAME + ' ' + FIRST + '%20' + LAST)), 2)

    def test_unicode_percent_bytes_and_html_letters(self):
        escaped = ''.join('%%%02x' % b for b in NAME.encode())
        self.assertEqual(len(gate.find_matches(escaped)), 1)
        self.assertEqual(len(gate.find_matches(FIRST + '&#109;' + LAST[1:])), 1)

    def test_extra_pattern(self):
        self.assertEqual(len(gate.find_matches('prefix synthetic-label', ['synthetic-label'])), 1)

    def test_whole_nfkc_composition_and_multiple_decodes_keep_source_offset(self):
        prefix = 'e\u0301 reordering a\u0315\u0300 %20 &#32; <b>prefix</b>\n'
        sample = prefix + FIRST + r'\u0020' + LAST
        hits = gate.find_matches(sample)
        self.assertEqual(len(hits), 1)
        self.assertEqual(hits[0]['offset'], len(prefix))
        self.assertEqual(hits[0]['line'], 2)


class GateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='hub-name-policy-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'candidate'
        self.base = Path(self.temp.name) / 'base'
        (self.root / 'scripts/name-policy').mkdir(parents=True)
        shutil.copyfile(SCRIPT, self.root / gate.SCRIPT_PATH)
        (self.root / 'scripts/test-name-policy.sh').write_text('# Synthetic fixture\n')
        self.data = dict(allow=dict(entries=[], references={prefix:[gate.SCRIPT_PATH, gate.DATA_PATHS[0]] for prefix in gate.REQUIRED_REFERENCES}, extra_patterns_b64=[]),
                         pins=dict(files=[]), milestones=dict(enabled_classes=sorted(gate.DEFAULT_CLASSES), closed=[], class_authorities={}))
        self.files = ['sample.txt']
        (self.root / 'sample.txt').write_text('Neutral sample.\n')
        self.save()
        shutil.copytree(self.root, self.base)

    def save(self):
        for name, value in self.data.items():
            (self.root / ('scripts/name-policy/' + name + '.json')).write_text(json.dumps(value))
        (Path(self.temp.name) / 'files.json').write_text(json.dumps(sorted(set(self.files + gate.REQUIRED_INPUTS))))

    def run_gate(self, expected=0, *extra):
        self.save()
        command = [sys.executable, '-B', str(SCRIPT), '--repo', str(self.root), '--files-from',
                   str(Path(self.temp.name) / 'files.json'), '--base-dir', str(self.base)]
        result = subprocess.run(command + list(extra), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if expected == 0:
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0, result.stderr + result.stdout)
        return result

    def allow_sample(self, count=1):
        self.data['allow']['entries'] = [dict(id='sample', **{'class': CLASS}, paths=['sample.txt'],
            match_b64=encoded(NAME), max_count=count, expires='G2-4', review_by='9999-01-01',
            reason='Synthetic test control', authority='Synthetic test authority')]
        (self.root / 'sample.txt').write_text(NAME + '\n')

    def reseed(self):
        self.save()
        shutil.rmtree(self.base)
        shutil.copytree(self.root, self.base)

    def test_neutral_passes_new_occurrence_fails(self):
        self.run_gate()
        (self.root / 'sample.txt').write_text(NAME)
        self.run_gate(1)

    def test_growth_reason_and_summary(self):
        self.allow_sample()
        self.run_gate(1)
        summary = Path(self.temp.name) / 'summary.json'
        self.run_gate(0, '--accept-growth', 'Synthetic fixture entry', '--summary', str(summary))
        self.assertEqual(json.loads(summary.read_text())['accepted_growth'], 'Synthetic fixture entry')

    def test_count_mismatch_and_reduction(self):
        self.allow_sample(2)
        self.reseed()
        self.run_gate()
        self.run_gate(1, '--strict')
        self.data['allow']['entries'][0]['max_count'] = 1
        self.run_gate(0, '--strict')
        self.data['allow']['entries'][0]['max_count'] = 0
        self.run_gate(1, '--strict')

    def test_entry_field_growth(self):
        self.allow_sample()
        self.reseed()
        for key, value in [('paths', ['*']), ('match_b64', encoded(FIRST)), ('class', 'read-sql'),
                           ('expires', 'G2-5'), ('review_by', '9999-12-31'), ('max_count', 2)]:
            original = copy.deepcopy(self.data)
            self.data['allow']['entries'][0][key] = value
            with self.subTest(key=key):
                self.run_gate(1)
            self.data = original
        self.data['allow']['entries'][0]['review_by'] = '9998-01-01'
        self.run_gate()

    def test_expiry_and_review_date(self):
        self.allow_sample()
        self.reseed()
        self.data['milestones']['closed'] = ['G2-4']
        self.run_gate(1)
        self.data['milestones']['closed'] = []
        self.data['allow']['entries'][0]['review_by'] = '2000-01-01'
        self.run_gate(1)

    def test_closed_milestone_cannot_be_deleted_even_with_growth(self):
        self.data['milestones']['closed'] = ['G2-4-M1']
        self.reseed()
        self.data['milestones']['closed'] = []
        self.run_gate(1, '--accept-growth', 'Cannot reopen a milestone')

    def test_permanent_entry_requires_pin(self):
        self.allow_sample()
        self.data['allow']['entries'][0]['expires'] = 'none'
        self.reseed()
        self.run_gate(1)
        self.data['pins']['files'] = [dict(path='sample.txt', sha256=hashlib.sha256((self.root / 'sample.txt').read_bytes()).hexdigest())]
        self.run_gate(0, '--accept-growth', 'Synthetic pin')

    def test_recorded_exception_requires_row(self):
        self.allow_sample()
        self.data['allow']['entries'][0]['class'] = 'g2-4-recorded'
        self.reseed()
        self.run_gate(1)
        self.data['allow']['entries'][0]['recorded_row'] = 'Synthetic coordinator row 2026-10-26'
        self.run_gate(0, '--accept-growth', 'Record synthetic authority')

    def test_exactly_one_entry(self):
        self.allow_sample()
        second = copy.deepcopy(self.data['allow']['entries'][0])
        second['id'] = 'second'
        self.data['allow']['entries'].append(second)
        self.reseed()
        self.run_gate(1)

    def test_disabled_class_and_authority(self):
        self.allow_sample()
        self.data['allow']['entries'][0].update({'class': 'deprecated-alias', 'expires': 'G2-9'})
        self.reseed()
        self.run_gate(1)
        self.data['milestones']['enabled_classes'].append('deprecated-alias')
        self.run_gate(1, '--accept-growth', 'Enable test class')
        self.data['milestones']['class_authorities']['deprecated-alias'] = 'Synthetic owner row'
        self.run_gate(1)
        self.run_gate(0, '--accept-growth', 'Synthetic owner row')

    def test_missing_base_and_deleted_readded_gate_data(self):
        (self.base / gate.DATA_PATHS[0]).unlink()
        self.run_gate(1)
        shutil.rmtree(self.base / 'scripts')
        self.run_gate(1)

    def test_mirror_requires_explicit_base(self):
        arguments = SimpleNamespace(base_dir=None, repo=self.root,
            files_from=Path(self.temp.name) / 'files.json', candidate_manifest=None, base=None)
        with mock.patch.object(gate, 'git_bytes', side_effect=AssertionError('VCS access is forbidden in mirror mode')) as vcs:
            with self.assertRaisesRegex(gate.PolicyError, 'mirror runs require --base-dir or --base'):
                gate.load_base(arguments)
            vcs.assert_not_called()

    def test_alias_import_reference_requires_coverage(self):
        (self.root / 'sample.txt').write_text('use crate::runtime::legacy_names as frozen;\nfrozen::FORMAT\n')
        result = self.run_gate(1)
        self.assertIn('unlisted frozen reference', result.stderr)
        self.data['allow']['references']['legacy_names::'].append('sample.txt')
        self.run_gate(0, '--accept-growth', 'Synthetic aliased reference coverage')

    def test_raw_and_escaped_policy_data_is_not_exempt(self):
        self.files = ['scripts/name-policy/' + name + '.json' for name in ('allow', 'pins', 'milestones')]
        for name in ('allow', 'pins', 'milestones'):
            original = copy.deepcopy(self.data)
            self.data[name]['test_data'] = NAME
            self.run_gate(1, '--accept-growth', 'Raw data is never an exception')
            self.data = original
        self.files = ['provenance-manifest.json']
        (self.root / self.files[0]).write_text(json.dumps({'rules': [{'id': 'neutral', 'rationale': NAME}]}))
        self.run_gate(1)
        (self.root / self.files[0]).write_text('{"rationale":"' + FIRST + r'\u0020' + LAST + '"}')
        self.run_gate(1)

    def test_expiry_is_restricted_to_its_class(self):
        for expiry, category in [('G2-9', 'frozen-history'), ('C6-R52a', 'read-sql'),
                                 ('N1b', 'frozen-history'), ('N1c', 'frozen-history')]:
            with self.subTest(expiry=expiry, category=category):
                self.allow_sample()
                entry = self.data['allow']['entries'][0]
                entry['expires'] = expiry
                entry['class'] = category
                self.reseed()
                self.run_gate(1)

    def test_regex_alternatives_match_the_complete_line(self):
        self.allow_sample()
        (self.root / 'sample.txt').write_text(NAME + ' sentinel\n')
        entry = self.data['allow']['entries'][0]
        entry['match_kind'] = 'regex'
        entry['match_b64'] = encoded('^unrelated|sentinel$')
        self.reseed()
        self.run_gate(1)
        entry['match_b64'] = encoded('^' + re.escape(NAME) + ' sentinel$')
        self.reseed()
        self.run_gate()

    def test_binary_path_and_digest(self):
        self.files = ['asset.png']
        (self.root / 'asset.png').write_bytes(b'\x89PNG\0synthetic')
        self.run_gate(1)
        self.data['pins']['files'] = [dict(path='asset.png', sha256=hashlib.sha256((self.root / 'asset.png').read_bytes()).hexdigest())]
        self.run_gate(1)
        self.run_gate(0, '--accept-growth', 'Synthetic binary pin')
        self.reseed()
        (self.root / 'asset.png').write_bytes(b'changed\0')
        self.run_gate(1)
        path = NAME + '.png'
        (self.root / path).write_bytes(b'\x00')
        self.files = [path]
        self.data['pins']['files'] = [dict(path_b64=encoded(path), sha256=hashlib.sha256(b'\x00').hexdigest())]
        self.run_gate(1, '--accept-growth', 'Path still fails')

    def test_changelog_tail_and_archive_pin(self):
        self.files = ['changelog.md']
        path = self.root / self.files[0]
        path.write_text('# Changes\n\n## Released\nOld content\n')
        tail = b'## Released\nOld content\n'
        self.data['pins']['files'] = [dict(path=self.files[0], anchor='## Released', sha256=hashlib.sha256(tail).hexdigest())]
        self.reseed()
        self.run_gate()
        path.write_text('# Changes\n\n## New entry\nNew content\n\n' + tail.decode())
        self.run_gate()
        path.write_text('# Changes\n\n## Released\nAltered content\n')
        self.run_gate(1)
        path.write_text('# Changes\nOld content\n## Released\n')
        self.run_gate(1)
        self.files = ['docs/development/archive/control.txt']
        (self.root / self.files[0]).parent.mkdir(parents=True)
        (self.root / self.files[0]).write_text('Archive')
        self.data['pins']['files'] = []
        self.run_gate(1, '--accept-growth', 'Archives require pins')

    def test_both_governed_changelogs_require_tail_pins(self):
        for relative in ['docs/releases/changelog.md', 'docs/legal/changelog.md']:
            self.files = [relative]
            self.put_file(relative, '# Changes\n\n## Released\nOld\n')
            self.data['pins']['files'] = []
            self.run_gate(1, '--accept-growth', 'Missing mandatory pin')
            payload = (self.root / relative).read_bytes()
            self.data['pins']['files'] = [dict(path=relative, sha256=hashlib.sha256(payload).hexdigest())]
            self.run_gate(1, '--accept-growth', 'Whole-file pin is insufficient')
            self.data['pins']['files'] = [dict(path=relative, anchor='## Released', sha256=hashlib.sha256(b'## Released\nOld\n').hexdigest())]
            self.run_gate(0, '--accept-growth', 'Synthetic tail')

    def put_file(self, relative, text):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def test_symlinked_base_data_is_refused(self):
        path = self.base / gate.DATA_PATHS[0]
        path.unlink()
        path.symlink_to(self.root / gate.DATA_PATHS[0])
        self.run_gate(1)

    def test_gate_inputs_cannot_be_omitted(self):
        for omitted in gate.REQUIRED_INPUTS:
            self.save()
            path = Path(self.temp.name) / 'files.json'
            paths = json.loads(path.read_text())
            path.write_text(json.dumps([item for item in paths if item != omitted]))
            result = subprocess.run([sys.executable, '-B', str(SCRIPT), '--repo', str(self.root),
                '--files-from', str(path), '--base-dir', str(self.base)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertNotEqual(result.returncode, 0)

    def test_extra_patterns_references_and_forbidden_directories(self):
        self.data['allow']['extra_patterns_b64'] = [encoded('synthetic-extra')]
        self.reseed()
        (self.root / 'sample.txt').write_text('synthetic-extra')
        self.run_gate(1)
        self.data['allow']['extra_patterns_b64'] = []
        self.run_gate(1)
        (self.root / 'sample.txt').write_text('legacy_names::FROZEN')
        self.run_gate(1, '--accept-growth', 'No unlisted reference')
        self.data['allow']['references']['legacy_names::'].append('sample.txt')
        self.run_gate(0, '--accept-growth', 'Synthetic reference location')
        self.files = ['vendor/sample.txt']
        (self.root / 'vendor').mkdir()
        (self.root / self.files[0]).write_text('Neutral')
        self.run_gate(1, '--accept-growth', 'Forbidden directory still fails')

    def test_working_tree_cannot_be_its_own_base(self):
        self.allow_sample()
        self.run_gate(1)
        self.base = self.root
        self.run_gate(1)

    def test_commit_message_has_no_exception(self):
        message = Path(self.temp.name) / 'message.txt'
        for text, expected in [('Neutral change', 0), (FIRST + r'\n' + LAST, 1)]:
            message.write_text(text)
            result = subprocess.run([sys.executable, '-B', str(SCRIPT), '--repo', str(self.root), '--commit-msg', str(message)], stdout=subprocess.PIPE)
            self.assertEqual(result.returncode, expected)

    def test_production_date_cannot_be_overridden(self):
        self.run_gate(1, '--today', '2000-01-01')

    def test_above_maximum_fails(self):
        self.allow_sample()
        self.reseed()
        (self.root / 'sample.txt').write_text(NAME + '\n' + NAME)
        self.run_gate(1)

    def test_unanchored_allow_pattern_is_refused(self):
        self.allow_sample()
        self.data['allow']['entries'][0].update(match_kind='regex', match_b64=encoded('.*'))
        self.reseed()
        self.run_gate(1)

    def test_commit_message_extra_patterns_are_enforced(self):
        self.data['allow']['extra_patterns_b64'] = [encoded('synthetic-extra')]
        self.save()
        message = Path(self.temp.name) / 'message.txt'
        message.write_text('synthetic-extra')
        result = subprocess.run([sys.executable, '-B', str(SCRIPT), '--repo', str(self.root), '--commit-msg', str(message)], stdout=subprocess.PIPE)
        self.assertNotEqual(result.returncode, 0)

    def test_count_only_is_report_evidence_never_strict_exemption(self):
        (self.root / 'sample.txt').write_text(NAME)
        self.run_gate(0, '--report', '--count-only', 'sample.txt')
        self.run_gate(1, '--strict', '--count-only', 'sample.txt')



if __name__ == '__main__':
    unittest.main(verbosity=2)
