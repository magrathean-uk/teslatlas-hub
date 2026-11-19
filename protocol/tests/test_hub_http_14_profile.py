"""Independent hub-http-v1@1.4.0 contract tests; no Hub implementation is imported.

The profile is hub-http-v1@1.3.0 plus one additive, negotiated drive member,
elevation_coverage. Expected values are written out literally here.
"""
import hashlib
import json
from pathlib import Path
import unittest

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / 'profiles/hub-http-v1/1.4.0'
BASE = ROOT / 'profiles/hub-http-v1/1.3.0'


def load(path):
    return json.loads(path.read_text(encoding='utf-8'))


class HubHttp14ProfileTests(unittest.TestCase):
    def validator(self):
        base = load(BASE / 'resources.schema.json')
        schema = load(PROFILE / 'drive-elevation-coverage.schema.json')
        registry = Registry().with_resources([
            (base['$id'], Resource.from_contents(base)),
            (schema['$id'], Resource.from_contents(schema)),
        ])
        drive = {'$ref': schema['$id'] + '#/$defs/drive_1_4_0'}
        return Draft202012Validator(drive, registry=registry)

    def test_identity_negotiation_and_values_are_exact(self):
        profile = load(PROFILE / 'profile.json')
        self.assertEqual(profile['profile_id'], 'hub-http-v1@1.4.0')
        self.assertEqual(profile['previous_profile'], 'hub-http-v1@1.3.0')
        self.assertEqual(profile['license'], 'Apache-2.0')
        self.assertEqual(profile['negotiation']['request_header'], 'x-teslatlas-http-profile')
        self.assertEqual(profile['negotiation']['value'], 'hub-http-v1@1.4.0')
        addition = profile['additions']['drive.elevation_coverage']
        self.assertEqual(sorted(addition['values']), ['complete', 'partial', 'unknown'])
        self.assertEqual(addition['absent_means'], 'unknown')
        schema = load(PROFILE / 'drive-elevation-coverage.schema.json')
        self.assertEqual(schema['$defs']['elevation_coverage'], {'enum': ['complete', 'partial', 'unknown']})

    def test_example_is_the_13_drive_plus_the_member(self):
        example = load(PROFILE / 'examples/drive.json')
        validator = self.validator()
        self.assertEqual(list(validator.iter_errors(example)), [])
        base = dict(example)
        del base['elevation_coverage']
        self.assertEqual(sorted(base), sorted(load(BASE / 'examples/drive.json')))
        for wrong in ('full', None, 1):
            broken = dict(example, elevation_coverage=wrong)
            self.assertTrue(list(validator.iter_errors(broken)), wrong)
        self.assertTrue(list(validator.iter_errors(base)))

    def test_cases_cover_each_state_and_old_clients(self):
        cases = {case['id']: case for case in load(PROFILE / 'cases.json')['cases']}
        self.assertEqual(cases['http14-no-header-is-1-3-0']['request_headers'], {})
        self.assertEqual(cases['http14-gap-is-partial']['expect']['elevation_coverage'], 'partial')
        self.assertEqual(cases['http14-invalid-is-partial']['expect']['elevation_coverage'], 'partial')
        filled = cases['http14-filled-gap-is-complete-and-republished']['expect']
        self.assertEqual((filled['elevation_coverage'], filled['drive_republished']), ('complete', True))
        self.assertEqual(cases['http14-insufficient-is-unknown']['expect']['elevation_coverage'], 'unknown')

    def test_checksums_cover_every_file(self):
        lines = (PROFILE / 'SHA256SUMS').read_text(encoding='utf-8').splitlines()
        listed = {}
        for line in lines:
            digest, name = line.split('  ', 1)
            listed[name] = digest
        files = sorted(
            path.relative_to(PROFILE).as_posix()
            for path in PROFILE.rglob('*')
            if path.is_file() and path.name != 'SHA256SUMS'
        )
        self.assertEqual(sorted(listed), files)
        for name, digest in listed.items():
            self.assertEqual(hashlib.sha256((PROFILE / name).read_bytes()).hexdigest(), digest, name)


if __name__ == '__main__':
    unittest.main()
