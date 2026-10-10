"""Independent hub-http-v1@1.2.0 contract tests; no Hub implementation is imported.

Part one checks the generated profile directory against the G2-0a specification, with every
expected list written out literally here so the generator cannot agree with itself. Part two runs
the native conformance cases against an in-memory fake Hub that follows the contract, and against
variants of it that each break one rule.
"""
import contextlib
import copy
import hashlib
import hmac
import http.client
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import socket
import socketserver
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unicodedata
import unittest
from unittest import mock
import urllib.parse
import uuid

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from jsonschema import Draft202012Validator, FormatChecker  # noqa: E402
from openapi_spec_validator import validate as validate_openapi  # noqa: E402

from conformance import hub_http_12 as conf  # noqa: E402

PROFILE = ROOT / 'profiles/hub-http-v1/1.2.0'
GENERATOR = ROOT / 'tools/build_hub_http_profile_12.py'
PROFILE_ID = 'hub-http-v1@1.2.0'
I64_MAX = 2 ** 63 - 1
I64_MIN = -(2 ** 63)

DRIVE_FIELDS = (
    'id vehicle_id start_date_ms end_date_ms duration_min speed_max start_soc end_soc ascent descent distance_km efficiency '
    'outside_temp_avg inside_temp_avg power_max power_min start_ideal_range_km end_ideal_range_km start_latitude start_longitude '
    'end_latitude end_longitude start_rated_range_km end_rated_range_km start_address end_address start_geofence end_geofence').split()
POSITION_FIELDS = (
    'id drive_id date_ms latitude longitude speed power battery_level usable_battery_level elevation odometer ideal_battery_range_km '
    'est_battery_range_km rated_battery_range_km inside_temp outside_temp is_climate_on fan_status driver_temp_setting '
    'passenger_temp_setting battery_heater battery_heater_on tpms_pressure_fl tpms_pressure_fr tpms_pressure_rl tpms_pressure_rr').split()
CHARGE_FIELDS = (
    'id vehicle_id start_date_ms end_date_ms duration_min charge_energy_added charge_energy_used_kwh start_battery_level end_battery_level '
    'start_ideal_range_km end_ideal_range_km start_rated_range_km end_rated_range_km start_latitude start_longitude address location_name '
    'geofence fast_charger_type is_dc charge_rate_km_per_hour max_charger_power_kw outside_temp_avg cost billing_type cost_per_unit session_fee').split()
SAMPLE_FIELDS = (
    'id charge_id timestamp_ms battery_level usable_battery_level charge_energy_added_kwh charger_power_kw charger_voltage '
    'charger_actual_current charger_pilot_current charger_phases ideal_range_km rated_range_km outside_temp_c battery_heater_on '
    'fast_charger_present fast_charger_brand fast_charger_type charge_cable').split()
NON_NULL = {
    'drive': {'id', 'vehicle_id', 'start_date_ms', 'end_date_ms'},
    'position': {'id', 'drive_id', 'date_ms'},
    'charge': {'id', 'vehicle_id', 'start_date_ms', 'end_date_ms'},
    'charge_sample': {'id', 'charge_id', 'timestamp_ms'}}


def types(**groups):
    return {name: token for token, names in groups.items() for name in names.split()}


# The JSON type of every field, written out from the Hub projection models (ProjectionDrive, ProjectionPosition, ProjectionCharge and
# ProjectionChargeSample: i64 is integer, f64 is number, bool, String), so a retyped generator table cannot agree with itself. position.power
# and the four charger fields of a sample are number although TeslaMate stores whole numbers: the page routes serve the f64 projections.
FIELD_TYPES = {
    'drive': types(
        uuid='vehicle_id', integer='id start_date_ms end_date_ms duration_min speed_max start_soc end_soc ascent descent',
        number='distance_km efficiency outside_temp_avg inside_temp_avg power_max power_min start_ideal_range_km end_ideal_range_km '
               'start_latitude start_longitude end_latitude end_longitude start_rated_range_km end_rated_range_km',
        string='start_address end_address start_geofence end_geofence'),
    'position': types(
        integer='id drive_id date_ms speed battery_level usable_battery_level elevation fan_status',
        number='latitude longitude power odometer ideal_battery_range_km est_battery_range_km rated_battery_range_km inside_temp outside_temp '
               'driver_temp_setting passenger_temp_setting tpms_pressure_fl tpms_pressure_fr tpms_pressure_rl tpms_pressure_rr',
        boolean='is_climate_on battery_heater battery_heater_on'),
    'charge': types(
        uuid='vehicle_id', integer='id start_date_ms end_date_ms duration_min start_battery_level end_battery_level',
        number='charge_energy_added charge_energy_used_kwh start_ideal_range_km end_ideal_range_km start_rated_range_km end_rated_range_km '
               'start_latitude start_longitude charge_rate_km_per_hour max_charger_power_kw outside_temp_avg cost cost_per_unit session_fee',
        string='address location_name geofence fast_charger_type billing_type', boolean='is_dc'),
    'charge_sample': types(
        integer='id charge_id timestamp_ms battery_level usable_battery_level charger_phases',
        number='charge_energy_added_kwh charger_power_kw charger_voltage charger_actual_current charger_pilot_current ideal_range_km rated_range_km outside_temp_c',
        boolean='battery_heater_on fast_charger_present', string='fast_charger_brand fast_charger_type charge_cable')}
JSON_TYPES = {
    'integer': {'type': 'integer', 'format': 'int64', 'minimum': I64_MIN, 'maximum': I64_MAX}, 'number': {'type': 'number'}, 'boolean': {'type': 'boolean'},
    'string': {'type': 'string', 'maxLength': 65536},
    'uuid': {'type': 'string', 'format': 'uuid', 'pattern': r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'}}
CAPABILITIES = ['query.vehicles', 'query.current', 'query.drives', 'query.charges', 'query.charge.samples', 'query.positions',
                'query.enrichment', 'sync.packs', 'auth.revoke']
ROUTE_TABLE = {
    # kind: (method, path, bearer, statuses)
    'discovery': ('GET', '/.well-known/teslatlas-hub', False, [200, 503]),
    'health': ('GET', '/healthz', False, [200, 503]),
    'ready': ('GET', '/readyz', False, [200, 503]),
    'vehicles': ('GET', '/v1/vehicles', True, [200, 401, 503]),
    'current': ('GET', '/v1/vehicles/{vehicle_id}/current', True, [200, 401, 404, 503]),
    'drives': ('GET', '/v1/vehicles/{vehicle_id}/drives', True, [200, 304, 400, 401, 404, 503]),
    'claim': ('POST', '/v1/pairings/{pairing_id}/claim', False, [200, 400, 401, 404, 413, 415, 422, 503]),
    'rotate': ('POST', '/v1/device/rotate', True, [200, 401, 404, 503]),
    'drive': ('GET', '/v1/vehicles/{vehicle_id}/drives/{drive_id}', True, [200, 401, 404, 503]),
    'drive_positions': ('GET', '/v1/vehicles/{vehicle_id}/drives/{drive_id}/positions', True, [200, 304, 400, 401, 404, 503]),
    'charges': ('GET', '/v1/vehicles/{vehicle_id}/charges', True, [200, 304, 400, 401, 404, 503]),
    'charge': ('GET', '/v1/vehicles/{vehicle_id}/charges/{charge_id}', True, [200, 401, 404, 503]),
    'charge_samples': ('GET', '/v1/vehicles/{vehicle_id}/charges/{charge_id}/samples', True, [200, 304, 400, 401, 404, 503]),
    'revoke': ('POST', '/v1/device/revoke', True, [204, 400, 401, 404, 503])}
PAGE_LIMITS = {'drives': (100, 1, 500), 'drive_positions': (1000, 1, 2048), 'charges': (100, 1, 500), 'charge_samples': (1000, 1, 2048)}
PAGE_ORDER = {'drives': (['start_date_ms', 'id'], 'descending'), 'drive_positions': (['date_ms', 'id'], 'ascending'),
              'charges': (['start_date_ms', 'id'], 'descending'), 'charge_samples': (['timestamp_ms', 'id'], 'ascending')}
# The 503 shape of every route (the handler time budget makes an empty 503 possible on all of them), written out literally.
SHAPES_503 = {
    'discovery': 'empty_or_error', 'health': 'empty', 'ready': 'empty_or_ready', 'vehicles': 'empty', 'current': 'empty', 'drives': 'empty_or_error',
    'claim': 'empty', 'rotate': 'empty', 'drive': 'empty_or_error', 'drive_positions': 'empty_or_error', 'charges': 'empty_or_error',
    'charge': 'empty_or_error', 'charge_samples': 'empty_or_error', 'revoke': 'empty'}
CASE_IDS = [
    'discovery-success', 'health-success', 'ready-success', 'vehicles-success', 'current-success', 'drives-success', 'claim-success',
    'rotate-success', 'invalid_limit', 'invalid_time_range', 'invalid_query', 'invalid_cursor', 'drive-conditional', 'pairing-replay',
    'missing-bearer', 'bad-bearer', 'rotated-bearer-grace-valid', 'rotation-lost-response-retry', 'cursor-time-binding', 'later-current',
    'scenario-page-2', 'scenario-page-3', 'current-empty', 'current-unknown', 'rotated-bearer-valid',
    'drive-success', 'drive-unknown', 'positions-success', 'positions-empty', 'positions-keyset-three-pages', 'charges-success',
    'charges-empty', 'charge-success', 'charge-unknown', 'malformed-identifier-404', 'charge-samples-success', 'page-conditional', 'cursor-kind-binding',
    'cursor-vehicle-binding', 'cursor-scope-binding', 'cursor-window-binding', 'page-byte-cap', 'ungranted-vehicle-404',
    'ungranted-vehicles-list', 'ungranted-pack-404', 'revoke-then-401', 'revoke-again-401', 'rotation-grace-same-successor',
    'rotation-after-grace-401', 'discovery-profile']
# Digest line of the unchanged 1.1.0 tree; pinned while that tree exists so a regenerated 1.1.0 cannot pass.
HUB_HTTP_110_DIGEST = '47278485e4962aaa3ae177a3fc916281191b0d066edd1b40aad73fdad20b0f85'
SENTENCE = 'A retry of rotate with the prior bearer during the grace window returns the same successor; it never issues a different token.'


def load(name):
    return json.loads((PROFILE / name).read_text(encoding='utf-8'))


def example(name):
    return load('examples/' + name + '.json')


def run_script(*arguments, **options):
    return subprocess.run([sys.executable, *map(str, arguments)], capture_output=True, **options)


def nullable_schema(schema):
    return any(branch == {'type': 'null'} for branch in schema.get('anyOf', []))


def reseal(root):
    """Rewrite SHA256SUMS of a scratch copy so a deliberate edit reaches the check under test."""
    root = Path(root)
    lines = []
    for path in sorted(item for item in root.rglob('*') if item.is_file() and item.name != 'SHA256SUMS'):
        lines.append(hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + path.relative_to(root).as_posix() + '\n')
    (root / 'SHA256SUMS').write_text(''.join(lines))


class Scratch(unittest.TestCase):
    def scratch_profile(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name) / '1.2.0'
        shutil.copytree(PROFILE, root)
        return root

    def edit(self, root, name, change):
        document = json.loads((root / name).read_text())
        change(document)
        (root / name).write_text(json.dumps(document, indent=2, sort_keys=True) + '\n')
        reseal(root)


# ---------------------------------------------------------------------------
# Generator and bundle integrity
# ---------------------------------------------------------------------------
class GeneratorTests(Scratch):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location('build_hub_http_profile_12', GENERATOR)
        cls.generator = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.generator)

    def test_generator_check_is_byte_identical_and_prints_the_digest_line(self):
        result = run_script(GENERATOR, '--check')
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        digest = hashlib.sha256((PROFILE / 'SHA256SUMS').read_bytes()).hexdigest()
        self.assertEqual(result.stdout.decode().strip(), PROFILE_ID + ' sha256=' + digest)

    def test_json_text_writes_del_and_the_c1_controls_as_escapes_and_leaves_other_text_raw(self):
        value = {'a': 'x\x00\x1f\x7f\x80\x85\x9f\u00a0\u00e9\u2028\U0001f697'}
        raw = self.generator.json_text(value)
        self.assertEqual(raw, b'{\n  "a": "x\\u0000\\u001f\\u007f\\u0080\\u0085\\u009f\xc2\xa0\xc3\xa9\xe2\x80\xa8\xf0\x9f\x9a\x97"\n}\n')
        self.assertEqual(json.loads(raw), value)  # the escape and the character are the same JSON string
        self.assertEqual(self.generator.json_text({'b': 1, 'a': [1.5]}), json.dumps({'b': 1, 'a': [1.5]}, ensure_ascii=False, sort_keys=True, indent=2).encode() + b'\n')

    @unittest.skipUnless((ROOT / 'tools/build_hub_http_profile.py').is_file(), 'the 1.1.0 generator is retired')
    def test_the_1_1_0_generator_is_still_byte_identical(self):
        result = run_script(ROOT / 'tools/build_hub_http_profile.py', '--check')
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(result.stdout.decode().strip(), 'hub-http-v1@1.1.0 sha256=' + HUB_HTTP_110_DIGEST)

    def test_generator_is_self_contained_standard_library_only(self):
        import ast
        imported = set()
        for node in ast.walk(ast.parse(GENERATOR.read_text())):
            if isinstance(node, ast.Import):
                imported |= {alias.name.split('.')[0] for alias in node.names}
            elif isinstance(node, ast.ImportFrom):
                imported.add((node.module or '').split('.')[0])
        self.assertTrue(imported, 'no imports found')
        self.assertLessEqual(imported, set(sys.stdlib_module_names))

    def run_main(self, root, *arguments):
        output = io.StringIO()
        with mock.patch.object(self.generator, 'ROOT', root), mock.patch.object(sys, 'argv', ['build', *arguments]), contextlib.redirect_stdout(output):
            self.generator.main()
        return output.getvalue()

    def test_check_detects_stale_missing_and_unexpected_files(self):
        root = self.scratch_profile()
        self.assertIn('sha256=', self.run_main(root, '--check'))
        (root / 'cases.json').write_bytes((root / 'cases.json').read_bytes() + b' ')
        with self.assertRaisesRegex(SystemExit, 'cases.json'):
            self.run_main(root, '--check')
        shutil.rmtree(root)
        shutil.copytree(PROFILE, root)
        (root / 'examples/health.json').unlink()
        with self.assertRaisesRegex(SystemExit, 'examples/health.json'):
            self.run_main(root, '--check')
        shutil.rmtree(root)
        shutil.copytree(PROFILE, root)
        (root / 'stray.json').write_text('{}')
        with self.assertRaisesRegex(SystemExit, 'unexpected stray.json'):
            self.run_main(root, '--check')

    def test_writing_regenerates_a_missing_tree_to_the_same_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            target = Path(temp) / 'out'
            printed = self.run_main(target)
            self.assertEqual(printed.strip(), PROFILE_ID + ' sha256=' + hashlib.sha256((PROFILE / 'SHA256SUMS').read_bytes()).hexdigest())
            for path in PROFILE.rglob('*'):
                if path.is_file():
                    self.assertEqual((target / path.relative_to(PROFILE)).read_bytes(), path.read_bytes(), path.name)
            self.assertEqual(sorted(p.relative_to(target).as_posix() for p in target.rglob('*') if p.is_file()),
                             sorted(p.relative_to(PROFILE).as_posix() for p in PROFILE.rglob('*') if p.is_file()))

    def test_tools_check_runs_the_generator_check(self):
        lines = (ROOT / 'tools/check').read_text().splitlines()
        self.assertIn('uv run python tools/build_hub_http_profile_12.py --check', lines)


class BundleClosureTests(Scratch):
    def test_sha256sums_lists_exactly_the_files_on_disk(self):
        listed = {}
        for line in (PROFILE / 'SHA256SUMS').read_text().splitlines():
            digest, name = line.split('  ', 1)
            listed[name] = digest
        on_disk = {path.relative_to(PROFILE).as_posix() for path in PROFILE.rglob('*') if path.is_file() and path.name != 'SHA256SUMS'}
        self.assertEqual(set(listed), on_disk)
        for name, digest in listed.items():
            self.assertEqual(hashlib.sha256((PROFILE / name).read_bytes()).hexdigest(), digest, name)
        self.assertEqual(len(on_disk), 12 + 19)  # twelve top-level files plus nineteen examples

    def test_no_generated_file_holds_a_raw_control_character_and_the_tree_is_utf_8(self):
        # json.dumps escapes U+0000 to U+001F but leaves DEL and the C1 controls raw; the generator writes them as escapes, so none sits invisible in a file (R5-text-N3)
        for path in sorted(item for item in PROFILE.rglob('*') if item.is_file()):
            text = path.read_bytes().decode('utf-8')
            raw = sorted({hex(ord(char)) for char in text if unicodedata.category(char) == 'Cc' and char != '\n'})
            self.assertEqual(raw, [], path.relative_to(PROFILE).as_posix())

    def test_required_files_are_present(self):
        for name in ('profile.json', 'openapi.json', 'discovery.schema.json', 'errors.schema.json', 'auth.schema.json', 'resources.schema.json',
                     'cursor.schema.json', 'rotation-grace.schema.json', 'sync.schema.json', 'field-semantics.json', 'cases.json',
                     'sync-regression.json', 'SHA256SUMS'):
            self.assertTrue((PROFILE / name).is_file(), name)
        self.assertEqual(sorted(path.stem for path in (PROFILE / 'examples').glob('*.json')), sorted(load('profile.json')['examples']))

    def test_every_schema_id_is_the_1_2_0_urn(self):
        for path in sorted(PROFILE.glob('*.schema.json')):
            document = json.loads(path.read_text())
            self.assertEqual(document['$id'], 'urn:teslatlas:hub-http-v1:1.2.0:' + path.name.removesuffix('.schema.json'))
            self.assertEqual(document['$schema'], 'https://json-schema.org/draft/2020-12/schema')

    def test_verify_bundle_accepts_the_profile_and_binds_the_digest(self):
        info = conf.verify_bundle(PROFILE)
        self.assertEqual(info['profile_id'], PROFILE_ID)
        self.assertEqual(info['profile_sha256'], hashlib.sha256((PROFILE / 'SHA256SUMS').read_bytes()).hexdigest())

    def test_verify_bundle_rejects_every_kind_of_drift(self):
        cases = {
            'extra file': lambda root: (root / 'extra.json').write_text('{}'),
            'extra example': lambda root: (root / 'examples/extra.json').write_text('{}'),
            'missing file': lambda root: (root / 'examples/health.json').unlink(),
            'altered file': lambda root: (root / 'cases.json').write_bytes((root / 'cases.json').read_bytes() + b'\n'),
            'symbolic link': lambda root: (root / 'link.json').symlink_to(root / 'profile.json'),
            'nested manifest': lambda root: (root / 'examples/SHA256SUMS').write_text(''),
            'traversal entry': lambda root: (root / 'SHA256SUMS').write_text((root / 'SHA256SUMS').read_text() + '0' * 64 + '  ../outside.json\n'),
            'duplicate entry': lambda root: (root / 'SHA256SUMS').write_text((root / 'SHA256SUMS').read_text() + (root / 'SHA256SUMS').read_text().splitlines()[0] + '\n'),
            'missing final newline': lambda root: (root / 'SHA256SUMS').write_text((root / 'SHA256SUMS').read_text().rstrip('\n')),
            'upper-case digest': lambda root: (root / 'SHA256SUMS').write_text((root / 'SHA256SUMS').read_text().upper().replace('  ', '  ', 1)),
        }
        for label, mutate in cases.items():
            with self.subTest(label):
                root = self.scratch_profile()
                mutate(root)
                with self.assertRaises(conf.ContractError):
                    conf.verify_bundle(root)

    def test_verify_bundle_rejects_a_profile_for_another_identity(self):
        root = self.scratch_profile()
        self.edit(root, 'profile.json', lambda document: document.update(profile_id='hub-http-v1@1.1.0'))
        with self.assertRaises(conf.ContractError):
            conf.verify_bundle(root)

    def test_strict_json_rejects_duplicates_nan_non_utf8_bom_and_trailing_data(self):
        for raw in (b'{"a":1,"a":2}', b'{"a":NaN}', b'{"a":Infinity}', b'{"a":1e999}', b'\xff\xfe{}', b'\xef\xbb\xbf{}', b'{} {}', b'{', b''):
            with self.subTest(raw=raw):
                with self.assertRaises(conf.ContractError):
                    conf.strict_json(raw)
        self.assertEqual(conf.strict_json(b'{"a":[1,2.5,"x"]}'), {'a': [1, 2.5, 'x']})
        self.assertEqual(conf.strict_json(b'{"a":5.0}'), {'a': 5})


# ---------------------------------------------------------------------------
# Profile content, literally from the specification
# ---------------------------------------------------------------------------
class ProfileContentTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile = load('profile.json')
        cls.routes = {route['kind']: route for route in cls.profile['routes']}

    def test_identity_fields(self):
        profile = self.profile
        self.assertEqual(profile['profile_id'], PROFILE_ID)
        self.assertEqual(profile['status'], 'served')
        self.assertEqual(profile['contract_version'], '1.2.0')
        self.assertEqual(profile['previous_profile'], 'hub-http-v1@1.1.0')
        self.assertNotIn('previous_profile_sha256', profile)
        self.assertEqual(profile['license'], 'Apache-2.0')
        self.assertEqual(profile['schema_dialect'], 'https://json-schema.org/draft/2020-12/schema')
        self.assertEqual(profile['max_response_bytes'], 1048576)
        self.assertEqual(profile['max_claim_request_bytes'], 4096)

    def test_additive_policy_is_exact(self):
        self.assertEqual(
            self.profile['additive_policy'],
            "Clients ignore unknown object fields and unknown capability tokens. Servers may add optional fields and tokens within 1.x. "
            "Removing or retyping a field, changing an enum value, or changing a route's status set is a new major profile.")

    def test_capabilities_and_advertisement_rules(self):
        profile = self.profile
        self.assertEqual(profile['capabilities'], CAPABILITIES)
        self.assertEqual(profile['capability_grammar'], {'pattern': '^[a-z0-9.]{1,64}$', 'max_items': 16, 'unique': True})
        rules = profile['capability_advertisement']
        self.assertEqual(rules['always'], ['query.vehicles', 'query.current'])
        self.assertEqual(sorted(rules['with_cursor_key']), sorted(['query.drives', 'query.charges', 'query.positions', 'query.charge.samples', 'sync.packs']))
        self.assertEqual(rules['with_paired_device_authentication'], ['auth.revoke'])
        self.assertEqual(rules['with_enrichment'], ['query.enrichment'])
        self.assertEqual(sorted(rules['always'] + rules['with_cursor_key'] + rules['with_paired_device_authentication'] + rules['with_enrichment']),
                         sorted(CAPABILITIES))
        self.assertIn('attributions', rules)

    def test_integers_are_as_1_1_0(self):
        self.assertEqual(self.profile['integers'], {
            'wire': 'signed 64-bit JSON integers',
            'javascript': 'decode losslessly or reject values outside [-9007199254740991,9007199254740991] before normalization; never silently round IDs'})

    def test_routes_match_the_specification(self):
        self.assertEqual(sorted(self.routes), sorted(ROUTE_TABLE))
        for kind, (method, path, bearer, statuses) in ROUTE_TABLE.items():
            route = self.routes[kind]
            self.assertEqual((route['method'], route['path'], route['bearer'], route['statuses']), (method, path, bearer, statuses), kind)
            self.assertEqual(sorted(int(status) for status in route['responses']), statuses, kind)
        for kind in ('drives', 'drive_positions', 'charges', 'charge_samples'):
            route = self.routes[kind]
            self.assertTrue(route['etag_required'], kind)
            limit = PAGE_LIMITS[kind]
            self.assertEqual((route['limit']['default'], route['limit']['min'], route['limit']['max']), limit, kind)
            self.assertEqual((route['order']['keys'], route['order']['direction']), PAGE_ORDER[kind], kind)
        for kind in set(self.routes) - {'drives', 'drive_positions', 'charges', 'charge_samples'}:
            self.assertFalse(self.routes[kind]['etag_required'], kind)

    def test_route_error_codes_and_not_found_forms(self):
        self.assertEqual(self.routes['drive']['error_codes']['404'], ['vehicle_not_found', 'drive_not_found'])
        self.assertEqual(self.routes['charge']['error_codes']['404'], ['vehicle_not_found', 'charge_not_found'])
        self.assertEqual(self.routes['drive_positions']['error_codes']['404'], ['vehicle_not_found', 'drive_not_found'])
        self.assertEqual(self.routes['charge_samples']['error_codes']['404'], ['vehicle_not_found', 'charge_not_found'])
        self.assertEqual(self.routes['drives']['error_codes']['404'], ['vehicle_not_found'])
        for kind in ('drives', 'drive_positions', 'charges', 'charge_samples'):
            self.assertEqual(self.routes[kind]['error_codes']['400'], ['invalid_query', 'invalid_time_range', 'invalid_limit', 'invalid_cursor'])
            self.assertEqual(self.routes[kind]['error_503_forms'], ['empty authentication-store failure', 'JSON service_unavailable query failure',
                                                                      'empty handler time-budget expiry'])
        self.assertEqual(self.routes['current']['responses']['404'], 'empty')
        self.assertEqual(self.routes['vehicles']['responses']['503'], 'empty')

    def test_revoke_route_takes_no_body(self):
        route = self.routes['revoke']
        self.assertEqual(route['capability'], 'auth.revoke')
        self.assertEqual(route['responses']['204'], 'empty')
        self.assertIn('any non-empty request body, including one over 4096 bytes, is 400', route['request_body'])
        self.assertIn('A request with a non-empty body, including one over 4096 bytes, is 400', self.profile['pairing']['revocation'])
        self.assertEqual(route['responses']['404'], 'empty')
        self.assertIn('auth.revoke is not advertised', route['availability'])
        self.assertIn('auth.revoke is not advertised', self.profile['pairing']['revocation'])
        operation = load('openapi.json')['paths']['/v1/device/revoke']['post']
        self.assertNotIn('requestBody', operation)
        self.assertIn('auth.revoke is not advertised', operation['responses']['404']['description'])

    def test_revoke_never_answers_413_because_no_layer_of_the_hub_rejects_a_body_it_does_not_read(self):
        # The 4096 byte ordinary limit only bites an extractor that reads the body, so the revoke rule is a requirement on the Hub:
        # every non-empty body, however large, is 400, and 413 is outside the status set of the route.
        route = self.routes['revoke']
        self.assertNotIn(413, route['statuses'])
        self.assertEqual(route['statuses'], [204, 400, 401, 404, 503])
        self.assertIn('413', route['request_body_limit'])
        self.assertIn('extractor', route['request_body_limit'])
        self.assertIn('it is never 413', self.profile['pairing']['revocation'])
        operation = load('openapi.json')['paths']['/v1/device/revoke']['post']
        self.assertNotIn('413', operation['responses'])
        self.assertIn('over 4096 bytes', operation['description'])
        self.assertIn('never 413', operation['description'])

    def test_the_claim_route_lists_413_as_extractor_text_and_says_how_the_limit_is_met(self):
        route = self.routes['claim']
        self.assertEqual(route['statuses'], [200, 400, 401, 404, 413, 415, 422, 503])
        self.assertEqual(route['responses']['413'], 'text')
        # the 413 belongs to a JSON body: the extractor checks the Content-Type first (application/json or any application/*+json), so another type is 415 whatever the size
        self.assertIn('A request body of a JSON media type (application/json or application/*+json) over 4096 bytes is answered 413', route['request_limit'])
        self.assertIn('before the claim is attempted', route['request_limit'])
        self.assertIn('Content-Type is missing or is not a JSON media type (application/json or application/*+json) is 415 whatever its size', route['request_limit'])
        self.assertNotIn('is not application/json', route['request_limit'])
        # a claim is never retried automatically, and a refusal before the claim was attempted leaves the invitation usable
        for phrase in ('Never retry a claim automatically after an empty 503, a timeout or a lost response', 'single use', 'may already have consumed it',
                       'A 400, 413, 415 or 422 answer means the claim was not attempted'):
            self.assertIn(phrase, route['retry'])
        self.assertEqual(self.profile['max_claim_request_bytes'], 4096)
        operation = load('openapi.json')['paths']['/v1/pairings/{pairing_id}/claim']['post']
        self.assertEqual(operation['requestBody']['x-max-request-bytes'], 4096)
        response = operation['responses']['413']
        self.assertEqual(response['content'], {'text/plain': {'schema': {'type': 'string', 'maxLength': 65536}}})
        self.assertIn('4096 byte limit', response['description'])
        self.assertIn('claim is not attempted', response['description'])
        self.assertIn('of a JSON media type (application/json or application/*+json)', response['description'])
        self.assertIn('Content-Type is missing or is not a JSON media type (application/json or application/*+json), whatever the size of the body',
                      operation['responses']['415']['description'])
        self.assertNotIn('is not application/json', operation['responses']['415']['description'])
        for phrase in ('device_name', 'empty, over 128 UTF-8 bytes or with a control character', 'the invitation is not consumed', 'bounded text/plain'):
            self.assertIn(phrase, operation['responses']['422']['description'])
        for phrase in ('Never retry a claim automatically', 'single use', 'handler time budget'):
            self.assertIn(phrase, operation['responses']['503']['description'])
        profile = conf.Profile(PROFILE)
        text = {'content-type': 'text/plain; charset=utf-8'}
        self.assertEqual(conf.validate_raw(profile, 'claim', 413, text, b'Failed to buffer the request body: length limit exceeded'), [])
        self.assertEqual(conf.validate_raw(profile, 'claim', 413, {'content-type': 'application/json'}, b'{}'), ['expected extractor text error'])
        self.assertEqual(conf.validate_raw(profile, 'claim', 413, text, b''), ['expected extractor text error'])
        for kind in set(self.routes) - {'claim'}:  # only the claim route reads a body through an extractor
            self.assertNotIn(413, self.routes[kind]['statuses'], kind)

    def test_the_device_name_rule_is_stated_and_a_refusal_is_422_text_that_consumes_nothing(self):
        # The Hub's store refuses an empty name, one over 128 bytes and one with a control character, and the claim handler maps that to an empty 503 that
        # no retry can fix. The profile makes it a 422 requirement on the later Hub batch, so a client can tell the owner what to correct.
        name = self.profile['pairing']['device_name']
        self.assertEqual((name['max_bytes'], name['consumes_invitation'], name['refused']['status'], name['refused']['body']), (128, False, 422, 'bounded text/plain'))
        for phrase in ('1 to 128 UTF-8 bytes', 'has no control character', 'U+0000 to U+001F, U+007F to U+009F', 'a character count cannot state the byte limit',
                       'can still be over 128 bytes, and is refused all the same'):
            self.assertIn(phrase, name['rule'])
        for phrase in ('answered 422 with a bounded text/plain body', 'before the invitation is consulted', 'consumes nothing',
                       'does not reveal whether the invitation exists', 'the same invitation still claims with a valid name',
                       # the name never causes a 503; it is not an absolute: the handler time budget can still end any request, the claim included (R5-text-N1)
                       'The name never causes a 503 (an empty 503 for another reason, such as the handler time budget, can still happen, '
                       'and the claim\'s never-retry rule then applies).'):
            self.assertIn(phrase, name['refused']['rule'])
        self.assertNotIn('never an empty 503', name['refused']['rule'])  # that absolute contradicted limits.handler_timeout_response
        self.assertIn('every route', self.profile['limits']['handler_timeout_response']['applies_to'])
        self.assertEqual(name['schema'], 'auth.schema.json#/$defs/claim_request')
        self.assertEqual(self.profile['pairing']['claim_fields'], ['secret', 'device_name'])
        route = self.routes['claim']
        self.assertEqual(route['responses']['422'], 'text')  # 422 was already in the status set, so the refusal changes none
        self.assertIn('pairing.device_name', route['request_fields']['device_name'])
        self.assertIn('consumes nothing', route['request_fields']['device_name'])

    def test_path_identifiers_are_signed_64_bit_and_probe_to_the_not_found_json(self):
        identifiers = self.profile['path_identifiers']
        self.assertIn('signed 64-bit integer >= 1', identifiers['drive_id'])
        self.assertIn('signed 64-bit integer >= 1', identifiers['charge_id'])
        self.assertIn('404', identifiers['invalid'])
        self.assertIn('never 400', identifiers['invalid'])
        document = load('openapi.json')
        # a segment that percent-decodes to invalid UTF-8 (the framework of the Hub answers 400 text/plain before the handler, so before the bearer check)
        undecodable = identifiers['undecodable']
        for phrase in ('%FF', 'not valid UTF-8', 'after the bearer check', 'never 400 and never a framework text/plain body',
                       'a request without a valid bearer is 401 whatever its path segments'):
            self.assertIn(phrase, undecodable['rule'])
        self.assertTrue(undecodable['after_bearer_check'])
        self.assertEqual(undecodable['answers'], {
            'current': 'empty 404', 'json_routes': '404 JSON vehicle_not_found for a malformed vehicle_id, drive_not_found or charge_not_found for a malformed drive_id or charge_id',
            'sync_manifest': 'empty 404', 'sync_noop': 'empty 404', 'pack': 'empty 404',
            'other_sync_routes': 'changes-since, signing-keys and prepared-artefacts answer as access.sync_routes.routes[].malformed_vehicle_id says'})
        self.assertIn('is 401 whatever its vehicle_id, drive_id, charge_id or object_name', self.profile['unauthorized']['path_segments'])
        self.assertIn('invalid UTF-8', self.profile['unauthorized']['path_segments'])
        # the nil UUID is well formed and no vehicle can have it: an unknown vehicle, never 503
        nil = identifiers['nil_vehicle_id']
        self.assertEqual(nil['value'], '00000000-0000-0000-0000-000000000000')
        self.assertIn('unknown vehicle', nil['rule'])
        # not an absolute: a 503 for another reason (the handler time budget, the authentication store) can still happen to any request
        self.assertIn('its value never causes a 503', nil['rule'])
        self.assertIn('handler time budget', nil['rule'])
        self.assertNotIn('never 503.', nil['rule'])
        self.assertEqual(nil['answers'], {'current': 'empty 404', 'json_routes': '404 JSON vehicle_not_found', 'sync_manifest': '404 JSON vehicle_not_found',
                                          'sync_noop': '404 JSON vehicle_not_found'})
        for path in ('/v1/vehicles/{vehicle_id}/current', '/v1/vehicles/{vehicle_id}/drives/{drive_id}'):
            vehicle = next(item for item in document['paths'][path]['get']['parameters'] if item['name'] == 'vehicle_id')
            for phrase in ('percent-decodes to invalid UTF-8', 'the nil UUID', 'unknown vehicle', 'never 400', 'Any value that does not parse as a UUID',
                           'lowercase hyphenated form', 'the only form this profile specifies', 'upper case, no hyphens, braces or urn:uuid:', 'must not rely on'):
                self.assertIn(phrase, vehicle['description'])
            self.assertNotIn('Any other value', vehicle['description'])  # the pattern admits only the lowercase hyphenated form; the Hub's parser admits more
        description = next(item for item in document['paths']['/v1/vehicles/{vehicle_id}/drives/{drive_id}']['get']['parameters'] if item['name'] == 'drive_id')['description']
        for phrase in ('percent-decodes to invalid UTF-8', 'in plain decimal form', 'the only form this profile specifies', 'a plus sign or leading zeros',
                       'Any value that does not parse as a signed 64-bit decimal integer >= 1 (non-numeric, zero, negative or out of range)', 'never 400'):
            self.assertIn(phrase, description)
        self.assertNotIn('Any other value', description)
        self.assertNotIn('Any value that does not parse as a decimal integer >= 1', description)  # 9223372036854775808 is a decimal integer >= 1: out of range must be named
        for parameter in ('drive_id', 'charge_id'):  # every OpenAPI path that takes one says it
            carrying = [(path, found) for path, item in document['paths'].items() for operation in item.values() for found in operation.get('parameters', [])
                        if found['name'] == parameter]
            self.assertTrue(carrying, parameter)
            for path, found in carrying:
                self.assertIn('(non-numeric, zero, negative or out of range)', found['description'], path)
        # the cases probe a decimal integer >= 1 that is out of range, so the parameter text has to cover it (R5-text-N2)
        self.assertEqual(conf.UNKNOWN_IDENTIFIERS, ('abc', '0', '-1', '9223372036854775808', '%FF'))
        self.assertGreater(int('9223372036854775808'), I64_MAX)
        # only the specified spellings are promised: the Hub's parsers accept more, and a client must not rely on the others being refused
        for phrase in ('lowercase hyphenated form of a vehicle_id', 'plain decimal form of a drive_id or charge_id', 'upper case, without hyphens, in braces or as urn:uuid:',
                       'an integer with a plus sign or leading zeros', 'does not rely on the others being refused'):
            self.assertIn(phrase, identifiers['spellings'])
        for path, name in (('/v1/vehicles/{vehicle_id}/drives/{drive_id}', 'drive_id'), ('/v1/vehicles/{vehicle_id}/charges/{charge_id}/samples', 'charge_id')):
            parameter = next(item for item in document['paths'][path]['get']['parameters'] if item['name'] == name)
            self.assertEqual(parameter['schema'], {'type': 'integer', 'format': 'int64', 'minimum': 1, 'maximum': I64_MAX})

    def test_pages_rules(self):
        pages = self.profile['pages']
        self.assertEqual(pages['routes'], ['drives', 'drive_positions', 'charges', 'charge_samples'])
        self.assertEqual(pages['max_bytes'], 1048576)
        self.assertEqual(pages['unknown_query_parameters'], {'status': 400, 'error_code': 'invalid_query'})
        self.assertEqual(pages['errors'], ['invalid_query', 'invalid_time_range', 'invalid_limit', 'invalid_cursor'])
        self.assertEqual((pages['cache_control'], pages['content_type']), ('no-store', 'application/json'))
        self.assertIn('inclusive', pages['query']['from_ms'])
        self.assertIn('default 0', pages['query']['from_ms'])
        self.assertIn('exclusive', pages['query']['to_ms'])
        self.assertIn('9223372036854775807', pages['query']['to_ms'])
        self.assertIn('503 service_unavailable', pages['item_split'])
        self.assertIn('never split inside an item', pages['item_split'])
        self.assertIn('SHA-256', pages['etag'])
        self.assertIn('304', pages['conditional'])
        self.assertNotIn('check_order', pages)  # not part of the specification

    def test_a_malformed_query_value_is_invalid_query_and_the_other_codes_are_for_well_formed_values(self):
        malformed = self.profile['pages']['malformed_query_values']
        self.assertEqual((malformed['status'], malformed['error_code']), (400, 'invalid_query'))
        rule = malformed['rule']
        for phrase in ('does not parse as a decimal integer', 'limit 0 to 4294967295', 'from_ms and to_ms signed 64-bit', 'limit=abc', 'limit=-1',
                       'from_ms=1.5', 'to_ms=9223372036854775808', 'invalid_limit and invalid_time_range apply only to well-formed values'):
            self.assertIn(phrase, rule)
        # the cases hold a Hub to every value the text names
        self.assertEqual(conf.MALFORMED_VALUES, (('limit', 'abc'), ('limit', '-1'), ('from_ms', '1.5'), ('to_ms', '9223372036854775808')))
        description = {case['id']: case for case in load('cases.json')['cases']}['invalid_query']['description']
        for name, value in conf.MALFORMED_VALUES:
            self.assertIn(name + '=' + value, description)

    def test_access_text_and_keys(self):
        access = self.profile['access']
        self.assertTrue(access['vehicle_scoped_routes_check_grant'])
        self.assertTrue(access['check_before_query_parsing'])
        self.assertTrue(access['unknown_equals_ungranted'])
        self.assertEqual(access['json_routes_not_found'], {'status': 404, 'error_code': 'vehicle_not_found'})
        self.assertEqual(access['empty_404_routes'], ['current', 'pack'])
        for phrase in ('answered exactly as an unknown vehicle on the same route', '404 vehicle_not_found on JSON routes',
                       'empty-body 404 on /current', 'Each sync route keeps the 404 the Hub gives an unknown vehicle today',
                       'lists only granted vehicles', 'indistinguishable from an unknown digest',
                       'hub-sync-v1@1.3.0 lists the JSON vehicle_not_found 404 on changes-since and an empty 404 on signing-keys and prepared-artefacts',
                       'It lists no 404 on manifest and no-op', 'JSON vehicle_not_found for a well-formed unknown vehicle id and an empty 404 for a malformed one',
                       '(the manifest also answers an empty 404 for a known vehicle that has no published full snapshot yet)',
                       'asserted, not inherited, statuses'):
            self.assertIn(phrase, access['text'])
        self.assertNotIn('empty-body 404 on /current, sync routes', access['text'])
        self.assertNotIn('that hub-sync-v1 gives them', access['text'])
        self.assertNotIn('hub-sync-v1 lists', access['text'])  # every statement about what hub-sync-v1 lists names the version it is read from
        self.assertNotIn('defined by hub-sync-v1 and unchanged', json.dumps(access))

    def test_each_sync_route_says_what_hub_sync_v1_lists_and_what_the_hub_answers_for_a_vehicle_it_does_not_serve(self):
        sync = self.profile['access']['sync_routes']
        self.assertIn('asserted by this profile, not inherited', sync['rule'])
        self.assertIn('hub-sync-v1@1.3.0 lists no 404', sync['rule'])
        self.assertNotIn('hub-sync-v1 lists', sync['rule'])
        self.assertIn('hub-sync-v1@1.3.0, its status-tables.json', sync['listed_by'])
        # hub-sync-v1@1.4.0 publishes no status table and lists no 404 on its two operations, as listed_by says
        later = ROOT / 'profiles/hub-sync-v1/1.4.0'
        self.assertFalse((later / 'status-tables.json').exists())
        later_paths = json.loads((later / 'openapi.json').read_text())['paths']
        later_statuses = {path + ' ' + method: sorted(operation['responses']) for path, item in later_paths.items() for method, operation in item.items()}
        self.assertEqual(later_statuses, {'/v1/vehicles/{vehicle_id}/sync/changes-since post': ['200', '409'], '/v1/vehicles/{vehicle_id}/sync/manifest get': ['200', '406']})
        self.assertIn('hub-sync-v1@1.4.0 publishes no status table', sync['listed_by'])
        by_route = {item['route']: item for item in sync['routes']}
        self.assertEqual(list(by_route), ['GET /v1/vehicles/{vehicle_id}/sync/manifest', 'GET /v1/vehicles/{vehicle_id}/sync/noop',
                                          'POST /v1/vehicles/{vehicle_id}/sync/changes-since', 'GET /v1/vehicles/{vehicle_id}/sync/signing-keys',
                                          'GET /v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}'])
        # what hub-sync-v1@1.3.0 lists (its status-tables.json), written out here, and what the Hub answers (server.rs and hub_sync_v1.rs)
        expected = {
            'manifest': ('no 404 (200, 401, 406)', 'empty 404', '404 JSON vehicle_not_found', True),
            'noop': ('no 404 (200, 401, 406)', 'empty 404', '404 JSON vehicle_not_found', True),
            'changes-since': ('404 JSON vehicle_not_found', '404 JSON vehicle_not_found', '404 JSON vehicle_not_found', False),
            'signing-keys': ('empty 404', 'empty 404', 'empty 404', False),
            'prepared-artefacts': ('empty 404', 'empty 404', '404 JSON vehicle_not_found', False)}
        for route, (lists, malformed, unknown, asserted) in expected.items():
            item = next(value for key, value in by_route.items() if '/sync/' + route in key)
            with self.subTest(route):
                self.assertEqual((item['hub_sync_v1_lists'], item['malformed_vehicle_id'], item['unknown_vehicle'], item['asserted_by_this_profile']),
                                 (lists, malformed, unknown, asserted))
        # the manifest also answers an empty 404 for a known vehicle with no published full snapshot (the store selects only mode full_snapshot); no other entry has it (R5-reality-N1)
        self.assertEqual(by_route['GET /v1/vehicles/{vehicle_id}/sync/manifest']['no_full_snapshot'], 'empty 404')
        self.assertIn('an empty 404 on the manifest means a malformed vehicle id or that no full snapshot is published yet', by_route['GET /v1/vehicles/{vehicle_id}/sync/manifest']['note'])
        self.assertEqual([key for key, value in by_route.items() if 'no_full_snapshot' in value], ['GET /v1/vehicles/{vehicle_id}/sync/manifest'])
        # only the two routes that hub-sync-v1 gives no 404 are asserted here
        self.assertEqual([key for key, value in by_route.items() if value['asserted_by_this_profile']],
                         ['GET /v1/vehicles/{vehicle_id}/sync/manifest', 'GET /v1/vehicles/{vehicle_id}/sync/noop'])
        self.assertIn('where hub-sync-v1@1.3.0 lists an empty 404', by_route['GET /v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}']['note'])
        status_tables = json.loads((ROOT / 'profiles/hub-sync-v1/1.3.0/status-tables.json').read_text())['tables']
        listed = {table['route']: {item['status']: item['body'] for item in table['responses']} for table in status_tables}
        shapes = {'vehicle_not_found error': '404 JSON vehicle_not_found', 'empty': 'empty 404'}
        for item in sync['routes']:
            statuses = listed[item['route']]  # the profile text is checked against the sync profile's own tables
            if 404 in statuses:
                self.assertEqual(item['hub_sync_v1_lists'], shapes[statuses[404]], item['route'])
            else:
                self.assertEqual(item['hub_sync_v1_lists'], 'no 404 (200, 401, 406)', item['route'])
                self.assertEqual(sorted(statuses), [200, 401, 406], item['route'])

    def test_companion_routes_follow_hub_sync_v1(self):
        companions = {route['kind']: route for route in self.profile['companion_routes']}
        self.assertEqual(sorted(companions), ['pack', 'sync_manifest'])
        pack, manifest = companions['pack'], companions['sync_manifest']
        # The Hub serves packs by object name, the digest plus .sqlite.zst, exactly as hub-sync-v1 and 1.1.0 name the route.
        self.assertEqual((pack['method'], pack['path'], pack['statuses']), ('GET', '/v1/packs/sha256/{object_name}', [401, 404]))
        self.assertEqual(pack['responses'], {'401': 'empty', '404': 'empty'})
        self.assertIn('.sqlite.zst', pack['object_name'])
        self.assertEqual(manifest['path'], '/v1/vehicles/{vehicle_id}/sync/manifest')
        # hub-sync-v1 lists no 404 for the manifest: this profile asserts it (empty for a malformed id, JSON vehicle_not_found otherwise)
        self.assertEqual(manifest['responses'], {'401': 'empty', '404': 'empty_or_error'})
        self.assertEqual(manifest['error_codes'], {'404': ['vehicle_not_found']})
        self.assertIn('hub-sync-v1@1.3.0 also defines 200 and 406 and lists no 404 for this route', manifest['note'])
        self.assertIn('asserted by this profile, not inherited from hub-sync-v1', manifest['note'])
        # what the Hub also answers on the manifest and the pack (server.rs: the manifest handler answers an empty 400 for a bad header, the pack handler 406, 416 and 503)
        self.assertIn('an empty 400 (an invalid x-teslatlas-sync-capability or x-teslatlas-supported-schemas header) and 503s '
                      '(empty, or the JSON service_unavailable envelope when the vehicle check fails) on it, '
                      'which hub-sync-v1@1.3.0 does not list and this profile does not assert', manifest['note'])
        self.assertNotIn('and empty 503s on it', manifest['note'])  # the vehicle check answers a JSON 503 as well (R5-text-N4, R5-reality-N2)
        # the manifest's empty 404 is not only the answer for a malformed id: a known vehicle with no published full snapshot gets it too (R5-reality-N1)
        self.assertIn('an empty body for a malformed one, or an empty body for a known vehicle that has no published full snapshot yet', manifest['note'])
        self.assertNotIn('the full status set is defined by hub-sync-v1', json.dumps(self.profile))
        # hub-sync-v1@1.3.0 lists 200, 206, 401 and an empty 404 for the pack; the Hub also answers 406 and empty 503s, which neither lists nor this profile asserts
        statuses = {item['status'] for table in json.loads((ROOT / 'profiles/hub-sync-v1/1.3.0/status-tables.json').read_text())['tables']
                    if table['route'] == 'GET /v1/packs/sha256/{object_name}' for item in table['responses']}
        self.assertEqual(statuses, {200, 206, 401, 404})
        for phrase in ('hub-sync-v1@1.3.0 lists 200, 206, 401 and an empty 404 for this route',
                       'the Hub also answers 406 (schema publication unavailable), 416 (an unsatisfiable Range) and empty 503s (no cursor key, no free stream slot, catalogue errors) on it',
                       'which hub-sync-v1@1.3.0 does not list and this profile does not assert'):
            self.assertIn(phrase, pack['note'])
        manifest_statuses = {item['status'] for table in json.loads((ROOT / 'profiles/hub-sync-v1/1.3.0/status-tables.json').read_text())['tables']
                             if table['route'] == 'GET /v1/vehicles/{vehicle_id}/sync/manifest' for item in table['responses']}
        self.assertEqual(manifest_statuses, {200, 401, 406})  # neither 400 nor 503 is listed, as the note says
        self.assertNotIn('defines the full status set', json.dumps(self.profile))
        # the manifest is served whether or not sync.packs is advertised, so no capability gates it; the pack is advertised with sync.packs
        self.assertNotIn('capability', manifest)
        self.assertIn('whether or not sync.packs is advertised', manifest['availability'])
        self.assertEqual(pack['capability'], 'sync.packs')
        for route in companions.values():
            self.assertEqual(route['defined_by'], 'hub-sync-v1')
        regression = load('sync-regression.json')
        self.assertEqual(regression['routes'], ['GET /v1/vehicles/{vehicle_id}/sync/manifest', 'GET /v1/vehicles/{vehicle_id}/sync/noop',
                                                'GET /v1/packs/sha256/{object_name}'])
        self.assertNotIn('{digest}', json.dumps(load('cases.json')) + json.dumps(regression) + json.dumps(self.profile))
        previous = json.loads((ROOT / 'profiles/hub-http-v1/1.1.0/sync-regression.json').read_text())
        self.assertEqual(regression['routes'], previous['routes'])
        self.assertNotIn('empty 404 identical to not-found', regression['access'])
        # the sync-regression appendix says the same as profile.json: asserted for the manifest and no-op, not inherited
        self.assertIn('each keeps the 404 the Hub gives an unknown vehicle today', regression['access'])
        self.assertIn('asserted by hub-http-v1@1.2.0, because hub-sync-v1@1.3.0 lists none for them', regression['access'])
        self.assertNotIn('hub-sync-v1 404 is unchanged', regression['access'])

    def test_pairing_rotation_and_lifetime(self):
        rotation = self.profile['pairing']['rotation']
        self.assertEqual(rotation['old_bearer_grace_ms'], 86400000)
        self.assertEqual(rotation['lost_response_retry'], SENTENCE)
        self.assertIn(SENTENCE, rotation['old_bearer_validity'])
        self.assertIn('rotation_at_ms + 86400000 exclusive', rotation['old_bearer_validity'])
        self.assertEqual(rotation['after_grace'], {'status': 401})
        self.assertIn('device_id, access_token, expires_at_ms', rotation['response_fields'])
        lifetime = self.profile['pairing']['lifetime']
        self.assertEqual(lifetime['sliding_ms'], 30 * 86400000)
        self.assertEqual(lifetime['absolute_maximum_default_days'], 180)
        self.assertEqual(lifetime['at_maximum']['status'], 200)
        self.assertEqual(lifetime['at_maximum']['reason'], 'max_lifetime')
        self.assertIn('unchanged', lifetime['at_maximum']['expires_at_ms'])
        self.assertIn('issues nothing', lifetime['text'])
        self.assertIn('re-pair', lifetime['text'])
        self.assertIn('repeated call returns 401', self.profile['pairing']['revocation'])
        # one grace row per device: a further rotation ends the earlier bearer's grace
        self.assertEqual(rotation['one_grace_per_device'], 'Only the bearer replaced by the latest rotation has a grace window; '
                                                           "a further rotation ends the earlier bearer's grace, and it is then 401.")

    def test_unsupported_limits_and_error_codes(self):
        unsupported = self.profile['unsupported']
        self.assertEqual(sorted(unsupported), ['commands', 'metadata', 'protocol_version_header', 'revisions', 'rich_protocol_versions', 'sse'])
        self.assertEqual(unsupported['rich_protocol_versions'], 'the root teslatlas-v1 family is not served')
        self.assertEqual(unsupported['commands'], 'no public vehicle command route')
        limits = self.profile['limits']
        self.assertEqual((limits['ordinary_request_body_bytes'], limits['changes_since_request_body_bytes']), (4096, 8192))
        self.assertEqual((limits['in_flight_requests'], limits['handler_timeout_seconds'], limits['json_page_bytes']), (32, 15, 1048576))
        self.assertEqual(limits['array_max_items'], {'vehicles': 10000, 'capabilities': 16, 'attributions': 8, 'drives': 500, 'charges': 500,
                                                     'positions': 2048, 'charge_samples': 2048})
        self.assertEqual(self.profile['error_codes'], ['invalid_query', 'invalid_time_range', 'invalid_limit', 'invalid_cursor',
                                                       'vehicle_not_found', 'service_unavailable', 'drive_not_found', 'charge_not_found'])

    def test_the_handler_timeout_answer_is_defined_for_every_route(self):
        # The Hub's outermost layer answers an empty 503 once the 15 second budget, which includes the wait for a request slot, is spent.
        rule = self.profile['limits']['handler_timeout_response']
        self.assertEqual((rule['status'], rule['body']), (503, 'empty'))
        self.assertIn('every route', rule['applies_to'])
        self.assertIn('hub-sync-v1', rule['applies_to'])
        self.assertIn('waiting for one of the 32 in-flight request slots', rule['budget'])
        self.assertIn('15 second', rule['budget'])
        for phrase in ('empty body', 'no content type', 'whatever the route', 'transient outage'):
            self.assertIn(phrase, rule['rule'])
        # except the claim: a claim that ended in an empty 503 may already have consumed the single-use invitation, so it is never retried automatically
        for phrase in ('on any route except the claim as a transient outage', 'A claim is never retried automatically', 'the invitation is single use', 'may already have consumed it'):
            self.assertIn(phrase, rule['rule'])
        self.assertEqual({kind: route['responses']['503'] for kind, route in self.routes.items()}, SHAPES_503)
        for kind in ('discovery', 'drives', 'drive', 'drive_positions', 'charges', 'charge', 'charge_samples'):
            self.assertIn('empty handler time-budget expiry', self.routes[kind]['error_503_forms'], kind)
        self.assertEqual(self.routes['discovery']['error_codes']['503'], ['service_unavailable'])
        document = load('openapi.json')
        self.assertIn('15 second handler time budget', document['info']['description'])
        self.assertIn('32 in-flight request slots', document['info']['description'])
        self.assertIn('except on the claim route, which is never retried automatically', document['info']['description'])
        for kind, shape in SHAPES_503.items():
            route = self.routes[kind]
            response = document['paths'][route['path']][route['method'].lower()]['responses']['503']
            self.assertEqual('content' in response, shape != 'empty', kind)
            self.assertEqual(response.get('x-empty-body-allowed', False), shape != 'empty', kind)
            self.assertIn('time budget', response['description'], kind)
        self.assertIn('not_ready', document['paths']['/readyz']['get']['responses']['503']['description'])
        # the health answer is a plain 200, so the handler time budget is the only cause of the 503 of /healthz and its text names no second cause (R5-reality-N3);
        # every other empty-503 route has one (the authentication or query store) and keeps 'also'
        self.assertEqual(document['paths']['/healthz']['get']['responses']['503']['description'], 'Empty body; only when the handler time budget expires.')
        self.assertNotIn('also', document['paths']['/healthz']['get']['responses']['503']['description'])
        for kind in ('vehicles', 'current', 'claim', 'rotate', 'revoke'):
            route = self.routes[kind]
            self.assertIn('also the answer when the handler time budget expires',
                          document['paths'][route['path']][route['method'].lower()]['responses']['503']['description'], kind)

    def test_a_request_with_an_origin_the_hub_does_not_admit_is_403_on_any_route(self):
        # The browser CORS layer of the Hub (browser_cors.rs: deny = 403, Cache-Control no-store, empty body) runs before every route.
        rule = self.profile['limits']['browser_origin_response']
        self.assertEqual((rule['status'], rule['body'], rule['cache_control']), (403, 'empty', 'no-store'))
        self.assertIn('every route', rule['applies_to'])
        self.assertIn('hub-sync-v1', rule['applies_to'])
        for phrase in ('carries an Origin header', 'does not admit that origin', 'owner\'s allowed origins', 'the route has no browser access',
                       'A client that sends no Origin header', 'never sees any of them',
                       # which routes admit browser origins and the preflight answers are the Hub's, outside this profile
                       'Which routes admit browser origins', 'how the Hub answers a CORS preflight (an OPTIONS request)', 'lie outside this profile'):
            self.assertIn(phrase, rule['rule'])
        description = load('openapi.json')['info']['description']
        self.assertIn('403 with an empty body', description)
        self.assertIn('Origin header', description)
        for phrase in ('Which routes admit browser origins', 'CORS preflight', 'lie outside this profile'):
            self.assertIn(phrase, description)
        # 403 is not in any status set: it is a profile-wide answer outside them, like the handler time budget, and no route lists it
        for kind, route in self.routes.items():
            self.assertNotIn(403, route['statuses'], kind)

    def test_only_closed_sessions_are_served_and_the_routes_say_what_an_open_one_is(self):
        sessions = self.profile['sessions']
        self.assertIn('Closed sessions only', sessions['served'])
        self.assertIn('end_date_ms', sessions['served'])
        self.assertIn('never listed', sessions['in_progress'])
        document = load('openapi.json')
        expected = {'drives': 'is left out', 'drive': '404 drive_not_found', 'drive_positions': '404 drive_not_found', 'charges': 'is left out',
                    'charge': '404 charge_not_found', 'charge_samples': '404 charge_not_found'}
        for kind, phrase in expected.items():
            route = self.routes[kind]
            self.assertIn(phrase, route['in_progress'], kind)
            self.assertEqual(document['paths'][route['path']][route['method'].lower()]['description'], route['in_progress'], kind)
        for kind in set(self.routes) - set(expected):
            self.assertNotIn('in_progress', self.routes[kind], kind)
        # the schemas agree: a session without an end cannot be served
        resources = load('resources.schema.json')['$defs']
        for name in ('drive', 'charge'):
            self.assertIn('end_date_ms', resources[name]['required'])
            self.assertEqual(resources[name]['properties']['end_date_ms']['type'], 'integer')

    def test_the_openapi_descriptions_name_the_cause_of_each_401_and_404(self):
        paths = load('openapi.json')['paths']
        self.assertIn('invitation secret', paths['/v1/pairings/{pairing_id}/claim']['post']['responses']['401']['description'])
        self.assertNotIn('bearer', paths['/v1/pairings/{pairing_id}/claim']['post']['responses']['401']['description'])
        self.assertIn('bearer', paths['/v1/device/rotate']['post']['responses']['401']['description'])
        self.assertIn('Paired-device authentication is off', paths['/v1/device/rotate']['post']['responses']['404']['description'])

    def test_whole_number_typing_is_declared(self):
        semantics = load('field-semantics.json')
        for name in ('charger_power_kw', 'charger_voltage', 'charger_actual_current', 'charger_pilot_current'):
            self.assertIn(name, semantics['number_rule'])
        self.assertIn('position.power', semantics['number_rule'])


# ---------------------------------------------------------------------------
# Schemas, OpenAPI and examples
# ---------------------------------------------------------------------------
class SchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile = conf.Profile(PROFILE)
        cls.resources = load('resources.schema.json')['$defs']
        cls.examples = load('profile.json')['examples']

    def validator(self, reference, strict=False):
        return self.profile.reference_validator(reference, strict)

    def test_every_schema_is_valid_draft_2020_12(self):
        names = sorted(path.name for path in PROFILE.glob('*.schema.json'))
        self.assertEqual(names, ['auth.schema.json', 'cursor.schema.json', 'discovery.schema.json', 'errors.schema.json',
                                 'resources.schema.json', 'rotation-grace.schema.json', 'sync.schema.json'])
        for name in names:
            with self.subTest(name):
                Draft202012Validator.check_schema(load(name))

    def test_openapi_validates_and_matches_the_routes(self):
        document = load('openapi.json')
        validate_openapi(document, base_uri=PROFILE.as_uri() + '/')
        self.assertEqual(document['openapi'], '3.1.0')
        self.assertEqual(document['info']['version'], '1.2.0')
        operations = {(path, method): operation for path, item in document['paths'].items() for method, operation in item.items()}
        self.assertEqual(len(operations), len(ROUTE_TABLE))
        for kind, (method, path, bearer, statuses) in ROUTE_TABLE.items():
            operation = document['paths'][path][method.lower()]
            self.assertEqual(operation['operationId'], kind)
            self.assertEqual(sorted(int(status) for status in operation['responses']), statuses)
            self.assertEqual(bool(operation['security']), bearer)
            if bearer:
                self.assertEqual(operation['responses']['401']['headers']['WWW-Authenticate']['schema']['const'], 'Bearer')
            if kind in PAGE_LIMITS:
                for status in ('200', '304'):
                    self.assertEqual(set(operation['responses'][status]['headers']), {'ETag', 'Cache-Control'})
                query = {item['name']: item['schema'] for item in operation['parameters'] if item['in'] == 'query'}
                self.assertEqual(set(query), {'from_ms', 'to_ms', 'limit', 'cursor'})
                self.assertEqual((query['limit']['default'], query['limit']['minimum'], query['limit']['maximum']), PAGE_LIMITS[kind])
                self.assertEqual((query['from_ms']['default'], query['from_ms']['minimum']), (0, 0))
                # The open upper bound is I64_MAX, which JavaScript cannot send exactly: it is documented, never a schema default.
                self.assertNotIn('default', query['to_ms'])
                self.assertEqual(query['to_ms']['maximum'], I64_MAX)
                described = {item['name']: item.get('description', '') for item in operation['parameters'] if item['in'] == 'query'}['to_ms']
                self.assertIn('9223372036854775807', described)
                self.assertIn('omit the parameter', described)

    def test_every_response_schema_is_open_and_only_requests_and_fixtures_are_closed(self):
        for name in ('discovery.schema.json', 'errors.schema.json', 'resources.schema.json'):
            self.assertNotIn('additionalProperties', json.dumps(load(name)), name)
        auth = load('auth.schema.json')['$defs']
        self.assertIs(auth['claim_request']['additionalProperties'], False)
        for name in ('invitation', 'claim'):
            self.assertNotIn('additionalProperties', json.dumps(auth[name]), name)
        for name in ('rotation-grace.schema.json', 'sync.schema.json'):
            for _, node in conf.walk_schema(load(name)):
                if 'properties' in node:
                    self.assertIs(node.get('additionalProperties'), False, name)

    def test_resource_defs_and_field_lists(self):
        self.assertEqual(sorted(self.resources), sorted([
            'vehicles', 'current', 'drive', 'drives', 'position', 'positions', 'charge', 'charges', 'charge_sample', 'charge_samples',
            'address_provenance', 'elevation_provenance', 'health', 'ready']))
        drive, position = self.resources['drive'], self.resources['position']
        charge, sample = self.resources['charge'], self.resources['charge_sample']
        self.assertEqual(sorted(drive['properties']), sorted(DRIVE_FIELDS + ['start_address_provenance', 'end_address_provenance']))
        self.assertEqual(sorted(drive['required']), sorted(DRIVE_FIELDS))
        self.assertEqual(len(DRIVE_FIELDS), 28)
        self.assertEqual(sorted(position['properties']), sorted(POSITION_FIELDS + ['elevation_provenance']))
        self.assertEqual(sorted(position['required']), sorted(POSITION_FIELDS))
        self.assertEqual(sorted(charge['properties']), sorted(CHARGE_FIELDS + ['address_provenance']))
        self.assertEqual(sorted(charge['required']), sorted(CHARGE_FIELDS))
        self.assertEqual(sorted(sample['properties']), sorted(SAMPLE_FIELDS))
        self.assertEqual(sorted(sample['required']), sorted(SAMPLE_FIELDS))
        for definition in (drive, position, charge, sample):
            self.assertEqual(len(definition['properties']), len(set(definition['properties'])))

    def test_nullability_and_integer_typing(self):
        for name, fields in (('drive', DRIVE_FIELDS), ('position', POSITION_FIELDS), ('charge', CHARGE_FIELDS), ('charge_sample', SAMPLE_FIELDS)):
            properties = self.resources[name]['properties']
            for field in fields:
                schema = properties[field]
                if field in NON_NULL[name]:
                    self.assertFalse(nullable_schema(schema), name + '.' + field)
                    if field == 'vehicle_id':
                        self.assertEqual(schema['format'], 'uuid')
                    else:
                        self.assertEqual(schema, {'type': 'integer', 'format': 'int64', 'minimum': I64_MIN, 'maximum': I64_MAX}, name + '.' + field)
                else:
                    self.assertTrue(nullable_schema(schema), name + '.' + field)
        for name in ('drive', 'position', 'charge', 'charge_sample'):
            for field in self.resources[name]['properties']:
                if field.endswith('_provenance'):
                    self.assertTrue(nullable_schema(self.resources[name]['properties'][field]), field)
        for field in ('cost', 'cost_per_unit', 'session_fee'):
            self.assertEqual(self.resources['charge']['properties'][field], {'anyOf': [{'type': 'number'}, {'type': 'null'}]})

    def test_every_field_has_exactly_the_json_type_written_out_in_this_module(self):
        for name, fields in (('drive', DRIVE_FIELDS), ('position', POSITION_FIELDS), ('charge', CHARGE_FIELDS), ('charge_sample', SAMPLE_FIELDS)):
            properties = self.resources[name]['properties']
            self.assertEqual(set(FIELD_TYPES[name]), set(fields), name)
            for field in fields:
                expected = JSON_TYPES[FIELD_TYPES[name][field]]
                if field not in NON_NULL[name]:
                    expected = {'anyOf': [expected, {'type': 'null'}]}
                self.assertEqual(properties[field], expected, name + '.' + field)
        # the five deviation fields stay number, so the recorded decision cannot be reverted by regenerating
        self.assertEqual(FIELD_TYPES['position']['power'], 'number')
        for field in ('charger_power_kw', 'charger_voltage', 'charger_actual_current', 'charger_pilot_current'):
            self.assertEqual(FIELD_TYPES['charge_sample'][field], 'number')
        # and the optional provenance objects are nullable references
        for name, field, target in (('drive', 'start_address_provenance', 'address_provenance'), ('drive', 'end_address_provenance', 'address_provenance'),
                                    ('position', 'elevation_provenance', 'elevation_provenance'), ('charge', 'address_provenance', 'address_provenance')):
            self.assertEqual(self.resources[name]['properties'][field], {'anyOf': [{'$ref': '#/$defs/' + target}, {'type': 'null'}]}, field)
            self.assertNotIn(field, self.resources[name]['required'])

    def test_provenance_objects(self):
        address, elevation = self.resources['address_provenance'], self.resources['elevation_provenance']
        self.assertEqual(address['properties']['source'], {'enum': ['import', 'geocoder', 'manual']})
        self.assertEqual(sorted(address['properties']), sorted(['source', 'provider', 'endpoint_host', 'language', 'licence', 'looked_up_at_ms']))
        self.assertEqual(elevation['properties']['source'], {'enum': ['vehicle', 'import', 'terrain']})
        self.assertEqual(sorted(elevation['properties']), sorted(['source', 'dataset', 'dataset_version', 'tile']))
        for definition in (address, elevation):
            self.assertEqual(sorted(definition['required']), sorted(definition['properties']))
            for name, schema in definition['properties'].items():
                if name != 'source':
                    self.assertTrue(nullable_schema(schema), name)

    def test_pages_are_bounded_and_carry_a_bounded_cursor(self):
        for name, maximum in (('drives', 500), ('positions', 2048), ('charges', 500), ('charge_samples', 2048)):
            page = self.resources[name]
            self.assertEqual(sorted(page['required']), ['items', 'next_cursor'])
            self.assertEqual(page['properties']['items']['maxItems'], maximum, name)
            self.assertEqual(page['properties']['next_cursor'], {'anyOf': [{'type': 'string', 'minLength': 1, 'maxLength': 4096}, {'type': 'null'}]})
        self.assertEqual(self.resources['vehicles']['properties']['vehicles']['maxItems'], 10000)

    def test_discovery_schema(self):
        discovery = load('discovery.schema.json')
        self.assertEqual(sorted(discovery['required']), sorted(['hub_id', 'protocol', 'protocol_major', 'api_versions', 'capabilities', 'version',
                                                                'sourceUrl', 'pack_format', 'profile']))
        properties = discovery['properties']
        self.assertEqual(properties['protocol']['const'], 'teslatlas-sync')
        self.assertEqual(properties['protocol_major']['const'], 1)
        self.assertEqual(properties['api_versions']['const'], ['1.0'])
        self.assertEqual(properties['api_versions']['maxItems'], 1)
        self.assertEqual(properties['pack_format']['const'], 'sqlite-zstd')
        self.assertEqual(properties['profile']['pattern'], r'^hub-http-v1@1\.[0-9]+\.[0-9]+$')
        self.assertEqual(properties['manifestPublicKey']['pattern'], '^[0-9a-f]{64}$')
        capabilities = properties['capabilities']
        self.assertEqual((capabilities['maxItems'], capabilities['uniqueItems'], capabilities['items']['pattern']), (16, True, '^[a-z0-9.]{1,64}$'))
        self.assertEqual(capabilities['allOf'], [{'contains': {'const': 'query.vehicles'}}, {'contains': {'const': 'query.current'}}])
        attributions = properties['attributions']
        self.assertEqual(attributions['maxItems'], 8)
        self.assertEqual(sorted(attributions['items']['required']), ['id', 'licence', 'text', 'url'])
        for field in ('id', 'text', 'licence'):
            self.assertIn('maxLength', attributions['items']['properties'][field])

    def test_the_claim_request_schema_states_the_device_name_rule_as_far_as_a_schema_can(self):
        validator = self.validator('auth.schema.json#/$defs/claim_request')
        schema = load('auth.schema.json')['$defs']['claim_request']['properties']['device_name']
        self.assertEqual((schema['type'], schema['minLength'], schema['maxLength']), ('string', 1, 128))
        self.assertIn('cannot state the byte limit', schema['description'])
        self.assertIn('422', schema['description'])
        accepted = {'one character': 'a', 'a space': ' ', 'a name': 'Home Assistant', 'exactly 128 bytes': 'x' * 128, '64 two-byte characters, 128 bytes': '\u00e9' * 64,
                    'a four-byte character': '\U0001f697', 'a no-break space': '\u00a0', 'a line separator, which is not a control character': '\u2028'}
        refused = {'empty': '', '129 characters': 'x' * 129, 'a newline inside': 'a\nb', 'a trailing newline, which an anchored pattern would let through': 'abc\n',
                   'a leading newline': '\nabc', 'NUL': 'a\x00b', 'the last C0 control': 'a\x1fb', 'a tab': 'a\tb', 'a carriage return': 'a\rb', 'DEL': 'a\x7fb',
                   'the first C1 control': 'a\x80b', 'the last C1 control': 'a\x9fb', 'a name and a control character': 'x' * 127 + '\n', '128 characters and one more': 'x' * 128 + '\u00e9'}
        for label, name in accepted.items():
            with self.subTest('accepted: ' + label):
                self.assertTrue(validator.is_valid({'secret': 's', 'device_name': name}))
                self.assertFalse(refused_device_name(name))  # the rule written out in this module agrees
        for label, name in refused.items():
            with self.subTest('refused: ' + label):
                self.assertFalse(validator.is_valid({'secret': 's', 'device_name': name}))
                self.assertTrue(refused_device_name(name))
        # 65 two-byte characters: 65 characters pass maxLength, but the name is 130 bytes, so the rule refuses what a schema cannot
        gap = '\u00e9' * 65
        self.assertTrue(validator.is_valid({'secret': 's', 'device_name': gap}))
        self.assertTrue(refused_device_name(gap))
        self.assertEqual(len(gap), 65)
        self.assertEqual(len(gap.encode('utf-8')), 130)
        for value in (None, 7, ['a']):
            self.assertFalse(validator.is_valid({'secret': 's', 'device_name': value}))
        # the OpenAPI request body is this schema
        claim = load('openapi.json')['paths']['/v1/pairings/{pairing_id}/claim']['post']
        self.assertEqual(claim['requestBody']['content']['application/json']['schema'], {'$ref': 'auth.schema.json#/$defs/claim_request'})

    # The pattern the device_name schema carried before R5-text-N3: a negative look-ahead and the [\s\S] class, which RE2-based engines and the Rust regex crate cannot compile.
    FORMER_DEVICE_NAME_PATTERN = r'^(?![\s\S]*[\u0000-\u001f\u007f-\u009f])'

    def test_the_device_name_rule_is_one_plain_character_class_that_refuses_and_accepts_the_names_the_look_ahead_did(self):
        validator = self.validator('auth.schema.json#/$defs/claim_request')
        schema = load('auth.schema.json')['$defs']['claim_request']['properties']['device_name']
        # the portable form: no pattern of its own, `not` over one pattern, the length limits kept, nothing else added
        self.assertEqual(sorted(schema), ['description', 'maxLength', 'minLength', 'not', 'type'])
        self.assertEqual((schema['type'], schema['minLength'], schema['maxLength']), ('string', 1, 128))
        self.assertEqual(list(schema['not']), ['pattern'])
        pattern = schema['not']['pattern']
        # one class, written here from its code points: the Cc category (U+0000 to U+001F, U+007F to U+009F)
        self.assertEqual(pattern, '[' + chr(0x00) + '-' + chr(0x1f) + chr(0x7f) + '-' + chr(0x9f) + ']')
        # and nothing but a class: no anchor, group, look-ahead, escape, alternation or quantifier (the subset of JSON Schema 2020-12 Core section 6.4)
        self.assertEqual(set(pattern) - set('[]-'), {chr(0x00), chr(0x1f), chr(0x7f), chr(0x9f)})
        self.assertEqual(pattern.count('['), 1)
        self.assertEqual(pattern.count(']'), 1)
        self.assertEqual(pattern.count('-'), 2)
        # the file says it with JSON escapes, so no control character is written into it raw (and nothing of the former pattern is left)
        raw = (PROFILE / 'auth.schema.json').read_text(encoding='utf-8')
        self.assertIn('"pattern": "[\\u0000-\\u001f\\u007f-\\u009f]"', raw)
        for left_over in ('(?!', '[\\s\\S]', '^(?'):
            self.assertNotIn(left_over, raw)
        # every code point, alone and between two letters: the class finds a control character exactly where the Cc category has one, and the verdict is the look-ahead's
        found, former = re.compile(pattern), re.compile(self.FORMER_DEVICE_NAME_PATTERN)
        differing = []
        for code in range(0x110000):
            if 0xd800 <= code <= 0xdfff:
                continue  # a surrogate is not a character of a JSON string
            name = 'a' + chr(code) + 'b'
            control = unicodedata.category(chr(code)) == 'Cc'
            if (found.search(name) is not None) != control or (former.search(name) is None) != control:
                differing.append(hex(code))
        self.assertEqual(differing, [])
        # the schema itself, by its own validator, gives the verdict of the former pattern on names that exercise each position and each range edge
        positions = ('{c}', 'a{c}', '{c}a', 'a{c}b', '{c}{c}', 'x' * 126 + '{c}')
        edges = (0x00, 0x01, 0x09, 0x0a, 0x0d, 0x1f, 0x20, 0x7e, 0x7f, 0x80, 0x84, 0x85, 0x86, 0x9f, 0xa0, 0xad, 0xff, 0x2028, 0x2029, 0xfeff, 0x1f697)
        for code in edges:
            for position in positions:
                name = position.format(c=chr(code))
                with self.subTest(code=hex(code), name=ascii(name)):
                    self.assertEqual(validator.is_valid({'secret': 's', 'device_name': name}), former.search(name) is not None)
                    self.assertEqual(validator.is_valid({'secret': 's', 'device_name': name}), unicodedata.category(chr(code)) != 'Cc')
        for name in ('', 'x' * 129, '\u00e9' * 129):  # the length limits are still the schema's own
            self.assertFalse(validator.is_valid({'secret': 's', 'device_name': name}), ascii(name[:8]))
        for name in ('a', 'x' * 128, '\u00e9' * 128, '\u00e9' * 65, 'Home Assistant'):
            self.assertTrue(validator.is_valid({'secret': 's', 'device_name': name}), ascii(name[:8]))

    def test_errors_and_auth_schemas(self):
        errors = load('errors.schema.json')
        self.assertEqual(errors['properties']['error']['properties']['code']['pattern'], '^[a-z_]{1,64}$')
        self.assertEqual(sorted(errors['properties']['error']['required']), ['code', 'message'])
        auth = load('auth.schema.json')['$defs']
        self.assertEqual(sorted(auth), ['claim', 'claim_request', 'invitation'])
        self.assertEqual(auth['claim']['properties']['reason'], {'enum': ['max_lifetime']})
        self.assertEqual(sorted(auth['claim']['required']), ['access_token', 'device_id', 'expires_at_ms'])
        self.assertEqual(sorted(auth['claim_request']['required']), ['device_name', 'secret'])

    # -- examples ------------------------------------------------------------
    def test_examples_are_exactly_the_nineteen_named_in_the_specification(self):
        self.assertEqual(sorted(self.examples), sorted([
            'discovery', 'discovery-unsigned', 'vehicles', 'current', 'drives', 'drive', 'positions', 'charges', 'charge', 'charge-samples',
            'claim', 'rotate-max-lifetime', 'invitation', 'error-drive-not-found', 'error-charge-not-found', 'health', 'ready', 'rotation-grace',
            'sync-schema-2-2-noop-unavailable']))

    def test_every_example_validates_tolerant_and_server_strict(self):
        for name, reference in self.examples.items():
            value = example(name)
            for strict in (False, True):
                with self.subTest(example=name, strict=strict):
                    errors = list(self.validator(reference, strict).iter_errors(value))
                    self.assertEqual(errors, [])

    def test_example_values_are_synthetic(self):
        for item in example('positions')['items']:
            self.assertEqual((item['latitude'], item['longitude']), (0.0, 0.0))
            # whole numbers typed number may carry a fraction digit but the examples show them whole, as the number rule says
            self.assertTrue(item['power'] is None or float(item['power']).is_integer(), item['id'])
            # provenance is null only when the elevation is null
            self.assertEqual(item['elevation'] is None, item['elevation_provenance'] is None, item['id'])
        for item in example('charges')['items']:
            self.assertIn(item['start_latitude'], (None, 0.0))
            self.assertIn(item['start_longitude'], (None, 0.0))
        for item in example('drives')['items']:
            for field in ('start_latitude', 'start_longitude', 'end_latitude', 'end_longitude'):
                self.assertIn(item[field], (None, 0.0))
        self.assertEqual(example('discovery')['sourceUrl'], 'https://example.invalid/source')
        self.assertEqual(example('invitation')['endpoint'], 'https://example.invalid')
        self.assertEqual(example('claim')['access_token'], '0' * 64)
        self.assertEqual(example('drive')['vehicle_id'], '11111111-1111-4111-8111-111111111111')
        self.assertEqual(example('discovery')['attributions'], [
            {'id': 'osm', 'text': '(c) OpenStreetMap contributors', 'licence': 'ODbL-1.0', 'url': 'https://www.openstreetmap.org/copyright'}])
        for name in self.examples:
            self.assertNotRegex((PROFILE / 'examples' / (name + '.json')).read_text(), r'(?i)\b[A-HJ-NPR-Z0-9]{17}\b', name)

    def test_discovery_examples(self):
        full, base = example('discovery'), example('discovery-unsigned')
        self.assertEqual(full['capabilities'], CAPABILITIES)
        self.assertEqual(full['profile'], PROFILE_ID)
        self.assertEqual(base['profile'], PROFILE_ID)
        self.assertEqual(base['capabilities'], ['query.vehicles', 'query.current'])
        self.assertNotIn('manifestPublicKey', base)
        self.assertNotIn('attributions', base)

    def test_rotate_example_carries_the_reason_and_claim_does_not(self):
        self.assertEqual(example('rotate-max-lifetime')['reason'], 'max_lifetime')
        self.assertNotIn('reason', example('claim'))
        validator = self.validator('auth.schema.json#/$defs/claim')
        self.assertFalse(validator.is_valid({**example('claim'), 'reason': 'something_else'}))

    # -- negative tests ---------------------------------------------------------
    def test_an_undocumented_field_fails_server_strict_but_passes_tolerant(self):
        closed = {'rotation-grace', 'sync-schema-2-2-noop-unavailable'}
        for name, reference in self.examples.items():
            value = example(name)
            value['undocumented_field'] = 1
            with self.subTest(example=name):
                self.assertEqual(self.validator(reference).is_valid(value), name not in closed)
                self.assertFalse(self.validator(reference, True).is_valid(value))

    def test_an_undocumented_nested_field_fails_server_strict_but_passes_tolerant(self):
        for name, path in (('drives', ['items', 0]), ('drives', ['items', 0, 'end_address_provenance']), ('positions', ['items', 0, 'elevation_provenance']),
                           ('charges', ['items', 0, 'address_provenance']), ('charge-samples', ['items', 1]), ('vehicles', ['vehicles', 0]),
                           ('discovery', ['attributions', 0]), ('error-drive-not-found', ['error']), ('current', ['car']), ('current', ['car', 'settings'])):
            value = example(name)
            target = value
            for step in path:
                target = target[step]
            self.assertIsInstance(target, dict, (name, path))
            target['undocumented_field'] = 'x'
            with self.subTest(example=name, path=path):
                self.assertEqual(list(self.validator(self.examples[name]).iter_errors(value)), [])
                self.assertFalse(self.validator(self.examples[name], True).is_valid(value))

    def required_of(self, reference):
        name, _, fragment = reference.partition('#')
        document = load(name)
        return (document['$defs'][fragment.rsplit('/', 1)[-1]] if fragment else document).get('required', [])

    def test_a_missing_required_field_fails_tolerant_and_server_strict(self):
        checked = 0
        for name, reference in self.examples.items():
            for field in self.required_of(reference):
                value = example(name)
                del value[field]
                checked += 1
                for strict in (False, True):
                    with self.subTest(example=name, removed=field, strict=strict):
                        self.assertFalse(self.validator(reference, strict).is_valid(value))
        self.assertGreater(checked, 40)
        for strict in (False, True):
            self.assertFalse(self.validator(self.examples['ready'], strict).is_valid({}))

    def test_a_missing_required_item_field_fails_tolerant_and_server_strict(self):
        for name, fields in (('drives', DRIVE_FIELDS), ('positions', POSITION_FIELDS), ('charges', CHARGE_FIELDS), ('charge-samples', SAMPLE_FIELDS)):
            for field in fields:
                value = example(name)
                del value['items'][0][field]
                for strict in (False, True):
                    with self.subTest(example=name, removed=field, strict=strict):
                        self.assertFalse(self.validator(self.examples[name], strict).is_valid(value))

    def test_non_null_fields_reject_null_and_nullable_fields_accept_it(self):
        for name, item_name, fields in (('drives', 'drive', DRIVE_FIELDS), ('positions', 'position', POSITION_FIELDS),
                                        ('charges', 'charge', CHARGE_FIELDS), ('charge-samples', 'charge_sample', SAMPLE_FIELDS)):
            for field in fields:
                value = example(name)
                value['items'][0][field] = None
                with self.subTest(example=name, field=field):
                    self.assertEqual(self.validator(self.examples[name]).is_valid(value), field not in NON_NULL[item_name])

    def test_signed_64_bit_bounds_on_identifiers_and_milliseconds(self):
        for name, field in (('drive', 'id'), ('drive', 'start_date_ms'), ('charge', 'end_date_ms'), ('positions', 'id'), ('charge-samples', 'timestamp_ms')):
            for number, valid in ((I64_MAX, True), (I64_MAX + 1, False), (I64_MIN, True), (I64_MIN - 1, False)):
                value = example(name)
                (value['items'][0] if 'items' in value else value)[field] = number
                with self.subTest(example=name, field=field, number=number):
                    self.assertEqual(self.validator(self.examples[name]).is_valid(value), valid)
        value = example('drive')
        value['id'] = 1.5
        self.assertFalse(self.validator(self.examples['drive']).is_valid(value))

    def test_an_array_over_max_items_fails(self):
        for name, key, maximum in (('drives', 'items', 500), ('positions', 'items', 2048), ('charges', 'items', 500),
                                   ('charge-samples', 'items', 2048), ('vehicles', 'vehicles', 10000)):
            value = example(name)
            value[key] = [value[key][0]] * maximum
            with self.subTest(example=name):
                self.assertTrue(self.validator(self.examples[name]).is_valid(value), 'the bound itself is allowed')
                value[key].append(value[key][0])
                self.assertEqual(len(value[key]), maximum + 1)
                for strict in (False, True):
                    self.assertFalse(self.validator(self.examples[name], strict).is_valid(value))
        discovery = example('discovery')
        discovery['capabilities'] = ['query.vehicles', 'query.current'] + ['x.token%d' % index for index in range(15)]
        self.assertFalse(self.validator('discovery.schema.json').is_valid(discovery))
        discovery['capabilities'] = discovery['capabilities'][:16]
        self.assertTrue(self.validator('discovery.schema.json').is_valid(discovery))
        discovery['attributions'] = [dict(example('discovery')['attributions'][0], id='a%d' % index) for index in range(9)]
        self.assertFalse(self.validator('discovery.schema.json').is_valid(discovery))
        discovery['attributions'] = discovery['attributions'][:8]
        self.assertTrue(self.validator('discovery.schema.json').is_valid(discovery))

    def test_every_array_schema_in_every_file_has_max_items(self):
        nodes = []
        for path in sorted(PROFILE.glob('*.schema.json')):
            nodes += [(path.name + pointer, node) for pointer, node in conf.walk_schema(json.loads(path.read_text()))]
        nodes += list(conf.iter_openapi_schemas(load('openapi.json')))
        arrays = [pointer for pointer, node in nodes if node.get('type') == 'array' or 'items' in node]
        self.assertGreaterEqual(len(arrays), 8)
        self.assertEqual(conf.unbounded_arrays(nodes), [])
        broken = copy.deepcopy(self.resources['positions'])
        del broken['properties']['items']['maxItems']
        self.assertEqual(len(conf.unbounded_arrays(list(conf.walk_schema(broken)))), 1)

    def test_strict_variant_closes_only_objects_that_declare_properties(self):
        original = {'type': 'object', 'properties': {'a': {'type': 'object', 'properties': {'b': {'type': 'integer'}}},
                                                     'items': {'type': 'array', 'maxItems': 2, 'items': {'type': 'object', 'properties': {'c': {}}}},
                                                     'free': {'type': 'object'}}}
        strict = conf.strict_variant(original)
        self.assertIs(strict['additionalProperties'], False)
        self.assertIs(strict['properties']['a']['additionalProperties'], False)
        self.assertIs(strict['properties']['items']['items']['additionalProperties'], False)
        self.assertNotIn('additionalProperties', strict['properties']['free'])
        self.assertNotIn('additionalProperties', original)

    # -- the car of /current sits under anyOf (nullable): strictness and bounds must reach it ------------------------------
    def test_a_current_example_with_a_car_is_closed_in_the_car_and_its_settings_under_server_strict(self):
        reference = self.examples['current']
        base = example('current')
        self.assertIsInstance(base['car'], dict)  # the example carries a car, so the nested objects are exercised
        self.assertIsInstance(base['car']['settings'], dict)
        for strict in (False, True):
            self.assertTrue(self.validator(reference, strict).is_valid(base))
        for label, where in (('car', lambda value: value['car']), ('car.settings', lambda value: value['car']['settings']), ('current', lambda value: value)):
            value = example('current')
            where(value)['undocumented_field'] = 1
            with self.subTest(label):
                self.assertTrue(self.validator(reference).is_valid(value), 'tolerant validation ignores an unknown field')
                self.assertFalse(self.validator(reference, True).is_valid(value), 'server-strict validation rejects it')
        for label, remove in (('car', lambda value: value['car'].pop('name')), ('car.settings', lambda value: value['car']['settings'].pop('suspend_min'))):
            value = example('current')
            remove(value)
            with self.subTest('required ' + label):
                self.assertFalse(self.validator(reference).is_valid(value))
        value = example('current')
        value['car'] = None  # a car may be absent
        self.assertTrue(self.validator(reference, True).is_valid(value))

    def test_walk_schema_reaches_an_array_that_only_a_nullable_wrapper_holds(self):
        def nullable(schema):
            return {'anyOf': [schema, {'type': 'null'}]}

        unbounded = nullable({'type': 'array', 'items': {}})
        self.assertEqual(conf.unbounded_arrays(list(conf.walk_schema(unbounded))), ['/anyOf/0'])
        self.assertEqual(conf.unbounded_arrays(list(conf.walk_schema(nullable({'type': 'array', 'maxItems': 3, 'items': {}})))), [])
        nested = {'type': 'object', 'properties': {'list': nullable({'type': 'array', 'items': {'type': 'string'}})}}
        self.assertEqual(conf.unbounded_arrays(list(conf.walk_schema(nested))), ['/properties/list/anyOf/0'])
        # the other list keywords are traversed too
        for keyword in ('allOf', 'oneOf'):
            self.assertEqual(conf.unbounded_arrays(list(conf.walk_schema({keyword: [{'type': 'array', 'items': {}}]}))), ['/' + keyword + '/0'])
        # and the strict variant closes an object that sits only under anyOf
        closed = conf.strict_variant(nullable({'type': 'object', 'properties': {'a': {}}}))
        self.assertIs(closed['anyOf'][0]['additionalProperties'], False)

    def test_the_max_items_check_reaches_an_unbounded_array_under_anyOf_in_a_schema_file(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name) / '1.2.0'
        shutil.copytree(PROFILE, root)
        document = json.loads((root / 'discovery.schema.json').read_text())
        document['properties']['sourceUrl'] = {'anyOf': [{'type': 'array', 'items': {'type': 'string'}}, {'type': 'null'}]}
        (root / 'discovery.schema.json').write_text(json.dumps(document, indent=2, sort_keys=True) + '\n')
        reseal(root)
        result = {item['case_id']: item for item in conf.check_offline(root)}['offline/max-items']
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(result['detail'], 'array schema without maxItems at discovery.schema.json/properties/sourceUrl/anyOf/0')

    def test_the_max_items_check_reaches_inline_arrays_in_openapi_parameters_and_responses(self):
        def injected(change):
            temp = tempfile.TemporaryDirectory()
            self.addCleanup(temp.cleanup)
            root = Path(temp.name) / '1.2.0'
            shutil.copytree(PROFILE, root)
            document = json.loads((root / 'openapi.json').read_text())
            change(document)
            (root / 'openapi.json').write_text(json.dumps(document, indent=2, sort_keys=True) + '\n')
            reseal(root)
            return {item['case_id']: item for item in conf.check_offline(root)}['offline/max-items']

        def parameter(document):
            item = next(value for value in document['paths']['/v1/vehicles/{vehicle_id}/drives']['get']['parameters'] if value['name'] == 'cursor')
            item['schema'] = {'type': 'array', 'items': {'type': 'string'}}

        def response(document):
            document['paths']['/v1/vehicles']['get']['responses']['200']['content']['application/json']['schema'] = {
                'type': 'array', 'items': {'$ref': 'resources.schema.json#/$defs/vehicles'}}

        def header(document):
            document['paths']['/v1/vehicles/{vehicle_id}/drives']['get']['responses']['200']['headers']['ETag']['schema'] = {'type': 'array', 'items': {'type': 'string'}}

        def request_body(document):
            document['paths']['/v1/pairings/{pairing_id}/claim']['post']['requestBody']['content']['application/json']['schema'] = {
                'type': 'array', 'items': {'type': 'string'}}

        def component(document):
            document.setdefault('components', {}).setdefault('schemas', {})['Unbounded'] = {'type': 'array', 'items': {'type': 'string'}}

        control = injected(lambda document: None)
        self.assertEqual(control['status'], 'passed')
        # the pointer names the parameter, the header and the part of the operation, so a failure says where the array is
        for label, change, pointer in (('parameter', parameter, '/paths//v1/vehicles/{vehicle_id}/drives/get/parameters/cursor'),
                                       ('response', response, '/paths//v1/vehicles/get/200/media'),
                                       ('header', header, '/paths//v1/vehicles/{vehicle_id}/drives/get/200/headers/ETag'),
                                       ('request body', request_body, '/paths//v1/pairings/{pairing_id}/claim/post/requestBody/media'),
                                       ('component', component, '/components/schemas/Unbounded')):
            with self.subTest(label):
                result = injected(change)
                self.assertEqual(result['status'], 'failed')
                self.assertEqual(result['detail'], 'array schema without maxItems at ' + pointer)

    def test_an_array_is_recognised_by_its_type_its_items_or_its_prefix_items_and_its_bound_must_be_an_integer(self):
        def flagged(node):
            return conf.unbounded_arrays(list(conf.walk_schema(node)))

        # an array schema that does not say `type: array` is still an array when it has items or prefixItems
        for node in ({'items': {}}, {'prefixItems': [{}]}, {'type': ['array', 'null']}, {'type': 'array'}):
            with self.subTest(node):
                self.assertEqual(flagged(node), ['/'])
        for bound in (True, '3', 3.5, None, [3]):  # only an integer is a bound; a boolean is not one
            with self.subTest(bound):
                self.assertEqual(flagged({'type': 'array', 'maxItems': bound, 'items': {}}), ['/'])
        self.assertEqual(flagged({'type': 'array', 'maxItems': 0, 'items': {}}), [])
        self.assertEqual(flagged({'type': 'object', 'properties': {'items': {'type': 'string'}, 'prefixItems': {'type': 'string'}}}), [])  # property names, not arrays

    # -- format int64 is an annotation -----------------------------------------------------------------------------------
    def schema_documents(self):
        documents = {path.name: json.loads(path.read_text()) for path in sorted(PROFILE.glob('*.schema.json'))}
        documents['openapi.json'] = load('openapi.json')
        return documents

    def test_every_signed_64_bit_integer_schema_declares_format_int64(self):
        documents = self.schema_documents()
        nodes = [(name + pointer, node) for name, document in documents.items() if name != 'openapi.json' for pointer, node in conf.walk_schema(document)]
        nodes += [('openapi.json' + pointer, node) for pointer, node in conf.iter_openapi_schemas(documents['openapi.json'])]
        wide = [(pointer, node) for pointer, node in nodes if node.get('type') == 'integer' and node.get('maximum') == I64_MAX]
        self.assertGreater(len(wide), 60)  # every id, *_ms and integer field of the resources, plus the path and window parameters
        for pointer, node in wide:
            self.assertEqual(node.get('format'), 'int64', pointer)
        self.assertEqual(sorted({node['format'] for _, node in nodes if 'format' in node}), ['int64', 'uri', 'uuid'])
        for pointer, node in nodes:
            if node.get('format') == 'int64':
                self.assertEqual(node.get('type'), 'integer', pointer)
        # the named parameters, which a generator reads: ids and the two window bounds are int64, the limit is not 64-bit
        paths = documents['openapi.json']['paths']
        parameters = {item['name']: item['schema'] for path in ('/v1/vehicles/{vehicle_id}/drives/{drive_id}', '/v1/vehicles/{vehicle_id}/drives')
                      for item in paths[path]['get']['parameters']}
        for name in ('drive_id', 'from_ms', 'to_ms'):
            self.assertEqual(parameters[name]['format'], 'int64', name)
        self.assertNotIn('format', parameters['limit'])
        self.assertEqual(parameters['limit']['maximum'], 500)

    def test_format_int64_is_an_annotation_no_validator_of_the_package_checks_it(self):
        self.assertNotIn('int64', FormatChecker.checkers)  # the checker every validator in the package uses has no such format
        self.assertNotIn('int64', FormatChecker().checkers)
        resources = load('resources.schema.json')

        def stripped(node):
            if isinstance(node, dict):
                return {key: stripped(value) for key, value in node.items() if not (key == 'format' and value == 'int64')}
            if isinstance(node, list):
                return [stripped(value) for value in node]
            return node

        without = stripped(resources)
        self.assertNotEqual(json.dumps(resources), json.dumps(without))  # the strip removed something
        values = [example(name) for name in ('drives', 'positions', 'charges', 'charge-samples', 'vehicles', 'current')]
        for number in (I64_MAX, I64_MAX + 1, I64_MIN, I64_MIN - 1, 2 ** 31, 2 ** 53 + 1, 1.5, '7', None):
            for name, field in (('drives', 'id'), ('charges', 'end_date_ms'), ('positions', 'id'), ('charge-samples', 'timestamp_ms')):
                value = example(name)
                value['items'][0][field] = number
                values.append(value)
        verdicts = set()
        for definition in ('drives', 'positions', 'charges', 'charge_samples', 'vehicles', 'current'):
            probe = {'$ref': '#/$defs/' + definition}
            left = Draft202012Validator({**resources, **probe}, format_checker=FormatChecker())
            right = Draft202012Validator({**without, **probe}, format_checker=FormatChecker())
            for value in values:
                verdicts.add(left.is_valid(value))
                self.assertEqual(left.is_valid(value), right.is_valid(value), definition)
        self.assertEqual(verdicts, {True, False})  # the comparison covered values that pass and values that fail
        # a value above 32 bits passes the int64 schema, as the epoch milliseconds a Hub serves today do
        value = example('drive')
        value['start_date_ms'] = 1788565900000
        self.assertGreater(value['start_date_ms'], 2 ** 31)
        self.assertTrue(self.validator(self.examples['drive']).is_valid(value))

    def test_a_cost_given_as_text_is_rejected(self):
        value = example('charges')
        value['items'][0]['cost'] = '4.50'
        self.assertFalse(self.validator(self.examples['charges']).is_valid(value))


# ---------------------------------------------------------------------------
# Discovery invariants and capabilities
# ---------------------------------------------------------------------------
class DiscoveryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile = conf.Profile(PROFILE)
        cls.validator = cls.profile.validator('discovery.schema.json')

    def test_home_assistant_invariants_in_the_schema(self):
        base = example('discovery-unsigned')
        self.assertTrue(self.validator.is_valid(base))
        mutations = {
            'api_versions 1.1': lambda value: value.update(api_versions=['1.1']),
            'api_versions two': lambda value: value.update(api_versions=['1.0', '1.1']),
            'api_versions empty': lambda value: value.update(api_versions=[]),
            'api_versions text': lambda value: value.update(api_versions='1.0'),
            'no query.vehicles': lambda value: value.update(capabilities=['query.current']),
            'no query.current': lambda value: value.update(capabilities=['query.vehicles']),
            'no capabilities': lambda value: value.update(capabilities=[]),
            'no profile': lambda value: value.pop('profile'),
            'other profile family': lambda value: value.update(profile='hub-sync-v1@1.4.0'),
            'profile 2.0.0': lambda value: value.update(profile='hub-http-v2@2.0.0'),
            'protocol': lambda value: value.update(protocol='teslatlas-v1'),
            'protocol_major': lambda value: value.update(protocol_major=2),
            'pack_format': lambda value: value.update(pack_format='zip'),
            'duplicate token': lambda value: value.update(capabilities=['query.vehicles', 'query.current', 'query.current']),
            'upper-case token': lambda value: value.update(capabilities=['query.vehicles', 'query.current', 'Query.Drives']),
            'long token': lambda value: value.update(capabilities=['query.vehicles', 'query.current', 'a' * 65]),
            'empty token': lambda value: value.update(capabilities=['query.vehicles', 'query.current', '']),
            'short manifest key': lambda value: value.update(manifestPublicKey='00'),
            'attribution without licence': lambda value: value.update(attributions=[{'id': 'a', 'text': 't', 'url': 'https://example.invalid/'}])}
        for label, mutate in mutations.items():
            value = copy.deepcopy(base)
            mutate(value)
            with self.subTest(label):
                self.assertFalse(self.validator.is_valid(value))

    def test_unknown_tokens_and_fields_are_tolerated(self):
        value = example('discovery-unsigned')
        value['capabilities'] = value['capabilities'] + ['query.future.thing']
        value['future_field'] = {'a': 1}
        self.assertTrue(self.validator.is_valid(value))

    def test_semantic_discovery_check(self):
        self.assertEqual(conf.check_discovery(example('discovery'), profile_id=PROFILE_ID), [])
        self.assertEqual(conf.check_discovery(example('discovery-unsigned'), profile_id=PROFILE_ID), [])
        problems = {
            'api version': dict(example('discovery'), api_versions=['1.1']),
            'protocol': dict(example('discovery'), protocol='other'),
            'major': dict(example('discovery'), protocol_major=2),
            'current missing': dict(example('discovery'), capabilities=['query.vehicles']),
            'partial cursor-key set': dict(example('discovery'), capabilities=['query.vehicles', 'query.current', 'query.drives']),
            'enrichment without attributions': dict(example('discovery'), attributions=[]),
            'other profile': dict(example('discovery'), profile='hub-http-v1@1.0.0'),
            'duplicates': dict(example('discovery'), capabilities=['query.vehicles', 'query.current', 'query.current']),
            'not an object': []}
        for label, value in problems.items():
            with self.subTest(label):
                self.assertTrue(conf.check_discovery(value, profile_id=PROFILE_ID))

    def test_a_token_outside_the_profile_is_flagged_when_the_set_is_given(self):
        tokens = load('profile.json')['capabilities']
        extra = dict(example('discovery'), capabilities=example('discovery')['capabilities'] + ['query.metadata', 'commands.send'])
        self.assertEqual(conf.check_discovery(extra, profile_id=PROFILE_ID), [])  # without the set a client-side check stays tolerant
        self.assertEqual(conf.check_discovery(extra, profile_id=PROFILE_ID, tokens=tokens), ['discovery advertises a token outside the profile'])
        self.assertEqual(conf.check_discovery(example('discovery'), profile_id=PROFILE_ID, tokens=tokens), [])
        self.assertEqual(conf.check_discovery(example('discovery-unsigned'), profile_id=PROFILE_ID, tokens=tokens), [])
        self.assertEqual(tokens, CAPABILITIES)

    def test_capability_grammar(self):
        self.assertEqual(conf.capability_problems(CAPABILITIES), [])
        self.assertEqual(conf.capability_problems(['a' * 64]), [])
        self.assertEqual(conf.capability_problems(['query.' + 'a' * 58]), [])
        for tokens in (['A'], [''], ['a' * 65], ['query_drives'], ['query-drives'], ['query drives'], ['x', 'x'], ['t%d' % index for index in range(17)], 'query.vehicles', [1]):
            with self.subTest(tokens=tokens):
                self.assertTrue(conf.capability_problems(tokens))
        self.assertEqual(conf.capability_problems(['t%d' % index for index in range(16)]), [])
        for token in load('profile.json')['capabilities']:
            self.assertIsNotNone(re.fullmatch(r'[a-z0-9.]{1,64}', token))
        self.assertEqual(len(set(CAPABILITIES)), len(CAPABILITIES))
        self.assertLessEqual(len(CAPABILITIES), 16)

    def test_routes_name_only_advertised_capabilities(self):
        profile = load('profile.json')
        for route in [*profile['routes'], *profile['companion_routes']]:
            self.assertIn(route.get('capability', 'query.vehicles'), CAPABILITIES)


# ---------------------------------------------------------------------------
# Cursors
# ---------------------------------------------------------------------------
KEY = bytes(range(32))
DOMAIN = b'teslatlas-hub/public-query-cursor/v1\x00'


def cursor_text(kind, vehicle, scope, from_ms, to_ms, key_a, key_b, *, key=KEY, version=1):
    payload = bytes([version, kind]) + uuid.UUID(vehicle).bytes + struct.pack('>qqqqq', scope, from_ms, to_ms, key_a, key_b)
    return 'tqp1.' + payload.hex() + '.' + hmac.new(key, DOMAIN + payload, hashlib.sha256).hexdigest()


class CursorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile = conf.Profile(PROFILE)
        cls.tqp1 = cls.profile.validator('cursor.schema.json', 'tqp1')
        cls.tqd1 = cls.profile.validator('cursor.schema.json', 'tqd1')
        cls.any = cls.profile.validator('cursor.schema.json')

    def example_cursors(self):
        return {name: example(name)['next_cursor'] for name in ('drives', 'positions', 'charges')}

    def test_layout_in_the_profile(self):
        cursor = load('profile.json')['cursor']
        self.assertEqual(cursor['format'], 'tqp1')
        self.assertEqual(cursor['payload']['bytes'], 58)
        self.assertEqual([item['bytes'] for item in cursor['payload']['layout']], [1, 1, 16, 8, 8, 8, 8, 8])
        self.assertEqual(sum(item['bytes'] for item in cursor['payload']['layout']), 58)
        self.assertEqual(cursor['payload']['layout'][1]['values'], {'drives': 1, 'charges': 2, 'positions': 3, 'charge_samples': 4})
        self.assertEqual((cursor['tag']['bytes'], cursor['tag']['algorithm']), (32, 'HMAC-SHA256'))
        self.assertEqual(cursor['tag']['domain_prefix'], 'teslatlas-hub/public-query-cursor/v1 followed by one NUL byte')
        self.assertEqual(cursor['binding'], ['Hub installation cursor key', 'cursor kind', 'vehicle UUID', 'scope id', 'exact from_ms', 'exact to_ms'])
        self.assertEqual(cursor['legacy'], {
            'format': 'tqd1', 'accepted_on': ['drives'], 'issued': False, 'accepted_for': 'one release', 'payload_bytes': 49, 'tag_bytes': 32,
            'payload_layout': 'version 1 (1 byte), vehicle UUID (16), from_ms (8), to_ms (8), last start_date_ms (8), last drive id (8)'})
        self.assertIn('never parse or log', cursor['handling'])
        self.assertIn('invalid_cursor', cursor['handling'])

    def test_generated_example_cursors_are_valid_tqp1_with_the_documented_tag(self):
        key = bytes.fromhex(load('profile.json')['cursor']['tag']['example_key_hex'])
        self.assertEqual(key, KEY)
        for name, text in self.example_cursors().items():
            with self.subTest(name):
                self.assertTrue(self.tqp1.is_valid(text))
                self.assertTrue(self.any.is_valid(text))
                self.assertFalse(self.tqd1.is_valid(text))
                self.assertEqual(len(text), 186)
                fields = conf.decode_cursor(text)
                payload = bytes.fromhex(text.split('.')[1])
                self.assertEqual(len(payload), 58)
                self.assertEqual(text.split('.')[2], hmac.new(key, DOMAIN + payload, hashlib.sha256).hexdigest())
                self.assertEqual(fields['kind'], {'drives': 1, 'positions': 3, 'charges': 2}[name])
        self.assertEqual(conf.decode_cursor(self.example_cursors()['positions'])['scope_id'], example('positions')['items'][0]['drive_id'])
        self.assertEqual(conf.decode_cursor(self.example_cursors()['drives'])['scope_id'], 0)

    def test_the_grammar_accepts_tqp1_and_rejects_other_forms(self):
        good = cursor_text(1, str(uuid.uuid4()), 0, 0, I64_MAX, 5, 6)
        self.assertTrue(self.tqp1.is_valid(good))
        legacy_payload = bytes([1]) + uuid.uuid4().bytes + struct.pack('>qqqq', 0, I64_MAX, 5, 6)
        legacy = 'tqd1.' + legacy_payload.hex() + '.' + hmac.new(KEY, DOMAIN + legacy_payload, hashlib.sha256).hexdigest()
        self.assertEqual((len(legacy_payload), len(legacy)), (49, 168))
        self.assertTrue(self.tqd1.is_valid(legacy))
        self.assertTrue(self.any.is_valid(legacy))
        self.assertFalse(self.tqp1.is_valid(legacy))
        prefix, payload, tag = good.split('.')
        for label, text in {
            'other prefix': 'tqx1.' + payload + '.' + tag, 'drives prefix': 'tqd1.' + payload + '.' + tag, 'upper-case payload': 'tqp1.' + payload.upper() + '.' + tag,
            'upper-case tag': 'tqp1.' + payload + '.' + tag.upper(), 'short payload': 'tqp1.' + payload[:-2] + '.' + tag, 'long payload': 'tqp1.' + payload + '00.' + tag,
            'short tag': 'tqp1.' + payload + '.' + tag[:-2], 'no tag': 'tqp1.' + payload, 'empty': '', 'trailing text': good + 'x',
            'leading space': ' ' + good, 'non-hex': 'tqp1.' + 'g' * 116 + '.' + tag, 'newline': good + '\n'}.items():
            with self.subTest(label):
                self.assertFalse(self.any.is_valid(text))
        self.assertFalse(self.any.is_valid(123))

    def test_decode_rejects_other_kinds_and_versions(self):
        vehicle = str(uuid.uuid4())
        for kind in (1, 2, 3, 4):
            self.assertEqual(conf.decode_cursor(cursor_text(kind, vehicle, 7, 1, 2, 3, 4)), {
                'kind': kind, 'vehicle_id': vehicle, 'scope_id': 7, 'from_ms': 1, 'to_ms': 2, 'key_a': 3, 'key_b': 4})
        for kind in (0, 5, 255):
            with self.subTest(kind=kind):
                with self.assertRaises(conf.ContractError):
                    conf.decode_cursor(cursor_text(kind, vehicle, 0, 0, I64_MAX, 1, 1))
        with self.assertRaises(conf.ContractError):
            conf.decode_cursor(cursor_text(1, vehicle, 0, 0, I64_MAX, 1, 1, version=2))
        with self.assertRaises(conf.ContractError):
            conf.decode_cursor('tqd1.' + '0' * 98 + '.' + '0' * 64)
        with self.assertRaises(conf.ContractError):
            conf.decode_cursor(None)

    def test_a_cursor_of_one_kind_does_not_describe_a_page_of_another(self):
        drives, charges = example('drives'), example('charges')
        self.assertEqual(conf.check_page(self.profile, 'drives', drives, vehicle_id=drives['items'][0]['vehicle_id']), [])
        self.assertEqual(conf.check_page(self.profile, 'charges', charges, vehicle_id=charges['items'][0]['vehicle_id']), [])
        swapped = dict(charges, next_cursor=drives['next_cursor'])
        self.assertIn('next_cursor does not describe this page', conf.check_page(self.profile, 'charges', swapped, vehicle_id=charges['items'][0]['vehicle_id']))
        positions = example('positions')
        scope = positions['items'][0]['drive_id']
        self.assertEqual(conf.check_page(self.profile, 'drive_positions', positions, scope_id=scope), [])
        self.assertTrue(conf.check_page(self.profile, 'drive_positions', positions, scope_id=scope + 1))
        self.assertTrue(conf.check_page(self.profile, 'drive_positions', positions, scope_id=scope, from_ms=1))
        self.assertTrue(conf.check_page(self.profile, 'drive_positions', positions, scope_id=scope, to_ms=I64_MAX - 1))
        self.assertTrue(conf.check_page(self.profile, 'drive_positions', dict(positions, next_cursor='not-a-cursor'), scope_id=scope))
        self.assertTrue(conf.check_page(self.profile, 'drives', dict(drives, items=[]), vehicle_id=drives['items'][0]['vehicle_id']))

    def test_page_order_window_and_continuity_checks(self):
        drives = example('drives')
        vehicle = drives['items'][0]['vehicle_id']
        reversed_page = dict(drives, items=list(reversed(drives['items'])))
        self.assertIn('items are not strictly ordered', conf.check_page(self.profile, 'drives', reversed_page, vehicle_id=vehicle))
        self.assertIn('item lies outside the requested window', conf.check_page(self.profile, 'drives', drives, vehicle_id=vehicle, from_ms=I64_MAX - 1))
        self.assertIn('page exceeds the requested limit', conf.check_page(self.profile, 'drives', drives, vehicle_id=vehicle, limit=1))
        self.assertIn('item belongs to another vehicle', conf.check_page(self.profile, 'drives', drives, vehicle_id=str(uuid.uuid4())))
        first, second = {'items': drives['items'][:1]}, {'items': drives['items'][1:]}
        self.assertEqual(conf.check_sequence(self.profile, 'drives', [first, second]), [])
        self.assertEqual(conf.check_sequence(self.profile, 'drives', [first, first]), ['an item appears on two pages'])
        self.assertEqual(conf.check_sequence(self.profile, 'drives', [second, first]), ['pages are not continuous in order'])


# ---------------------------------------------------------------------------
# Rotation, revocation and errors
# ---------------------------------------------------------------------------
class RotationTests(Scratch):
    def test_rotation_grace_returns_the_same_successor(self):
        fixture = example('rotation-grace')
        self.assertEqual(fixture['lost_response_retry'], {'request': 'POST /v1/device/rotate', 'status': 200, 'returns': 'same_successor'})
        self.assertEqual(fixture['after_grace'], {'status': 401})
        self.assertEqual(fixture['old_bearer_valid_until_ms'], fixture['rotation_at_ms'] + fixture['grace_duration_ms'])
        validator = conf.Profile(PROFILE).reference_validator('rotation-grace.schema.json#/$defs/fixture')
        self.assertTrue(validator.is_valid(fixture))
        for retry in ({'request': 'POST /v1/device/rotate', 'status': 200, 'returns': 'new_successor'},
                      {'request': 'POST /v1/device/rotate', 'status': 401, 'returns': 'same_successor'},
                      {'request': 'POST /v1/device/rotate', 'status': 200},
                      {'request': 'POST /v1/device/rotate', 'status': 200, 'returns': 'same_successor', 'extra': 1},
                      {'request': 'POST /v1/device/revoke', 'status': 200, 'returns': 'same_successor'}):
            with self.subTest(retry=retry):
                self.assertFalse(validator.is_valid(dict(fixture, lost_response_retry=retry)))
        self.assertFalse(validator.is_valid(dict(fixture, grace_duration_ms=3600000)))
        self.assertFalse(validator.is_valid(dict(fixture, after_grace={'status': 200})))

    def test_the_rotation_sentence_is_the_only_retry_rule(self):
        rotation = load('profile.json')['pairing']['rotation']
        self.assertEqual(rotation['lost_response_retry'], SENTENCE)
        root = self.scratch_profile()
        self.edit(root, 'profile.json', lambda document: document['pairing']['rotation'].update(lost_response_retry='A retry issues a new bearer.'))
        results = {item['case_id']: item for item in conf.check_offline(root)}
        self.assertEqual(results['offline/rotation-and-errors']['status'], 'failed')

    def test_the_lost_response_retry_case_expects_200_and_the_same_successor(self):
        cases = {case['id']: case for case in load('cases.json')['cases']}
        for case_id in ('rotation-lost-response-retry', 'rotation-grace-same-successor'):
            self.assertEqual((cases[case_id]['status'], cases[case_id]['returns']), (200, 'same_successor'))
        self.assertEqual(cases['rotation-after-grace-401']['status'], 401)

    def test_claim_and_rotate_responses(self):
        validator = conf.Profile(PROFILE).reference_validator('auth.schema.json#/$defs/claim')
        self.assertTrue(validator.is_valid(example('claim')))
        self.assertTrue(validator.is_valid(example('rotate-max-lifetime')))
        self.assertTrue(validator.is_valid(dict(example('claim'), future_field=1)))
        self.assertFalse(validator.is_valid(dict(example('claim'), access_token='short')))
        closed = conf.Profile(PROFILE).validator('auth.schema.json', 'claim_request')
        self.assertTrue(closed.is_valid({'secret': 's', 'device_name': 'd'}))
        self.assertFalse(closed.is_valid({'secret': 's', 'device_name': 'd', 'extra': 1}))
        self.assertFalse(closed.is_valid({'secret': 's'}))

    def test_error_envelope_and_codes(self):
        profile = conf.Profile(PROFILE)
        validator = profile.validator('errors.schema.json')
        for name, code in (('error-drive-not-found', 'drive_not_found'), ('error-charge-not-found', 'charge_not_found')):
            self.assertEqual(example(name)['error']['code'], code)
            self.assertTrue(validator.is_valid(example(name)))
        self.assertTrue(validator.is_valid({'error': {'code': 'some_future_code', 'message': 'm'}}))
        for value in ({'error': {'code': 'Bad-Code', 'message': 'm'}}, {'error': {'code': 'a' * 65, 'message': 'm'}}, {'error': {'code': 'x'}}, {}):
            self.assertFalse(validator.is_valid(value))
        for code in load('profile.json')['error_codes']:
            self.assertTrue(validator.is_valid({'error': {'code': code, 'message': 'm'}}))

    def test_validate_raw_enforces_headers_and_bodies(self):
        profile = conf.Profile(PROFILE)
        body = json.dumps(example('drives')).encode()
        good = {'content-type': 'application/json', 'etag': '"' + hashlib.sha256(body).hexdigest() + '"', 'cache-control': 'no-store'}
        self.assertEqual(conf.validate_raw(profile, 'drives', 200, good, body), [])
        self.assertEqual(conf.validate_raw(profile, 'drives', 304, {'etag': good['etag'], 'cache-control': 'no-store'}, b''), [])
        for label, headers in {'no etag': {k: v for k, v in good.items() if k != 'etag'}, 'weak etag': dict(good, etag='W/' + good['etag']),
                               'wrong etag': dict(good, etag='"' + '0' * 64 + '"'), 'cacheable': dict(good, **{'cache-control': 'max-age=60'}),
                               'wrong type': dict(good, **{'content-type': 'text/plain'})}.items():
            with self.subTest(label):
                self.assertTrue(conf.validate_raw(profile, 'drives', 200, headers, body))
        self.assertEqual(conf.validate_raw(profile, 'vehicles', 401, {'www-authenticate': 'Bearer'}, b''), [])
        self.assertTrue(conf.validate_raw(profile, 'vehicles', 401, {}, b''))
        self.assertTrue(conf.validate_raw(profile, 'vehicles', 401, {'www-authenticate': 'Bearer'}, b'x'))
        self.assertEqual(conf.validate_raw(profile, 'revoke', 204, {}, b''), [])
        self.assertTrue(conf.validate_raw(profile, 'revoke', 204, {}, b'{}'))
        self.assertEqual(conf.validate_raw(profile, 'revoke', 400, {}, b'anything'), [])
        self.assertTrue(conf.validate_raw(profile, 'revoke', 200, {}, b''))
        self.assertTrue(conf.validate_raw(profile, 'drive', 304, {}, b''))
        self.assertTrue(conf.validate_raw(profile, 'drives', 200, good, b'x' * 1048577))
        extra = json.dumps(dict(example('drives'), future=1)).encode()
        self.assertEqual(conf.validate_raw(profile, 'drives', 200, dict(good, etag='"' + hashlib.sha256(extra).hexdigest() + '"'), extra, strict=False), [])
        self.assertTrue(conf.validate_raw(profile, 'drives', 200, dict(good, etag='"' + hashlib.sha256(extra).hexdigest() + '"'), extra))
        error = json.dumps({'error': {'code': 'drive_not_found', 'message': 'm'}}).encode()
        self.assertEqual(conf.validate_raw(profile, 'drive', 404, {'content-type': 'application/json', 'cache-control': 'no-store'}, error), [])
        self.assertTrue(conf.validate_raw(profile, 'drive', 404, {'content-type': 'application/json'}, error))
        self.assertTrue(conf.validate_raw(profile, 'drives', 404, {'content-type': 'application/json', 'cache-control': 'no-store'}, error))

    def test_an_empty_503_is_valid_on_every_route_and_a_json_503_only_where_the_profile_allows_one(self):
        profile = conf.Profile(PROFILE)
        document = load('profile.json')
        json_headers = {'content-type': 'application/json', 'cache-control': 'no-store'}
        error = json.dumps({'error': {'code': 'service_unavailable', 'message': 'm'}}).encode()
        other_code = json.dumps({'error': {'code': 'vehicle_not_found', 'message': 'm'}}).encode()
        not_ready = json.dumps({'status': 'not_ready', 'reason': 'collector_stale'}).encode()
        for route in [*document['routes'], *document['companion_routes']]:
            kind = route['kind']
            with self.subTest(kind):
                # the handler time budget: an empty 503 on any route, including the hub-sync-v1 routes the profile only names
                self.assertEqual(conf.validate_raw(profile, kind, 503, {}, b''), [])
                self.assertTrue(conf.validate_raw(profile, kind, 503, {}, b'<html>overloaded</html>'))
                shape = SHAPES_503.get(kind, 'empty')
                if shape == 'empty_or_error':
                    self.assertEqual(conf.validate_raw(profile, kind, 503, json_headers, error), [])
                    self.assertTrue(conf.validate_raw(profile, kind, 503, json_headers, other_code))
                    self.assertTrue(conf.validate_raw(profile, kind, 503, {'content-type': 'application/json'}, error))
                    self.assertTrue(conf.validate_raw(profile, kind, 503, {'content-type': 'text/plain', 'cache-control': 'no-store'}, error))
                elif shape == 'empty':
                    self.assertTrue(conf.validate_raw(profile, kind, 503, json_headers, error))
        self.assertEqual(conf.validate_raw(profile, 'ready', 503, {'content-type': 'application/json'}, not_ready), [])
        self.assertTrue(conf.validate_raw(profile, 'ready', 503, {'content-type': 'application/json'}, b'{"status":"ready"}'))
        self.assertTrue(conf.validate_raw(profile, 'ready', 503, {'content-type': 'application/json'}, error))
        self.assertTrue(conf.validate_raw(profile, 'ready', 503, {}, not_ready))
        # a 503 that the profile does not describe is not accepted when the profile carries no timeout rule
        root = self.scratch_profile()
        self.edit(root, 'profile.json', lambda d: d['limits'].pop('handler_timeout_response'))
        self.assertEqual(conf.validate_raw(conf.Profile(root, verify=False), 'pack', 503, {}, b''), ['status not specified by profile'])
        self.assertEqual(conf.validate_raw(conf.Profile(root, verify=False), 'health', 503, {}, b''), [])  # health lists its own 503

    def test_validate_raw_names_the_defect_of_each_kind_of_response(self):
        profile = conf.Profile(PROFILE)
        body = json.dumps(example('drives')).encode()
        etag = '"' + hashlib.sha256(body).hexdigest() + '"'
        page = {'content-type': 'application/json', 'cache-control': 'no-store', 'etag': etag}
        self.assertEqual(conf.validate_raw(profile, 'no-such-route', 200, {}, b''), ['unknown route kind'])
        self.assertEqual(conf.validate_raw(profile, 'drives', 418, page, body), ['status not specified by profile'])
        # an ETag is checked on a 304 as well as on a 200
        for label, headers in {'missing': {'cache-control': 'no-store'}, 'weak': {'cache-control': 'no-store', 'etag': 'W/' + etag},
                               'short': {'cache-control': 'no-store', 'etag': '"abc"'}}.items():
            self.assertEqual(conf.validate_raw(profile, 'drives', 304, headers, b''), ['page ETag missing or not a quoted SHA-256'], label)
        self.assertEqual(conf.validate_raw(profile, 'drives', 304, {'etag': etag}, b''), ['page Cache-Control must be no-store'])
        self.assertEqual(conf.validate_raw(profile, 'drives', 200, dict(page, etag='"' + '0' * 64 + '"'), body), ['page ETag does not match the body'])
        self.assertEqual(conf.validate_raw(profile, 'drives', 304, {'etag': etag, 'cache-control': 'no-store'}, b'x'), ['expected empty response body'])
        # the text bodies of the claim route
        self.assertEqual(conf.validate_raw(profile, 'claim', 400, {'content-type': 'text/plain; charset=utf-8'}, b'bad request'), [])
        for headers, raw in (({'content-type': 'text/plain'}, b''), ({'content-type': 'application/json'}, b'bad request'), ({}, b'bad request')):
            self.assertEqual(conf.validate_raw(profile, 'claim', 400, headers, raw), ['expected extractor text error'])
        # readiness
        ready = {'content-type': 'application/json'}
        self.assertEqual(conf.validate_raw(profile, 'ready', 200, ready, b'{"status":"ready"}'), [])
        not_ready = json.dumps({'status': 'not_ready', 'reason': 'collector_stale'}).encode()
        self.assertEqual(conf.validate_raw(profile, 'ready', 200, ready, not_ready), ['readiness status mismatch'])  # 200 means ready
        self.assertEqual(conf.validate_raw(profile, 'ready', 503, ready, b'{"status":"ready"}'), ['readiness status mismatch'])
        # the pairing invitation
        invitation = example('invitation')
        self.assertEqual(conf.validate_raw(profile, 'invitation', 200, {}, json.dumps(invitation).encode()), [])
        del_secret = {key: value for key, value in invitation.items() if key != 'secret'}
        self.assertEqual(conf.validate_raw(profile, 'invitation', 200, {}, json.dumps(del_secret).encode()), ['body violates invitation schema'])
        for field, value in (('secret', '1' * 64), ('endpoint', 'https://other.example.invalid'), ('pairingId', str(uuid.uuid4())), ('tlsPin', '1' * 64)):
            self.assertEqual(conf.validate_raw(profile, 'invitation', 200, {}, json.dumps(dict(invitation, **{field: value})).encode()),
                             ['invitation URI does not match its fields'], field)
        self.assertEqual(conf.validate_raw(profile, 'invitation', 200, {}, b'not json'), ['invalid JSON response'])

    def test_check_discovery_reports_a_bad_capability_list_instead_of_raising(self):
        for capabilities in (None, 5, 'query.vehicles', {'query.vehicles': 1}, [1, 2], ['query.vehicles'] * 2):
            with self.subTest(capabilities=capabilities):
                problems = conf.check_discovery(dict(example('discovery'), capabilities=capabilities), profile_id=PROFILE_ID)
                self.assertEqual(len(problems), 1)
                self.assertNotIn('discovery lacks', problems[0])

    def test_verify_bundle_says_which_rule_a_profile_breaks(self):
        def remove_required(root):
            (root / 'sync-regression.json').unlink()
            reseal(root)

        def unindexed_example(root):
            self.edit(root, 'profile.json', lambda d: d['examples'].update(extra='resources.schema.json#/$defs/health'))

        cases = [
            ('profile is incomplete', remove_required),
            ('profile example is missing', unindexed_example),
            ('unsupported profile identity', lambda root: self.edit(root, 'profile.json', lambda d: d.update(profile_id='hub-http-v1@9.9.9'))),
            ('profile file digest mismatch', lambda root: (root / 'cases.json').write_bytes((root / 'cases.json').read_bytes() + b' ')),
            ('profile manifest does not list exactly the profile files', lambda root: (root / 'extra.json').write_text('{}')),
            ('profile manifest is invalid', lambda root: (root / 'SHA256SUMS').write_text('not a manifest\n')),
            ('profile manifest is invalid', lambda root: (root / 'SHA256SUMS').write_text((root / 'SHA256SUMS').read_text().rstrip('\n'))),  # no final newline
            ('profile manifest is unreadable', lambda root: (root / 'SHA256SUMS').unlink()),
            ('profile contains a symbolic link', lambda root: (root / 'link.json').symlink_to(root / 'profile.json'))]
        for message, mutate in cases:
            with self.subTest(message):
                root = self.scratch_profile()
                mutate(root)
                with self.assertRaisesRegex(conf.ContractError, '^' + re.escape(message) + '$'):
                    conf.verify_bundle(root)


# ---------------------------------------------------------------------------
# Cases and the offline conformance checks
# ---------------------------------------------------------------------------
class CaseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cases = load('cases.json')
        cls.routes = {route['kind'] for route in [*load('profile.json')['routes'], *load('profile.json')['companion_routes']]}

    def test_the_case_set_is_exactly_the_specified_one(self):
        self.assertEqual([case['id'] for case in self.cases['cases']], CASE_IDS)
        self.assertEqual(self.cases['profile_id'], PROFILE_ID)
        self.assertEqual(len(CASE_IDS), 50)

    def test_every_case_references_declared_routes_and_inputs(self):
        for case in self.cases['cases']:
            kinds = [case['kind'], *case.get('kinds', []), *[step['kind'] for step in case.get('steps', [])]]
            self.assertLessEqual(set(kinds), self.routes, case['id'])
            self.assertLessEqual(set(case['inputs']) | set(case.get('optional_inputs', [])), set(self.cases['inputs']), case['id'])
            self.assertIn(case['method'], ('GET', 'POST'))

    def test_cases_name_the_descriptor_inputs_they_need(self):
        by_id = {case['id']: case for case in self.cases['cases']}
        # bearer reads the target first: a vehicle or pack that does not exist would pass the comparison without proving any grant
        self.assertEqual(by_id['ungranted-vehicle-404']['inputs'], ['bearer', 'second_bearer', 'ungranted_vehicle_id'])
        self.assertEqual(by_id['ungranted-vehicles-list']['inputs'], ['bearer', 'second_bearer', 'ungranted_vehicle_id'])
        self.assertEqual(by_id['ungranted-pack-404']['inputs'], ['bearer', 'second_bearer', 'ungranted_pack_digest'])
        for case_id in ('ungranted-vehicle-404', 'ungranted-vehicles-list', 'ungranted-pack-404'):
            self.assertIn('otherwise not_run', by_id[case_id]['description'], case_id)
        self.assertIn('First bearer', by_id['ungranted-vehicle-404']['description'])
        self.assertIn('First bearer', by_id['ungranted-pack-404']['description'])
        inputs = self.cases['inputs']
        self.assertIn('granted to bearer and not granted to second_bearer', inputs['ungranted_vehicle_id'])
        self.assertIn('bearer can download', inputs['ungranted_pack_digest'])
        self.assertIn('descriptor attests', inputs['expired_grace_bearer'])
        self.assertIn('descriptor-attested', by_id['rotation-after-grace-401']['description'])
        self.assertEqual(by_id['cursor-window-binding']['kinds'], ['charges', 'drive_positions', 'charge_samples'])
        self.assertEqual(by_id['cursor-window-binding']['optional_inputs'], ['charge_id'])
        self.assertIn('disposable_bearer', by_id['revoke-then-401']['inputs'])
        self.assertIn('disposable_bearer', by_id['revoke-again-401']['inputs'])
        self.assertEqual(by_id['rotation-after-grace-401']['inputs'], ['expired_grace_bearer'])
        self.assertIn('other_drive_id', by_id['cursor-scope-binding']['inputs'])
        self.assertIn('other_vehicle_id', by_id['cursor-vehicle-binding']['inputs'])
        self.assertIn('drive_id', by_id['positions-keyset-three-pages']['inputs'])
        self.assertIn('charge_id', by_id['charge-samples-success']['inputs'])
        self.assertEqual(by_id['ungranted-pack-404']['body'], 'empty')
        self.assertEqual(by_id['drive-unknown']['error_code'], 'drive_not_found')
        self.assertEqual(by_id['charge-unknown']['error_code'], 'charge_not_found')
        self.assertEqual([step['status'] for step in by_id['revoke-then-401']['steps']], [204, 401, 401, 401, 401, 401])
        self.assertEqual(by_id['revoke-again-401']['status'], 401)
        for case_id in ('cursor-kind-binding', 'cursor-vehicle-binding', 'cursor-scope-binding', 'cursor-window-binding'):
            self.assertEqual((by_id[case_id]['status'], by_id[case_id]['error_code']), (400, 'invalid_cursor'))
        self.assertEqual(by_id['page-conditional']['status'], 304)
        self.assertEqual(sorted(by_id['page-conditional']['kinds']), sorted(PAGE_LIMITS))

    def test_the_malformed_identifier_case_names_every_route_that_takes_a_vehicle_id_or_a_pack_name_and_the_values_it_sends(self):
        case = {item['id']: item for item in self.cases['cases']}['malformed-identifier-404']
        self.assertEqual((case['kind'], case['status'], case['error_code'], case['inputs']), ('drives', 404, 'vehicle_not_found', ['bearer']))
        self.assertEqual(case['kinds'], ['current', 'drives', 'drive', 'drive_positions', 'charges', 'charge', 'charge_samples', 'sync_manifest', 'pack'])
        self.assertEqual(case['kinds'], list(IDENTIFIER_KINDS))
        # every route of the profile that takes a vehicle id or a pack name is listed (the sync no-op and the other sync routes are not checked)
        profile = load('profile.json')
        with_vehicle = {route['kind'] for route in [*profile['routes'], *profile['companion_routes']] if '{vehicle_id}' in route['path'] or '{object_name}' in route['path']}
        self.assertEqual(with_vehicle, set(IDENTIFIER_KINDS))
        self.assertEqual(conf.MALFORMED_SEGMENTS, ('abc', '%FF'))
        self.assertEqual(conf.NIL_VEHICLE, NIL_VEHICLE)
        for text in ('abc', '%FF', NIL_VEHICLE, 'without a valid bearer is 401', 'never 400', 'the whole case not_run',
                     # not "as an unknown vehicle": on the manifest a malformed id is an empty 404 and a bad drive or charge id has its own code
                     'as not found, after the bearer check, with the answer of the route'):
            self.assertIn(text, case['description'])
        self.assertNotIn('as an unknown vehicle', case['description'])

    def test_the_claim_case_sends_the_refused_device_names_before_the_claim_and_says_so(self):
        case = {item['id']: item for item in self.cases['cases']}['claim-success']
        self.assertEqual(conf.REFUSED_DEVICE_NAMES, CLAIM_REFUSED)  # the names the case sends are the ones written out in this module
        for label, name in CLAIM_REFUSED:
            self.assertTrue(refused_device_name(name), label)
        self.assertEqual(len({name for _, name in CLAIM_REFUSED}), len(CLAIM_REFUSED))
        for phrase in ('six device_name values a Hub must refuse (empty, 129 bytes, 65 two-byte characters, a newline, DEL and a C1 control character)',
                       'each 422 with a bounded text/plain body and consume nothing', 'then the claim itself is 200'):
            self.assertIn(phrase, case['description'])
        self.assertEqual(case['inputs'], ['invitation_path'])
        self.assertEqual((case['status'], case['kind']), (200, 'claim'))

    def test_destructive_cases_use_their_own_bearers(self):
        by_id = {case['id']: case for case in self.cases['cases']}
        for case_id in ('rotate-success', 'rotation-lost-response-retry', 'rotation-grace-same-successor', 'revoke-then-401', 'revoke-again-401'):
            self.assertTrue(by_id[case_id]['destructive'], case_id)
            self.assertNotIn('bearer', by_id[case_id]['inputs'], case_id)
        self.assertFalse(any(case.get('destructive') for case in self.cases['cases'] if 'bearer' in case['inputs']))

    def test_input_catalogue_matches_the_descriptor_kinds(self):
        self.assertEqual(set(self.cases['inputs']), set(conf.INPUT_KINDS))
        self.assertEqual(set(conf.REQUIRED_CASE_IDS), set(CASE_IDS))
        self.assertEqual(set(conf.EXECUTION_ORDER), set(CASE_IDS))
        self.assertEqual(len(conf.EXECUTION_ORDER), len(set(conf.EXECUTION_ORDER)))


class OfflineConformanceTests(Scratch):
    def test_offline_checks_pass(self):
        results = conf.check_offline(PROFILE)
        self.assertEqual([item['case_id'] for item in results], ['offline/' + name for name, _ in conf.OFFLINE_CHECKS])
        self.assertEqual([item for item in results if item['status'] != 'passed'], [])
        self.assertEqual(len(results), 11)

    def test_a_run_without_a_descriptor_reports_every_native_case_as_not_run(self):
        summary = conf.run(None)
        self.assertEqual((summary['runs'], summary['passed'], summary['failed'], summary['not_run']), (61, 11, 0, 50))
        self.assertFalse(summary['complete'])
        self.assertFalse(summary['native_requested'])
        self.assertEqual(summary['provenance'], 'offline-profile')
        native = [item for item in summary['results'] if item['scope'] == 'native']
        self.assertEqual([item['case_id'] for item in native], CASE_IDS)
        self.assertTrue(all(item == {'scope': 'native', 'case_id': item['case_id'], 'status': 'not_run', 'missing': ['descriptor']} for item in native))
        self.assertEqual(summary['profile_sha256'], hashlib.sha256((PROFILE / 'SHA256SUMS').read_bytes()).hexdigest())

    def test_main_prints_json_and_succeeds_offline(self):
        output, text = io.StringIO(), io.StringIO()
        # The native cases include destructive ones: a variable in the environment must never start them.
        with mock.patch.dict(os.environ, {'TESLATLAS_HUB_HTTP_12_CONFIG': '/nonexistent/synthetic-descriptor.json'}):
            with contextlib.redirect_stdout(output):
                code = conf.main(['--json'])
            with contextlib.redirect_stdout(text):
                conf.main([])
        self.assertEqual(code, 0)
        self.assertFalse(json.loads(output.getvalue())['native_requested'])
        self.assertNotIn('synthetic-descriptor', output.getvalue() + text.getvalue())
        summary = json.loads(output.getvalue())
        self.assertEqual((summary['profile_id'], summary['passed'], summary['failed'], summary['not_run']), (PROFILE_ID, 11, 0, 50))
        self.assertIn('NOT RUN hub-http-v1@1.2.0 charges-success: missing: descriptor', text.getvalue())
        self.assertIn('profiles=1 runs=61 passed=11 failed=0 not_run=50', text.getvalue())

    def test_each_offline_check_detects_the_defect_it_exists_for(self):
        """Every mutation must be caught by the check that exists for it, for the reason it exists: the detail is asserted too."""
        def drop_max_items(document):
            del document['$defs']['positions']['properties']['items']['maxItems']

        def drop_unobserved_max_items(document):  # an array the bounds table in the check never reads
            del document['properties']['api_versions']['maxItems']

        def extra_field_in_example(root):
            path = root / 'examples/drive.json'
            value = json.loads(path.read_text())
            value['undocumented_field'] = 1
            path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

        def missing_required(root):
            path = root / 'examples/charge.json'
            value = json.loads(path.read_text())
            del value['id']
            path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

        mutations = {
            'offline/max-items': [
                (lambda root: self.edit(root, 'resources.schema.json', drop_max_items), 'array schema without maxItems at resources.schema.json'),
                (lambda root: self.edit(root, 'discovery.schema.json', drop_unobserved_max_items), 'array schema without maxItems at discovery.schema.json/properties/api_versions')],
            'offline/examples-server-strict': [(lambda root: (extra_field_in_example(root), reseal(root)), 'example drive violates its server-strict schema')],
            'offline/examples': [(lambda root: (missing_required(root), reseal(root)), 'example charge violates its schema')],
            'offline/capabilities': [
                (lambda root: self.edit(root, 'examples/discovery.json', lambda d: d.update(capabilities=d['capabilities'][:-3])), 'discovery example discovery violates the capability rules'),
                (lambda root: self.edit(root, 'examples/discovery.json', lambda d: d['capabilities'].append('query.metadata')), 'discovery example discovery violates the capability rules')],
            'offline/cases': [(lambda root: self.edit(root, 'cases.json', lambda d: d['cases'].pop()), 'required cases are missing')],
            'offline/cursors': [(lambda root: self.edit(root, 'examples/charges.json', lambda d: d.update(next_cursor=example('drives')['next_cursor'])),
                                 'example charges cursor: next_cursor does not describe this page')],
            'offline/rotation-and-errors': [(lambda root: self.edit(root, 'examples/rotation-grace.json', lambda d: d['lost_response_retry'].update(returns='new_successor')),
                                             'rotation-grace example does not return the same successor')],
            'offline/field-semantics': [(lambda root: self.edit(root, 'field-semantics.json', lambda d: d['charge'].pop('cost')), 'field semantics differ from the charge schema')],
            'offline/openapi': [(lambda root: self.edit(root, 'openapi.json', lambda d: d['paths']['/v1/device/revoke']['post']['responses'].pop('204')),
                                 'OpenAPI operation differs from route revoke')],
            'offline/schemas': [(lambda root: self.edit(root, 'errors.schema.json', lambda d: d.update(type='no-such-type')), 'errors.schema.json is not a valid Draft 2020-12 schema')],
            'offline/bundle': [(lambda root: (root / 'cases.json').write_bytes((root / 'cases.json').read_bytes() + b' '), 'profile is missing, incomplete or altered')]}
        for check_id, variants in mutations.items():
            for index, (mutate, expected) in enumerate(variants):
                with self.subTest(check_id, variant=index):
                    root = self.scratch_profile()
                    mutate(root)
                    results = {item['case_id']: item for item in conf.check_offline(root)}
                    self.assertEqual(results[check_id]['status'], 'failed', results[check_id])
                    self.assertTrue(results[check_id]['detail'].startswith(expected), results[check_id]['detail'])
                    for item in results.values():
                        self.assertNotIn('Traceback', json.dumps(item))

    def test_each_offline_branch_has_a_defect_that_only_it_reports(self):
        """One defect per rule of the offline checks, each asserted by the detail it must give."""
        def profile_edit(change):
            return lambda root: self.edit(root, 'profile.json', change)

        def openapi_edit(change):
            return lambda root: self.edit(root, 'openapi.json', change)

        def cases_edit(change):
            return lambda root: self.edit(root, 'cases.json', change)

        def vehicles_schema(document):
            return document['paths']['/v1/vehicles']['get']['responses']['200']['content']['application/json']['schema']

        def drives_limit(document):
            return next(item for item in document['paths']['/v1/vehicles/{vehicle_id}/drives']['get']['parameters'] if item['name'] == 'limit')['schema']

        def extra_example(root):
            (root / 'examples/extra.json').write_text('{}\n')
            reseal(root)

        branches = [
            ('offline/schemas', 'id', lambda root: self.edit(root, 'cursor.schema.json', lambda d: d.update({'$id': 'urn:teslatlas:hub-http-v1:1.1.0:cursor'})),
             'cursor.schema.json has the wrong $schema or $id'),
            ('offline/schemas', 'dialect', lambda root: self.edit(root, 'cursor.schema.json', lambda d: d.update({'$schema': 'http://json-schema.org/draft-07/schema#'})),
             'cursor.schema.json has the wrong $schema or $id'),
            ('offline/openapi', 'version', openapi_edit(lambda d: d['info'].update(version='1.1.0')), 'openapi.json has the wrong version'),
            ('offline/openapi', 'document', openapi_edit(lambda d: d['info'].pop('title')), 'openapi.json is not a valid OpenAPI 3.1 document'),
            ('offline/openapi', 'extra operation', openapi_edit(lambda d: d['paths'].update({'/v1/extra': {'get': {'operationId': 'extra', 'responses': {'200': {'description': 'x'}}}}})),
             'OpenAPI operations differ from the profile routes'),
            ('offline/openapi', 'operation id', openapi_edit(lambda d: d['paths']['/v1/vehicles']['get'].update(operationId='other')), 'OpenAPI operation differs from route vehicles'),
            ('offline/openapi', 'security', openapi_edit(lambda d: d['paths']['/v1/vehicles']['get'].update(security=[])), 'OpenAPI security differs from route vehicles'),
            ('offline/openapi', 'limit', openapi_edit(lambda d: drives_limit(d).update(maximum=499)), 'OpenAPI limit differs from route drives'),
            ('offline/openapi', 'default limit', openapi_edit(lambda d: drives_limit(d).update(default=99)), 'OpenAPI limit differs from route drives'),
            # the OpenAPI validator resolves references in the places it knows; an extension object is where only these checks look
            ('offline/openapi', 'missing file', openapi_edit(lambda d: d.update({'x-note': {'$ref': 'nowhere.schema.json#/$defs/vehicles'}})), 'OpenAPI references a missing file'),
            ('offline/openapi', 'unresolved reference', openapi_edit(lambda d: d.update({'x-note': {'$ref': 'resources.schema.json#/$defs/nowhere'}})),
             'OpenAPI reference does not resolve'),
            ('offline/openapi', 'broken reference in a schema', openapi_edit(lambda d: vehicles_schema(d).update({'$ref': 'resources.schema.json#/$defs/nowhere'})),
             'openapi.json is not a valid OpenAPI 3.1 document'),
            ('offline/examples', 'index', extra_example, 'examples differ from the profile example index'),
            ('offline/max-items', 'bounds', profile_edit(lambda d: d['limits']['array_max_items'].update(drives=501)), 'array bounds differ from the profile limits'),
            ('offline/max-items', 'route limit', profile_edit(lambda d: next(r for r in d['routes'] if r['kind'] == 'drives')['limit'].update(max=499)),
             'page bound differs from the route limit'),
            ('offline/capabilities', 'grammar', profile_edit(lambda d: d['capabilities'].__setitem__(2, 'Query.Drives')), 'profile capabilities violate the token grammar'),
            ('offline/capabilities', 'advertisement rules', profile_edit(lambda d: d['capability_advertisement'].update(always=['query.vehicles'])),
             'capability advertisement rules differ from the capability list'),
            ('offline/capabilities', 'route capability', profile_edit(lambda d: next(r for r in d['routes'] if r['kind'] == 'drives').update(capability='query.nothing')),
             'route names an unknown capability'),
            ('offline/capabilities', 'unsigned key', lambda root: self.edit(root, 'examples/discovery-unsigned.json', lambda d: d.update(manifestPublicKey='0' * 64)),
             'the unsigned discovery example carries a signing key'),
            ('offline/cases', 'profile', cases_edit(lambda d: d.update(profile_id='hub-http-v1@1.1.0')), 'cases name another profile'),
            ('offline/cases', 'duplicate id', cases_edit(lambda d: d['cases'].append(dict(d['cases'][0]))), 'case identifiers are not unique'),
            ('offline/cases', 'input catalogue', cases_edit(lambda d: d['inputs'].update(invented_input='x')), 'descriptor input kinds differ from the case input catalogue'),
            ('offline/cases', 'route kind', cases_edit(lambda d: d['cases'][0].update(kind='nowhere')), 'case discovery-success references an undeclared route kind'),
            ('offline/cases', 'step kind', cases_edit(lambda d: next(c for c in d['cases'] if c['id'] == 'revoke-then-401')['steps'][1].update(kind='nowhere')),
             'case revoke-then-401 references an undeclared route kind'),
            ('offline/cases', 'unknown input', cases_edit(lambda d: d['cases'][0].update(inputs=['no_such_input'])), 'case discovery-success names an unknown input'),
            ('offline/cases', 'status', cases_edit(lambda d: d['cases'][0].update(status=418)), 'case discovery-success expects a status outside its route'),
            ('offline/cases', 'method', cases_edit(lambda d: d['cases'][0].update(method='POST')), 'case discovery-success uses the wrong method'),
            ('offline/cursors', 'separation', lambda root: self.edit(root, 'cursor.schema.json', lambda d: d['$defs'].update(
                tqp1={'type': 'string'}, tqd1={'type': 'string', 'pattern': '^never$'})), 'cursor grammars do not separate tqp1 from tqd1'),
            ('offline/cursors', 'own form', lambda root: self.edit(root, 'cursor.schema.json', lambda d: d['$defs'].update(tqd1={'type': 'string', 'pattern': '^never$'})),
             'cursor grammars reject their own form'),
            ('offline/cursors', 'example grammar', lambda root: self.edit(root, 'examples/drives.json', lambda d: d.update(next_cursor='tqp1.zz')),
             'example drives cursor violates tqp1'),
            ('offline/rotation-and-errors', 'after grace', profile_edit(lambda d: d['pairing']['rotation'].update(after_grace={'status': 200})),
             'rotation semantics differ from the contract'),
            ('offline/rotation-and-errors', 'validity text', profile_edit(lambda d: d['pairing']['rotation'].update(old_bearer_validity='The prior bearer stays valid.')),
             'rotation semantics differ from the contract'),
            ('offline/rotation-and-errors', 'window', lambda root: self.edit(root, 'examples/rotation-grace.json', lambda d: d.update(old_bearer_valid_until_ms=d['old_bearer_valid_until_ms'] + 1)),
             'rotation-grace example window is inconsistent'),
            ('offline/rotation-and-errors', 'route codes', profile_edit(lambda d: d['error_codes'].remove('drive_not_found')), 'route error codes are not declared'),
            ('offline/rotation-and-errors', 'error example', lambda root: self.edit(root, 'examples/error-drive-not-found.json', lambda d: d['error'].update(code='made_up')),
             'error example uses an undeclared code'),
            ('offline/rotation-and-errors', 'status', profile_edit(lambda d: d.update(status='draft')), 'profile identity fields are wrong'),
            ('offline/rotation-and-errors', 'previous profile', profile_edit(lambda d: d.update(previous_profile='hub-http-v1@1.0.0')), 'profile identity fields are wrong'),
            ('offline/field-semantics', 'extra field', lambda root: self.edit(root, 'field-semantics.json', lambda d: d['drive'].update(invented_field={})),
             'field semantics differ from the drive schema'),
        ]
        for check_id, label, mutate, expected in branches:
            with self.subTest(check_id, branch=label):
                root = self.scratch_profile()
                mutate(root)
                results = {item['case_id']: item for item in conf.check_offline(root)}
                self.assertEqual(results[check_id]['status'], 'failed', results[check_id])
                self.assertEqual(results[check_id]['detail'], expected)

    def test_the_offline_runner_reports_each_kind_of_trouble_with_a_fixed_label(self):
        secret = 'synthetic-' + token('offline secret')

        def boom(profile):
            raise RuntimeError(secret)

        def unreadable(profile):
            raise OSError(secret)

        def skipped(profile):
            raise conf.NotRun('a-tool')

        checks = (('boom', boom), ('unreadable', unreadable), ('skipped', skipped), ('fine', lambda profile: None))
        with mock.patch.object(conf, 'OFFLINE_CHECKS', checks):
            results = conf.check_offline(PROFILE)
        self.assertEqual(results, [
            {'scope': 'offline', 'case_id': 'offline/boom', 'status': 'failed', 'detail': 'check raised an unexpected error'},
            {'scope': 'offline', 'case_id': 'offline/unreadable', 'status': 'failed', 'detail': 'check could not read the profile'},
            {'scope': 'offline', 'case_id': 'offline/skipped', 'status': 'not_run', 'missing': ['a-tool']},
            {'scope': 'offline', 'case_id': 'offline/fine', 'status': 'passed'}])
        self.assertNotIn(secret, json.dumps(results))

    def test_the_runner_runs_the_offline_profile_checks(self):
        result = run_script(ROOT / 'conformance/runner.py', '--profile', PROFILE_ID, '--json')
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        summary = json.loads(result.stdout)
        self.assertEqual((summary['profile_id'], summary['passed'], summary['failed'], summary['not_run']), (PROFILE_ID, 11, 0, 50))
        self.assertFalse(summary['complete'])

    def test_the_runner_refuses_to_mix_profiles_or_take_an_adapter(self):
        for arguments in (('--profile', PROFILE_ID, '--profile', 'hub-http-v1@1.1.0'),
                          ('--profile', PROFILE_ID, '--adapter', str(ROOT / 'conformance/adapters/actual-hub'))):
            with self.subTest(arguments):
                result = run_script(ROOT / 'conformance/runner.py', *arguments, '--json')
                self.assertEqual(result.returncode, 2, result.stdout.decode())
                self.assertIn(b'"error"', result.stdout)

    def test_a_bad_descriptor_path_fails_without_echoing_it(self):
        secret_path = '/nonexistent/synthetic-secret-descriptor.json'
        result = run_script(ROOT / 'conformance/runner.py', '--profile', PROFILE_ID, '--config', secret_path, '--json')
        self.assertEqual(result.returncode, 1)
        self.assertNotIn(b'synthetic-secret-descriptor', result.stdout + result.stderr)
        summary = json.loads(result.stdout)
        self.assertEqual(summary['failed'], 1)
        self.assertEqual(summary['not_run'], 50)
        failed = [item for item in summary['results'] if item['status'] == 'failed']
        self.assertEqual(failed[0]['case_id'], 'native/descriptor')


# ---------------------------------------------------------------------------
# Native checks against an in-memory Hub
# ---------------------------------------------------------------------------
VEHICLE_MAIN = '22222222-2222-4222-8222-222222222221'
VEHICLE_OTHER = '22222222-2222-4222-8222-222222222222'
VEHICLE_EMPTY = '22222222-2222-4222-8222-222222222223'
VEHICLE_HIDDEN = '22222222-2222-4222-8222-222222222224'
PAIRING_ID = '33333333-3333-4333-8333-333333333333'
T0 = 1788565900000
GRACE_MS = 86400000
CURSOR_KINDS = {'drives': 1, 'charges': 2, 'drive_positions': 3, 'charge_samples': 4}
NIL_VEHICLE = '00000000-0000-0000-0000-000000000000'
UUID_TEXT = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}')
# every route kind that takes a vehicle id or a pack name, in the order the malformed-identifier case reaches them
IDENTIFIER_KINDS = ('current', 'drives', 'drive', 'drive_positions', 'charges', 'charge', 'charge_samples', 'sync_manifest', 'pack')
# the device_name values the claim case sends before the real claim, written out here: a Hub must answer each with 422 and consume nothing
CLAIM_REFUSED = (('empty', ''), ('129 bytes', 'x' * 129), ('65 two-byte characters', '\u00e9' * 65), ('newline', 'a\nb'), ('DEL', 'a\x7fb'), ('C1 control character', 'a\u0085b'))


def token(label):
    return hashlib.sha256(label.encode()).hexdigest()


def refused_device_name(name):
    """The device_name rule of the contract, written out here independently of the generator and of the conformance module:
    1 to 128 UTF-8 bytes and no control character (the Cc category, which is what the Hub's char::is_control tests)."""
    return not isinstance(name, str) or not 1 <= len(name.encode('utf-8')) <= 128 or any(unicodedata.category(char) == 'Cc' for char in name)


class Device:
    def __init__(self, label, grants):
        self.device_id = str(uuid.UUID(hashlib.sha256(('device ' + label).encode()).hexdigest()[:32]))
        self.token, self.grants, self.grace, self.revoked = token(label), set(grants), {}, False


class FakeHub:
    """A small Hub that follows the 1.2.0 contract. `quirks` switch single behaviours off to prove the checks bite.

    A quirk name alone breaks every route it can apply to; `name:kind` breaks one route kind only.
    """

    def __init__(self, quirks=()):
        self.quirks = set(quirks)
        self.now = T0 + 1000
        self.observed = T0
        self.claimed = False
        self.devices = {
            'bearer': Device('bearer', {VEHICLE_MAIN, VEHICLE_OTHER, VEHICLE_EMPTY, VEHICLE_HIDDEN}),
            'second_bearer': Device('second_bearer', {VEHICLE_OTHER}),
            'disposable_bearer': Device('disposable_bearer', {VEHICLE_MAIN}),
            'rotation_bearer': Device('rotation_bearer', {VEHICLE_MAIN})}
        if 'disposable_unusable' in self.quirks:
            self.devices['disposable_bearer'].revoked = True
        self.vehicles = {VEHICLE_MAIN, VEHICLE_OTHER, VEHICLE_EMPTY, VEHICLE_HIDDEN}
        self.secrets = {name: device.token for name, device in self.devices.items()}
        self.secrets['invitation_secret'] = token('invitation secret')
        self.minted = []
        self.drives = []
        self.positions = {}
        self.charges = []
        self.samples = {}
        for index in range(1, 6):
            drive = example('drive')
            drive.update(id=100 + index, vehicle_id=VEHICLE_MAIN, start_date_ms=T0 + index * 1000000, end_date_ms=T0 + index * 1000000 + 60000)
            self.drives.append(drive)
            self.positions[drive['id']] = []
            for step in range(1, 5):
                position = copy.deepcopy(example('positions')['items'][0])
                position.update(id=drive['id'] * 10 + step, drive_id=drive['id'], date_ms=drive['start_date_ms'] + step * 1000)
                self.positions[drive['id']].append(position)
        for index in range(1, 4):
            charge = example('charge')
            charge.update(id=200 + index, vehicle_id=VEHICLE_MAIN, start_date_ms=T0 + index * 2000000, end_date_ms=T0 + index * 2000000 + 90000)
            self.charges.append(charge)
            self.samples[charge['id']] = []
            for step in range(1, 5):
                sample = copy.deepcopy(example('charge-samples')['items'][0])
                sample.update(id=charge['id'] * 10 + step, charge_id=charge['id'], timestamp_ms=charge['start_date_ms'] + step * 60000)
                self.samples[charge['id']].append(sample)

    def quirk(self, name, kind=None):
        """True when `name` is set for every route, or `name:kind` for this one."""
        return name in self.quirks or (kind is not None and name + ':' + kind in self.quirks)

    # -- responses ---------------------------------------------------------
    @staticmethod
    def empty(status, **headers):
        return conf.Response(status, {key.replace('_', '-'): value for key, value in headers.items()}, b'')

    def unauthorized(self):
        headers = {} if 'no_challenge' in self.quirks else {'www-authenticate': 'Bearer'}
        return conf.Response(401, headers, b'')

    def json(self, status, value, *, etag=False, spaced=False):
        body = json.dumps(value, separators=(', ', ': ') if spaced else (',', ':')).encode()
        headers = {'content-type': 'application/json', 'cache-control': 'no-store'}
        if etag:
            headers['etag'] = '"' + ('0' * 64 if 'etag_wrong' in self.quirks else hashlib.sha256(body).hexdigest()) + '"'
        return conf.Response(status, headers, body)

    def error(self, status, code, message=None):
        return self.json(status, {'error': {'code': code, 'message': message or code.replace('_', ' ')}})

    # -- routes ----------------------------------------------------------------
    @staticmethod
    def route_kind(rest):
        """The route kind of the path after /v1/vehicles/{vehicle_id}."""
        if rest == ['current']:
            return 'current'
        if rest[0] == 'sync':
            return 'sync_manifest' if rest[1:] == ['manifest'] else 'sync'
        owner = {'drives': ('drives', 'drive', 'drive_positions'), 'charges': ('charges', 'charge', 'charge_samples')}.get(rest[0])
        return owner[len(rest) - 1] if owner and len(rest) <= 3 else None

    def kind_of(self, segments):
        if segments == ['v1', 'vehicles']:
            return 'vehicles'
        if len(segments) >= 4 and segments[:2] == ['v1', 'vehicles']:
            return self.route_kind(segments[3:])
        if len(segments) == 4 and segments[:3] == ['v1', 'packs', 'sha256']:
            return 'pack'
        return None

    @staticmethod
    def undecodable(raw_segment):
        """True when the percent-decoding of a raw path segment is not valid UTF-8 (the Hub's framework answers such a segment 400 text/plain)."""
        try:
            urllib.parse.unquote_to_bytes(raw_segment).decode('utf-8')
        except UnicodeDecodeError:
            return True
        return False

    @staticmethod
    def invalid_utf8_response():
        return conf.Response(400, {'content-type': 'text/plain; charset=utf-8'}, b'Invalid URL: Invalid UTF-8 in `vehicle_id`')

    # -- authentication ------------------------------------------------------
    def authenticate(self, bearer, kind=None):
        for device in self.devices.values():
            if device.revoked and 'revoke_noop' not in self.quirks and not self.quirk('revoked_reads', kind):
                continue
            if bearer == device.token:
                return device
            grace = device.grace.get(bearer)
            if grace is not None and self.now < grace[1]:
                return device
        return None

    # -- entry point ---------------------------------------------------------
    def exchange(self, method, path, *, bearer=None, body=None, headers=None):
        headers = {key.lower(): value for key, value in (headers or {}).items()}
        parts = urllib.parse.urlsplit(path)
        segments = [urllib.parse.unquote(item) for item in parts.path.split('/')[1:]]
        query = urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
        if segments == ['.well-known', 'teslatlas-hub'] and method == 'GET':
            value = example('discovery-unsigned' if 'base_capabilities' in self.quirks else 'discovery')
            if 'discovery_old_profile' in self.quirks:
                value['profile'] = 'hub-http-v1@1.1.0'
            if 'extra_capability' in self.quirks:
                value['capabilities'] = value['capabilities'] + ['query.metadata', 'commands.send']
            if 'revoke_capability_only' in self.quirks:  # paired-device authentication on, no cursor key: no page route, no pack
                value = example('discovery-unsigned')
                value['capabilities'] = value['capabilities'] + ['auth.revoke']
            return self.json(200, value)
        if segments == ['healthz']:
            return self.json(200, example('health'))
        if segments == ['readyz']:
            return self.json(200, example('ready'))
        if len(segments) == 4 and segments[:2] == ['v1', 'pairings'] and segments[3] == 'claim' and method == 'POST':
            return self.claim(segments[2], body)
        if 'revoke_again_204' in self.quirks and segments == ['v1', 'device', 'revoke'] and method == 'POST' \
                and any(item.revoked and item.token == bearer for item in self.devices.values()):
            return self.empty(204)
        if 'rotate_after_grace' in self.quirks and segments == ['v1', 'device', 'rotate'] and bearer == token('expired grace'):
            return self.json(200, {'device_id': self.devices['bearer'].device_id, 'access_token': token('late successor'), 'expires_at_ms': self.now + 2592000000})
        if 'expired_grace_reads' in self.quirks and segments == ['v1', 'vehicles'] and bearer == token('expired grace'):
            return self.json(200, {'vehicles': []})
        kind = self.kind_of(segments)
        undecodable = any(self.undecodable(item) for item in parts.path.split('/')[1:])
        device = self.authenticate(bearer, kind)
        if device is None:
            if undecodable and self.quirk('undecodable_unauthenticated_400', kind):  # the path is read before the bearer is checked
                return self.invalid_utf8_response()
            return self.unauthorized()
        if undecodable and self.quirk('undecodable_400', kind):  # the answer of the Hub's framework for any segment that is not UTF-8
            return self.invalid_utf8_response()
        if segments == ['v1', 'device', 'rotate'] and method == 'POST':
            if 'grace_rejects_rotate_retry' in self.quirks and bearer != device.token:
                return self.unauthorized()
            return self.rotate(device, bearer)
        if segments == ['v1', 'device', 'revoke'] and method == 'POST':
            if body:
                if 'revoke_over_limit_413' in self.quirks and len(body) > 4096:  # the default body extractor of the Hub's HTTP layer
                    return conf.Response(413, {'content-type': 'text/plain; charset=utf-8'}, b'Failed to buffer the request body: length limit exceeded')
                if 'revoke_body_revokes' in self.quirks:
                    device.revoked = True
                    return self.empty(204)
                if 'revoke_body_ignored' in self.quirks:
                    return self.empty(204)
                device.revoked = device.revoked or 'revoke_body_400_revokes' in self.quirks
                return conf.Response(400, {'content-type': 'text/plain'}, b'request body not allowed')
            if 'revoke_noop' not in self.quirks:
                device.revoked = True
            return self.empty(404 if 'revoke_answers_404' in self.quirks else 204)
        if segments == ['v1', 'vehicles']:
            granted = sorted(self.vehicles if 'list_leaks' in self.quirks else device.grants & self.vehicles)
            if 'list_omits_granted' in self.quirks:
                granted = [vehicle for vehicle in granted if vehicle != VEHICLE_MAIN]
            return self.json(200, {'vehicles': [{'vehicle_id': vehicle, 'display_name': None} for vehicle in granted]})
        if len(segments) == 4 and segments[:3] == ['v1', 'packs', 'sha256']:
            # Like the Hub, the route takes an object name: the digest plus .sqlite.zst. Any other name is an empty 404 before a lookup.
            if not segments[3].endswith('.sqlite.zst'):
                return self.error(404, 'vehicle_not_found') if self.quirk('malformed_vehicle_wrong_form', 'pack') else self.empty(404)
            digest = segments[3][:-len('.sqlite.zst')]
            if digest == token('pack'):  # the digest of a pack that only VEHICLE_HIDDEN owns, so only a device granted it can read it
                if VEHICLE_HIDDEN in device.grants or 'pack_streams_ungranted' in self.quirks:
                    return conf.Response(200, {'content-type': 'application/octet-stream'}, b'synthetic pack bytes')
                if 'pack_differs' in self.quirks:
                    return conf.Response(404, {'content-type': 'application/octet-stream'}, b'')
            return self.empty(404)
        if len(segments) >= 4 and segments[:2] == ['v1', 'vehicles']:
            return self.vehicle_route(device, segments, query, headers)
        return self.empty(404)

    def claim(self, pairing_id, body):
        try:
            request = json.loads(body)
        except (TypeError, ValueError):
            return conf.Response(400, {'content-type': 'text/plain'}, b'invalid json')
        name = request.get('device_name')
        if refused_device_name(name) and 'claim_name_unchecked' not in self.quirks and 'claim_name_accepted=' + ascii(name) not in self.quirks:
            if 'claim_name_503' in self.quirks:  # what the Hub answers today: the store refuses the name and the handler maps the error to an empty 503
                return self.empty(503)
            self.claimed = self.claimed or 'claim_name_consumes' in self.quirks  # a refusal that still burned the invitation
            if 'claim_name_422_json' in self.quirks:
                return self.error(422, 'invalid_query')
            if 'claim_name_422_empty' in self.quirks:
                return self.empty(422)
            return conf.Response(422, {'content-type': 'text/plain; charset=utf-8'}, b'Failed to deserialize the JSON body into the target type: device_name')
        replay = self.claimed and 'claim_replayable' not in self.quirks
        if pairing_id != PAIRING_ID or replay or request.get('secret') != self.secrets['invitation_secret']:
            return self.empty(401)
        self.claimed = True
        device = Device('claimed', {VEHICLE_MAIN})
        self.devices['claimed'] = device
        self.secrets['claimed'] = device.token
        return self.json(200, {'device_id': device.device_id, 'access_token': device.token, 'expires_at_ms': self.now + 2592000000})

    def rotate(self, device, bearer):
        if bearer == device.token:
            if 'rotate_same_token' in self.quirks:
                return self.json(200, {'device_id': device.device_id, 'access_token': bearer, 'expires_at_ms': self.now + 2592000000})
            successor = token('successor of ' + bearer)
            if 'no_grace' not in self.quirks:
                device.grace[bearer] = (successor, self.now + GRACE_MS)
            if 'successor_rejected' not in self.quirks:
                device.token = successor
            self.secrets['successor of ' + bearer] = successor
            return self.json(200, {'device_id': device.device_id, 'access_token': successor, 'expires_at_ms': self.now + 2592000000})
        successor = device.grace[bearer][0]
        if 'retry_new_successor' in self.quirks:
            successor = token('second successor of ' + bearer)
            self.secrets['second successor of ' + bearer] = successor
            device.token = successor
        value = {'device_id': device.device_id, 'access_token': successor, 'expires_at_ms': self.now + 2592000000}
        if 'retry_other_device' in self.quirks:
            value['device_id'] = str(uuid.UUID(hashlib.sha256(b'another device').hexdigest()[:32]))
        if 'retry_with_reason' in self.quirks:
            value['reason'] = 'max_lifetime'
        return self.json(200, value)

    # -- vehicle routes ----------------------------------------------------------
    def vehicle_route(self, device, segments, query, headers):
        vehicle = segments[2]
        rest = segments[3:]
        kind = self.route_kind(rest)
        known = vehicle in self.vehicles
        granted = known and vehicle in device.grants
        json_route = rest[0] in ('drives', 'charges')
        malformed = UUID_TEXT.fullmatch(vehicle) is None
        if vehicle == NIL_VEHICLE and self.quirk('nil_vehicle_503', kind):  # the nil UUID is not a vehicle, but the Hub's active-vehicle check fails on it
            return self.empty(503) if kind == 'current' else self.error(503, 'service_unavailable')
        if not granted:
            if rest == ['current']:
                if 'ungranted_differs:current' in self.quirks and known:
                    return self.error(404, 'vehicle_not_found')
                if malformed and self.quirk('malformed_vehicle_wrong_form', kind):
                    return self.error(404, 'vehicle_not_found')
                return self.empty(404)
            if self.quirk('query_before_access', kind) and ('limit', '0') in query:
                return self.error(400, 'invalid_limit')
            if rest[0] == 'sync' and 'sync_ungranted_differs' in self.quirks and known:
                return self.empty(404)
            if malformed and (rest[0] == 'sync') != self.quirk('malformed_vehicle_wrong_form', kind):
                return self.empty(404)  # a vehicle id that is not a UUID is an empty 404 on a sync route (the Hub parses it before any lookup)
            if self.quirk('ungranted_differs', kind) and known:
                return self.error(404, 'vehicle_not_found', 'not granted to this device')
            if self.quirk('ungranted_wrong_code', kind):  # the same answer for an unknown vehicle, but the code of an item that is missing
                return self.error(404, 'drive_not_found' if kind in ('drive', 'drive_positions') else 'charge_not_found')
            return self.error(404, 'vehicle_not_found')  # sync routes keep hub-sync-v1's JSON vehicle_not_found for an unknown vehicle too
        if rest == ['current']:
            value = example('current')
            has_data = vehicle == VEHICLE_MAIN or (vehicle == VEHICLE_EMPTY and 'empty_current_has_data' in self.quirks)
            value.update(vehicle_id=VEHICLE_OTHER if 'current_wrong_vehicle' in self.quirks else vehicle, observed_at_ms=self.observed if has_data else None)
            return self.json(200, value)
        if rest[0] == 'sync':
            return self.empty(404)
        if not json_route:
            return self.empty(404)
        data = {'drives': self.drives, 'charges': self.charges} if vehicle == VEHICLE_MAIN else {'drives': [], 'charges': []}
        owner = 'drives' if rest[0] == 'drives' else 'charges'
        if len(rest) == 1:
            return self.page(owner, data[owner], vehicle, 0, query, headers)
        item_kind = 'drive' if owner == 'drives' else 'charge'
        identifier = rest[1]
        valid = re.fullmatch(r'[1-9][0-9]*', identifier) is not None and int(identifier) <= I64_MAX
        code = 'drive_not_found' if owner == 'drives' else 'charge_not_found'
        if not valid and self.quirk('invalid_id_400', item_kind):
            return self.error(400, 'invalid_query')
        parent = next((item for item in data[owner] if valid and item['id'] == int(identifier)), None)
        if parent is None:
            wrong = self.quirk('unknown_wrong_code', item_kind) or (len(rest) == 3 and self.quirk('nested_unknown_wrong_code', item_kind))
            response = self.error(404, 'vehicle_not_found' if wrong else code)
            if identifier == 'abc' and len(rest) == 2 and self.quirk('invalid_id_shape', item_kind):
                response.headers['content-type'] = 'application/json; charset=utf-8'
            return response
        if len(rest) == 2:
            if 'extra_field' in self.quirks:
                parent = dict(parent, undocumented_field=1)
            if self.quirk('item_differs', item_kind):
                parent = dict(parent, **{'start_address' if owner == 'drives' else 'address': 'Another place'})
            if self.quirk('item_wrong_id', item_kind):
                parent = dict(parent, id=parent['id'] + 1)
            if self.quirk('item_wrong_vehicle', item_kind):
                parent = dict(parent, vehicle_id=VEHICLE_OTHER)
            return self.json(200, parent)
        if rest[2:] == ['positions'] and owner == 'drives':
            return self.page('drive_positions', self.positions[parent['id']], vehicle, parent['id'], query, headers)
        if rest[2:] == ['samples'] and owner == 'charges':
            return self.page('charge_samples', self.samples[parent['id']], vehicle, parent['id'], query, headers)
        return self.empty(404)

    # -- pages -----------------------------------------------------------------
    def cursor_for(self, kind, vehicle, scope, from_ms, to_ms, last, keys):
        payload = bytes([1, CURSOR_KINDS[kind]]) + uuid.UUID(vehicle).bytes + struct.pack('>qqqqq', scope, from_ms, to_ms, last[keys[0]], last['id'])
        return 'tqp1.' + payload.hex() + '.' + hmac.new(KEY, DOMAIN + payload, hashlib.sha256).hexdigest()

    def read_cursor(self, text, kind, vehicle, scope, from_ms, to_ms):
        match = re.fullmatch(r'tqp1\.([0-9a-f]{116})\.([0-9a-f]{64})', text)
        if match is None:
            return None
        payload = bytes.fromhex(match.group(1))
        if not self.quirk('no_tag_check', kind) and not hmac.compare_digest(match.group(2), hmac.new(KEY, DOMAIN + payload, hashlib.sha256).hexdigest()):
            return None
        found_scope, found_from, found_to, key_a, key_b = struct.unpack('>qqqqq', payload[18:58])
        if payload[0] != 1 or (payload[1] != CURSOR_KINDS[kind] and not self.quirk('cursor_ignores_kind', kind)):
            return None
        if uuid.UUID(bytes=payload[2:18]) != uuid.UUID(vehicle) and not self.quirk('cursor_ignores_vehicle', kind):
            return None
        if found_scope != scope and not self.quirk('cursor_ignores_scope', kind):
            return None
        if (found_from, found_to) != (from_ms, to_ms) and not self.quirk('cursor_ignores_window', kind):
            return None
        return key_a, key_b

    def foreign(self, kind, item):
        """The same item attributed to something the request did not ask for."""
        if kind == 'drive_positions':
            return dict(item, drive_id=item['drive_id'] + 1000)
        if kind == 'charge_samples':
            return dict(item, charge_id=item['charge_id'] + 1000)
        return dict(item, vehicle_id=VEHICLE_OTHER)

    def page(self, kind, items, vehicle, scope, query, headers):
        keys, direction = PAGE_ORDER[kind]
        default, minimum, maximum = PAGE_LIMITS[kind]
        allowed = ('from_ms', 'to_ms', 'limit', 'cursor')
        params = {}
        for name, value in query:
            if name in params or (name not in allowed and not self.quirk('unknown_param_ignored', kind)):
                return self.error(400, 'invalid_query')
            if name in allowed:
                params[name] = value
        numbers = {}
        for name, fallback in (('from_ms', 0), ('to_ms', I64_MAX), ('limit', default)):
            text = params.get(name)
            low, high = (I64_MIN if self.quirk('limit_parsed_signed', kind) else 0, 2 ** 32 - 1) if name == 'limit' else (I64_MIN, I64_MAX)
            if text is None:
                numbers[name] = fallback
            elif re.fullmatch(r'-?[0-9]{1,19}', text) and low <= int(text) <= high:
                numbers[name] = int(text)
            elif 'malformed_accepted=' + name + '=' + text in self.quirks:  # one malformed value read as if the parameter were absent
                numbers[name] = fallback
            elif self.quirk('malformed_wrong_code', kind):  # a malformed value answered with the code of the parameter's range check
                return self.error(400, 'invalid_limit' if name == 'limit' else 'invalid_time_range')
            else:
                return self.error(400, 'invalid_query')
        from_ms, to_ms, limit = numbers['from_ms'], numbers['to_ms'], numbers['limit']
        if (from_ms < 0 and not self.quirk('negative_from_ok', kind)) or to_ms < 0 or (from_ms >= to_ms and not self.quirk('time_range_unchecked', kind)):
            return self.error(400, 'invalid_time_range')
        if limit == 0 and self.quirk('limit_zero_ok', kind):
            limit = default
        if limit > maximum and self.quirk('limit_clamped', kind):
            limit = maximum
        if not minimum <= limit <= maximum and not (kind == 'drive_positions' and 'positions_no_limit_check' in self.quirks):
            return self.error(400, 'invalid_limit')
        after = None
        if 'cursor' in params and not (self.quirk('malformed_cursor_ignored', kind) and not params['cursor'].startswith('tqp1.')):
            after = self.read_cursor(params['cursor'], kind, vehicle, scope, from_ms, to_ms)
            if after is None:
                return self.error(400, 'invalid_cursor')
        descending = direction == 'descending'
        key = lambda item: (item[keys[0]], item['id'])  # noqa: E731
        everything = self.quirk('empty_window_items', kind) and from_ms >= I64_MAX - 1000

        def inside(item):
            value = item[keys[0]]
            if everything:
                return True
            if not self.quirk('from_ignored', kind) and (value < from_ms or (value == from_ms and self.quirk('from_exclusive', kind))):
                return False
            return value < to_ms or (value == to_ms and self.quirk('to_inclusive', kind))

        ordered = sorted((item for item in items if inside(item)), key=key, reverse=descending != self.quirk('page_unordered', kind))
        if after is not None:
            if self.quirk('keyset_duplicates', kind):
                ordered = [item for item in ordered if (key(item) <= after if descending else key(item) >= after)]
            else:
                ordered = [item for item in ordered if (key(item) < after if descending else key(item) > after)]
            if self.quirk('keyset_skips', kind):
                ordered = ordered[1:]
        chosen = ordered if self.quirk('limit_ignored', kind) else ordered[:limit + 1 if self.quirk('page_over_limit', kind) else limit]
        cursor = self.cursor_for(kind, vehicle, scope, from_ms, to_ms, chosen[-1], keys) if chosen and len(ordered) > len(chosen) else None
        if self.quirk('empty_window_cursor', kind) and not chosen and items and from_ms >= I64_MAX - 1000:  # an empty page that still points at more
            cursor = self.cursor_for(kind, vehicle, scope, from_ms, to_ms, items[-1], keys)
        if 'extra_field' in self.quirks:
            chosen = [dict(item, undocumented_field=1) for item in chosen]
        if self.quirk('page_foreign_parent', kind) and chosen:
            chosen = chosen[:-1] + [self.foreign(kind, chosen[-1])]
        value = {'items': chosen, 'next_cursor': cursor}
        if self.quirk('page_oversize', kind):
            value['padding'] = 'x' * 1100000
        response = self.json(200, value, etag=True)
        header = headers.get('if-none-match')
        ignores = 'nested_ignores_validator' in self.quirks and kind in ('drive_positions', 'charge_samples')
        if header is not None and not ignores and (header == response.headers['etag'] or self.quirk('any_validator_304', kind)):
            etag = '"' + '1' * 64 + '"' if 'etag_304_differs' in self.quirks else response.headers['etag']
            return conf.Response(304, {'etag': etag, 'cache-control': 'no-store'}, b'')
        if header is not None and self.quirk('unstable_etag', kind):
            return self.json(200, value, etag=True, spaced=True)
        return response


def write_private(path, value):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'wb') as stream:
        stream.write(json.dumps(value).encode())


class NativeTests(unittest.TestCase):
    ENDPOINT = 'https://127.0.0.1:8443'

    @classmethod
    def setUpClass(cls):
        cls.profile = conf.Profile(PROFILE)

    def environment(self, hub, *, drop=(), receipt_mode='atomic', overrides=None):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        certificate = root / 'ca.pem'
        certificate.write_text('synthetic')
        invitation = dict(example('invitation'), pairingId=PAIRING_ID, secret=hub.secrets['invitation_secret'], endpoint=self.ENDPOINT,
                          expiresAtMs=int(time.time() * 1000) + 900000)
        invitation['pairingUri'] = 'teslatlas-hub://pair?' + urllib.parse.urlencode(
            {'endpoint': self.ENDPOINT, 'pairing_id': PAIRING_ID, 'secret': invitation['secret'], 'tls_pin': invitation['tlsPin']})
        write_private(root / 'invitation.json', invitation)
        inputs = {
            'bearer': hub.devices['bearer'].token, 'second_bearer': hub.devices['second_bearer'].token,
            'disposable_bearer': hub.devices['disposable_bearer'].token, 'rotation_bearer': hub.devices['rotation_bearer'].token,
            'expired_grace_bearer': token('expired grace'), 'vehicle_id': VEHICLE_MAIN, 'other_vehicle_id': VEHICLE_OTHER,
            'empty_vehicle_id': VEHICLE_EMPTY, 'ungranted_vehicle_id': VEHICLE_HIDDEN, 'drive_id': 105, 'other_drive_id': 104,
            'charge_id': 203, 'other_charge_id': 202, 'ungranted_pack_digest': token('pack'), 'invitation_path': str(root / 'invitation.json'),
            'update_request_path': str(root / 'update-request'), 'update_receipt_path': str(root / 'update-receipt')}
        inputs.update(overrides or {})
        for name in drop:
            del inputs[name]
        if 'expired_grace_bearer' in inputs:
            hub.secrets['expired grace'] = inputs['expired_grace_bearer']
        descriptor = {'endpoint': self.ENDPOINT, 'certificate_path': str(certificate), 'tls_pin': None, 'inputs': inputs}
        stop = threading.Event()
        self.addCleanup(stop.set)

        def harness():
            request, receipt = Path(inputs.get('update_request_path', root / 'x')), root / 'update-receipt'
            while not stop.is_set() and receipt_mode != 'never':
                if request.exists() and not receipt.exists():
                    if receipt_mode != 'stale':  # 'stale' is a harness that reports success without appending the observation
                        hub.observed += 1000
                    if receipt_mode in ('atomic', 'stale', 'failed'):  # a private temporary file renamed into place: the path never exists without content
                        partial = receipt.with_name('update-receipt.partial')
                        write_private(partial, {'status': 'failed' if receipt_mode == 'failed' else 'passed'})
                        os.replace(partial, receipt)
                    else:  # a harness that writes in place: the path exists, empty, before the content arrives
                        descriptor = os.open(receipt, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                        if receipt_mode == 'slow':
                            time.sleep(0.35)
                            with os.fdopen(descriptor, 'wb') as stream:
                                stream.write(json.dumps({'status': 'passed'}).encode())
                        else:
                            os.close(descriptor)
                    return
                time.sleep(0.005)

        thread = threading.Thread(target=harness, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 2)
        self.addCleanup(stop.set)
        return descriptor

    def start(self, hub, *, drop=(), timeout=None, clock=time.monotonic, receipt_mode='atomic', overrides=None):
        descriptor = self.environment(hub, drop=drop, receipt_mode=receipt_mode, overrides=overrides)
        patcher = mock.patch.object(conf, 'NativeClient', lambda *arguments, **options: hub)
        patcher.start()
        self.addCleanup(patcher.stop)
        return conf.NativeRun(self.profile, descriptor, timeout=timeout, clock=clock, client=hub)

    def run_cases(self, hub, case_ids, **options):
        run = self.start(hub, **options)
        for case_id in case_ids:
            run.execute(case_id)
        return run.results

    def assert_clean(self, hub, results):
        text = json.dumps(results) + json.dumps(conf.summarize(list(results.values()) if isinstance(results, dict) else results, 'x', native_requested=True))
        for name, secret in hub.secrets.items():
            self.assertNotIn(secret, text, name)
        self.assertNotIn(VEHICLE_HIDDEN, text)
        self.assertNotIn('Authorization', text)


class NativeFullRunTests(NativeTests):
    def test_every_case_passes_against_a_conforming_hub(self):
        hub = FakeHub()
        log = []
        exchange = hub.exchange

        def recording(method, path, **options):
            response = exchange(method, path, **options)
            log.append((method, urllib.parse.urlsplit(path).path.split('/')[-1], response.status))
            return response

        hub.exchange = recording
        results = self.start(hub).run()
        self.assertGreater(len(log), 120)
        for status in (200, 304, 400, 401, 404):
            self.assertIn(status, {item[2] for item in log})
        for last in ('positions', 'samples'):  # the error cases reach the nested page routes too
            self.assertIn(('GET', last, 400), log)
        self.assertEqual([item['case_id'] for item in results], CASE_IDS)
        self.assertEqual([item for item in results if item['status'] != 'passed'], [])
        self.assertTrue(all(item['scope'] == 'native' for item in results))
        self.assert_clean(hub, results)

    def test_a_conforming_hub_is_complete_through_the_summary(self):
        hub = FakeHub()
        results = conf.check_offline(PROFILE) + self.start(hub).run()
        summary = conf.summarize(results, 'a' * 64, native_requested=True)
        self.assertEqual((summary['runs'], summary['passed'], summary['failed'], summary['not_run']), (61, 61, 0, 0))
        self.assertTrue(summary['complete'])
        self.assertEqual(summary['provenance'], 'actual-hub')
        self.assert_clean(hub, results)

    def test_missing_inputs_are_not_run_and_named_never_passed(self):
        hub = FakeHub()
        keep = {'bearer', 'vehicle_id'}
        drop = [name for name in load('cases.json')['inputs'] if name not in keep]
        results = self.start(hub, drop=drop).run()
        cases = {case['id']: case for case in load('cases.json')['cases']}
        for item in results:
            missing = [name for name in cases[item['case_id']]['inputs'] if name not in keep]
            with self.subTest(item['case_id']):
                if missing:
                    self.assertEqual(item['status'], 'not_run')
                    self.assertEqual(item['missing'], missing)
                else:
                    self.assertIn(item['status'], ('passed', 'not_run'))
        by_id = {item['case_id']: item for item in results}
        self.assertEqual(by_id['ungranted-pack-404']['missing'], ['second_bearer', 'ungranted_pack_digest'])
        self.assertEqual(by_id['revoke-then-401']['missing'], ['disposable_bearer'])
        self.assertEqual(by_id['rotation-after-grace-401']['missing'], ['expired_grace_bearer'])
        self.assertEqual(by_id['claim-success']['missing'], ['invitation_path'])
        self.assertEqual(by_id['current-empty']['missing'], ['empty_vehicle_id'])
        self.assertEqual(by_id['drive-success']['missing'], ['drive_id'])
        self.assertEqual(by_id['drives-success']['status'], 'passed')
        self.assertEqual([item for item in results if item['status'] == 'failed'], [])
        summary = conf.summarize(results, 'a' * 64, native_requested=True)
        self.assertFalse(summary['complete'])
        self.assertGreater(summary['not_run'], 20)

    def test_data_that_is_absent_is_not_run_with_the_data_named(self):
        hub = FakeHub()
        hub.drives, hub.charges = [], []
        results = {item['case_id']: item for item in self.start(hub).run()}
        for case_id in ('drives-success', 'charges-success'):
            self.assertEqual(results[case_id]['status'], 'not_run', case_id)
            self.assertTrue(results[case_id]['missing'][0].startswith('data: '), case_id)

    def test_page_routes_that_are_not_advertised_are_not_run(self):
        hub = FakeHub({'base_capabilities'})
        results = {item['case_id']: item for item in self.start(hub).run()}
        self.assertEqual([item for item in results.values() if item['status'] == 'failed'], [])
        for case_id, capability in (('drives-success', 'query.drives'), ('charges-success', 'query.charges'), ('positions-success', 'query.positions'),
                                    ('charge-samples-success', 'query.charge.samples'), ('ungranted-pack-404', 'sync.packs')):
            self.assertEqual(results[case_id], {'scope': 'native', 'case_id': case_id, 'status': 'not_run', 'missing': ['capability ' + capability]})
        # a case that declares several routes names every capability it lacks, so one that cannot run all of them never runs part of them
        self.assertEqual(results['revoke-then-401'], {'scope': 'native', 'case_id': 'revoke-then-401', 'status': 'not_run',
                                                      'missing': ['capability auth.revoke', 'capability query.charges', 'capability sync.packs']})
        self.assertEqual(results['ungranted-vehicle-404']['missing'], ['capability query.drives', 'capability query.positions', 'capability query.charges',
                                                                      'capability query.charge.samples'])
        self.assertEqual(results['malformed-identifier-404']['missing'], ['capability query.drives', 'capability query.positions', 'capability query.charges',
                                                                         'capability query.charge.samples', 'capability sync.packs'])
        for case_id in ('discovery-success', 'discovery-profile', 'vehicles-success', 'current-success', 'rotate-success'):
            self.assertEqual(results[case_id]['status'], 'passed', case_id)

    def test_an_exhausted_time_budget_runs_nothing(self):
        hub = FakeHub()
        exchanges, original = [], hub.exchange
        hub.exchange = lambda *arguments, **options: (exchanges.append(arguments), original(*arguments, **options))[1]
        results = self.start(hub, timeout=0.0).run()
        self.assertEqual(exchanges, [])  # not even the read that precedes a comparison
        self.assertEqual({item['status'] for item in results}, {'not_run'})
        for item in results:
            self.assertTrue(item['missing'] == ['time budget'] or item['missing'][0].startswith('prerequisite '), item)
        self.assertEqual(hub.devices['rotation_bearer'].grace, {})
        self.assertFalse(hub.devices['disposable_bearer'].revoked)

    def test_a_receipt_that_exists_before_it_has_content_is_read_again(self):
        # A harness that writes the receipt in place makes the path exist before the content does; that is not a failure.
        results = self.run_cases(FakeHub(), ['discovery-success', 'later-current'], receipt_mode='slow')
        self.assertEqual(results['later-current'], {'scope': 'native', 'case_id': 'later-current', 'status': 'passed'})

    def test_a_receipt_that_never_gets_content_fails_as_unreadable(self):
        run = self.start(FakeHub(), receipt_mode='empty')
        run.execute('discovery-success')
        with mock.patch.object(conf, 'RECEIPT_WAIT_SECONDS', 0.5):  # the harness's own limit, not the run budget
            run.execute('later-current')
        self.assertEqual(run.results['later-current'], {'scope': 'native', 'case_id': 'later-current', 'status': 'failed',
                                                        'detail': 'update receipt is unreadable'})

    def test_a_receipt_that_never_arrives_fails_the_harness_not_the_budget(self):
        run = self.start(FakeHub(), receipt_mode='never')
        run.execute('discovery-success')
        with mock.patch.object(conf, 'RECEIPT_WAIT_SECONDS', 0.5):
            run.execute('later-current')
        self.assertEqual(run.results['later-current'], {'scope': 'native', 'case_id': 'later-current', 'status': 'failed',
                                                        'detail': 'the harness did not confirm the update'})

    def test_a_run_budget_that_ends_the_wait_is_not_run_never_a_failure_of_the_hub(self):
        # The harness is slow and the run budget is short: the Hub did nothing wrong, so the case did not run.
        for receipt_mode in ('never', 'empty'):
            with self.subTest(receipt_mode):
                run = self.start(FakeHub(), timeout=1.0, receipt_mode=receipt_mode)
                run.execute('discovery-success')
                self.assertEqual(run.results['discovery-success']['status'], 'passed')
                run.execute('later-current')
                self.assertEqual(run.results['later-current'], {'scope': 'native', 'case_id': 'later-current', 'status': 'not_run',
                                                                'missing': ['time budget']})

    def test_a_receipt_that_is_not_private_fails_at_once_as_unreadable(self):
        hub = FakeHub()
        run = self.start(hub, receipt_mode='empty')
        receipt = Path(run.inputs['update_receipt_path'])
        original = conf.hub_http.read_private

        def public_file(path):
            os.chmod(path, 0o644)
            return original(path)

        started = time.monotonic()
        with mock.patch.object(conf.hub_http, 'read_private', public_file):
            run.execute('discovery-success')
            run.execute('later-current')
        self.assertTrue(receipt.exists())
        self.assertEqual(run.results['later-current']['detail'], 'update receipt is unreadable')
        self.assertLess(time.monotonic() - started, 10)

    def test_dependent_cases_wait_for_their_prerequisite(self):
        hub = FakeHub()
        results = self.run_cases(hub, ['rotated-bearer-grace-valid', 'rotation-lost-response-retry', 'pairing-replay'])
        self.assertEqual(results['rotation-lost-response-retry'], {'scope': 'native', 'case_id': 'rotation-lost-response-retry', 'status': 'not_run',
                                                                    'missing': ['prerequisite rotate-success']})
        self.assertEqual(results['pairing-replay']['missing'], ['prerequisite claim-success'])
        self.assertEqual(hub.devices['rotation_bearer'].grace, {})


class NativeBreakageTests(NativeTests, Scratch):
    """Each variant of the fake Hub breaks one rule; the matching case must fail and carry no secret.

    EXPECTATIONS maps a quirk to (cases to run, case or cases that must fail). DETAILS pins the reason the first failing case gives.
    A quirk written `name:kind` breaks that route kind only, so a check that was narrowed to another route is caught too.
    """

    BASE = ('discovery-success',)
    PAGE_SUCCESS = {'drives': 'drives-success', 'drive_positions': 'positions-success', 'charges': 'charges-success', 'charge_samples': 'charge-samples-success'}
    SKIPPED = {'drives': 'drives: pages two at a time differ from the unpaged read', 'drive_positions': 'positions keyset: pages duplicate or miss an item',
               'charges': 'charges: single-item pages differ from the unpaged read', 'charge_samples': 'charge samples: single-item pages differ from the unpaged read'}
    EMPTY_WINDOW = {'drives': 'drives-success', 'drive_positions': 'positions-empty', 'charges': 'charges-empty', 'charge_samples': 'charge-samples-success'}
    # The case in which a Hub that ignores the limit is first caught for each page route (the case that reads it with a small limit).
    LIMITED = {'drives': 'scenario-page-2', 'drive_positions': 'positions-keyset-three-pages', 'charges': 'charges-success', 'charge_samples': 'charge-samples-success'}
    EXPECTATIONS = {
        'cursor_ignores_kind': (['cursor-kind-binding'], 'cursor-kind-binding'),
        'cursor_ignores_vehicle': (['cursor-vehicle-binding', 'invalid_cursor'], ('cursor-vehicle-binding', 'invalid_cursor')),
        'cursor_ignores_scope': (['cursor-scope-binding'], 'cursor-scope-binding'),
        'cursor_ignores_window': (['cursor-window-binding', 'cursor-time-binding'], ('cursor-window-binding', 'cursor-time-binding')),
        'no_tag_check': (['invalid_cursor'], 'invalid_cursor'),
        'ungranted_differs': (['ungranted-vehicle-404'], 'ungranted-vehicle-404'),
        'pack_differs': (['ungranted-pack-404'], 'ungranted-pack-404'),
        'list_leaks': (['ungranted-vehicles-list'], 'ungranted-vehicles-list'),
        'revoke_noop': (['revoke-then-401'], 'revoke-then-401'),
        'retry_new_successor': (['rotate-success', 'rotation-lost-response-retry'], 'rotation-lost-response-retry'),
        'extra_field': (['drives-success'], 'drives-success'),
        'etag_wrong': (['drive-conditional'], 'drive-conditional'),
        'invalid_id_400': (['drive-unknown'], 'drive-unknown'),
        'keyset_duplicates': (['positions-keyset-three-pages', 'scenario-page-2', 'charges-success'], ('positions-keyset-three-pages', 'scenario-page-2', 'charges-success')),
        'no_challenge': (['missing-bearer'], 'missing-bearer'),
        'positions_no_limit_check': (['invalid_limit'], 'invalid_limit'),
        'discovery_old_profile': (['discovery-profile'], 'discovery-profile'),
        'extra_capability': (['discovery-profile'], 'discovery-profile'),
        'nested_ignores_validator': (['page-conditional'], 'page-conditional'),
        'etag_304_differs': (['page-conditional'], 'page-conditional'),
        'revoke_again_204': (['revoke-then-401', 'revoke-again-401'], 'revoke-again-401'),
        'rotate_after_grace': (['rotation-after-grace-401'], 'rotation-after-grace-401'),
        'unknown_wrong_code': (['drive-unknown'], 'drive-unknown'),
        'query_before_access': (['ungranted-vehicle-404'], 'ungranted-vehicle-404'),
        'sync_ungranted_differs': (['ungranted-vehicle-404'], 'ungranted-vehicle-404'),
        'pack_streams_ungranted': (['ungranted-pack-404'], 'ungranted-pack-404'),
        # the plain reads
        'list_omits_granted': (['vehicles-success'], 'vehicles-success'),
        'current_wrong_vehicle': (['current-success'], 'current-success'),
        'empty_current_has_data': (['current-empty'], 'current-empty'),
        # revocation and pairing: each step of the cases must be able to fail on its own
        'revoke_answers_404': (['revoke-then-401'], 'revoke-then-401'),
        'revoke_body_revokes': (['revoke-then-401'], 'revoke-then-401'),
        'revoke_body_ignored': (['revoke-then-401'], 'revoke-then-401'),
        'revoke_body_400_revokes': (['revoke-then-401'], 'revoke-then-401'),
        'revoke_over_limit_413': (['revoke-then-401'], 'revoke-then-401'),
        'disposable_unusable': (['revoke-then-401'], 'revoke-then-401'),
        'revoked_reads:vehicles': (['revoke-then-401'], 'revoke-then-401'),
        'revoked_reads:current': (['revoke-then-401'], 'revoke-then-401'),
        'revoked_reads:charges': (['revoke-then-401'], 'revoke-then-401'),
        'revoked_reads:sync_manifest': (['revoke-then-401'], 'revoke-then-401'),
        'revoked_reads:pack': (['revoke-then-401'], 'revoke-then-401'),
        'claim_replayable': (['claim-success', 'pairing-replay'], 'pairing-replay'),
        'rotate_same_token': (['rotate-success'], 'rotate-success'),
        'no_grace': (['rotate-success', 'rotated-bearer-grace-valid', 'rotation-lost-response-retry'], ('rotated-bearer-grace-valid', 'rotation-lost-response-retry')),
        'grace_rejects_rotate_retry': (['rotate-success', 'rotation-lost-response-retry', 'rotation-grace-same-successor'],
                                       ('rotation-lost-response-retry', 'rotation-grace-same-successor')),
        'successor_rejected': (['rotate-success', 'rotated-bearer-valid'], 'rotated-bearer-valid'),
        'retry_other_device': (['rotate-success', 'rotation-lost-response-retry'], 'rotation-lost-response-retry'),
        'retry_with_reason': (['rotate-success', 'rotation-lost-response-retry'], 'rotation-lost-response-retry'),
        'expired_grace_reads': (['rotation-after-grace-401'], 'rotation-after-grace-401'),
        # items and their not-found shapes
        'item_differs:drive': (['drive-success'], 'drive-success'),
        'item_differs:charge': (['charge-success'], 'charge-success'),
        'item_wrong_id:drive': (['drive-success'], 'drive-success'),
        'item_wrong_id:charge': (['charge-success'], 'charge-success'),
        'item_wrong_vehicle:drive': (['drive-success'], 'drive-success'),
        'item_wrong_vehicle:charge': (['charge-success'], 'charge-success'),
        'invalid_id_shape:drive': (['drive-unknown'], 'drive-unknown'),
        'invalid_id_shape:charge': (['charge-unknown'], 'charge-unknown'),
        'unknown_wrong_code:charge': (['charge-unknown'], 'charge-unknown'),
        'nested_unknown_wrong_code:drive': (['drive-unknown'], 'drive-unknown'),
        'nested_unknown_wrong_code:charge': (['charge-unknown'], 'charge-unknown'),
        'invalid_id_400:charge': (['charge-unknown'], 'charge-unknown'),
        'ungranted_differs:current': (['ungranted-vehicle-404'], 'ungranted-vehicle-404'),
        'ungranted_differs:sync_manifest': (['ungranted-vehicle-404'], 'ungranted-vehicle-404'),
        # the device_name rule of the claim route: a refusal is 422 with a text body, and it consumes nothing
        'claim_name_unchecked': (['claim-success'], 'claim-success'),
        'claim_name_503': (['claim-success'], 'claim-success'),
        'claim_name_consumes': (['claim-success'], 'claim-success'),
        'claim_name_422_json': (['claim-success'], 'claim-success'),
        'claim_name_422_empty': (['claim-success'], 'claim-success')}
    # The reason a quirk must be caught for; the quirks not listed are held only to the fixed-label guard below.
    DETAILS = {
        'positions_no_limit_check': 'positions limit 0: expected HTTP 400, got 200',
        'discovery_old_profile': 'discovery names a different profile',
        'extra_capability': 'discovery advertises a token outside the profile',
        'nested_ignores_validator': 'expected HTTP 304, got 200',
        'etag_304_differs': 'the 304 does not carry the validator ETag',
        'revoke_again_204': 'second revoke: expected HTTP 401, got 204',
        'rotate_after_grace': 'rotate after the grace window: expected HTTP 401, got 200',
        'unknown_wrong_code': 'wrong not-found code',
        'query_before_access': 'expected HTTP 404, got 400',
        'sync_ungranted_differs': 'ungranted sync manifest: wrong not-found code',
        'pack_streams_ungranted': 'expected HTTP 404, got 200',
        'pack_differs': 'an ungranted digest differs from an unknown digest',
        'ungranted_differs': 'an ungranted vehicle differs from an unknown vehicle',
        'list_omits_granted': 'vehicles: the granted vehicle is not listed',
        'current_wrong_vehicle': 'current: vehicle identity mismatch',
        'empty_current_has_data': 'empty current: observed_at_ms is not null',
        'revoke_answers_404': 'revoke: expected HTTP 204, got 404',
        'revoke_body_revokes': 'revoke with a body: expected HTTP 400, got 204',
        'revoke_body_ignored': 'revoke with a body: expected HTTP 400, got 204',
        'revoke_body_400_revokes': 'bearer after a rejected revoke: expected HTTP 200, got 401',
        'revoke_over_limit_413': 'revoke with a body over the request limit: expected HTTP 400, got 413',
        'disposable_unusable': 'disposable bearer before revoke: expected HTTP 200, got 401',
        'revoked_reads:vehicles': 'vehicles after revoke: expected HTTP 401, got 200',
        'revoked_reads:current': 'current after revoke: expected HTTP 401, got 200',
        'revoked_reads:charges': 'a page after revoke: expected HTTP 401, got 200',
        'revoked_reads:sync_manifest': 'sync manifest after revoke: expected HTTP 401, got 404',
        'revoked_reads:pack': 'pack after revoke: expected HTTP 401, got 404',
        'claim_replayable': 'claim replay: expected HTTP 401, got 200',
        'rotate_same_token': 'rotate: the bearer was not replaced',
        'no_grace': 'prior bearer in the grace window: expected HTTP 200, got 401',
        'grace_rejects_rotate_retry': 'rotation retry: expected HTTP 200, got 401',
        'successor_rejected': 'successor bearer: expected HTTP 200, got 401',
        'retry_other_device': 'rotation retry: the retry did not return the same successor',
        'retry_with_reason': 'rotation retry: the retry did not return the same successor',
        'expired_grace_reads': 'vehicles after the grace window: expected HTTP 401, got 200',
        'item_differs:drive': 'drive: item differs from its listing entry',
        'item_differs:charge': 'charge: item differs from its listing entry',
        'item_wrong_id:drive': 'drive: identity mismatch',
        'item_wrong_id:charge': 'charge: identity mismatch',
        'item_wrong_vehicle:drive': 'drive: identity mismatch',
        'item_wrong_vehicle:charge': 'charge: identity mismatch',
        'invalid_id_shape:drive': 'drive: invalid identifiers return different shapes',
        'invalid_id_shape:charge': 'charge: invalid identifiers return different shapes',
        'unknown_wrong_code:charge': 'charge: wrong not-found code',
        'nested_unknown_wrong_code:drive': 'drive nested: wrong error code',
        'nested_unknown_wrong_code:charge': 'charge nested: wrong error code',
        'invalid_id_400:charge': 'expected HTTP 404, got 400',
        'ungranted_differs:current': 'current of an ungranted vehicle: expected empty response body',
        'ungranted_differs:sync_manifest': 'ungranted sync manifest: an ungranted vehicle differs from an unknown vehicle',
        'claim_name_unchecked': 'claim with a refused device_name (empty): expected HTTP 422, got 200',
        'claim_name_503': 'claim with a refused device_name (empty): expected HTTP 422, got 503',  # the answer of the current Hub
        'claim_name_consumes': 'claim: expected HTTP 200, got 401',
        'claim_name_422_json': 'claim with a refused device_name (empty): expected extractor text error',
        'claim_name_422_empty': 'claim with a refused device_name (empty): expected extractor text error'}

    @classmethod
    def per_route(cls):
        """One quirk per page route for every page-level rule: the success case of each route must catch it on its own."""
        scope = {'drives': 'another vehicle', 'charges': 'another vehicle', 'drive_positions': 'another parent', 'charge_samples': 'another parent'}
        for kind, success in cls.PAGE_SUCCESS.items():
            maximum = PAGE_LIMITS[kind][2]
            yield 'page_unordered:' + kind, ([success], success), 'items are not strictly ordered'
            yield 'page_foreign_parent:' + kind, ([success], success), 'item belongs to ' + scope[kind]
            yield 'from_exclusive:' + kind, ([success], success), 'from_ms is not inclusive'
            yield 'to_inclusive:' + kind, ([success], success), 'item lies outside the requested window'
            yield 'from_ignored:' + kind, ([success], success), 'item lies outside the requested window'
            yield 'page_over_limit:' + kind, ([cls.LIMITED[kind]], cls.LIMITED[kind]), 'page exceeds the requested limit'
            yield 'limit_ignored:' + kind, ([cls.LIMITED[kind]], cls.LIMITED[kind]), 'page exceeds the requested limit'
            yield 'page_oversize:' + kind, (['page-byte-cap'], 'page-byte-cap'), 'response exceeds byte limit'
            yield 'unknown_param_ignored:' + kind, (['invalid_query'], 'invalid_query'), kind + ' unknown parameter: expected HTTP 400, got 200'
            yield 'time_range_unchecked:' + kind, (['invalid_time_range'], 'invalid_time_range'), kind + ' time range: expected HTTP 400, got 200'
            yield 'negative_from_ok:' + kind, (['invalid_time_range'], 'invalid_time_range'), kind + ' time range: expected HTTP 400, got 200'
            yield 'limit_zero_ok:' + kind, (['invalid_limit'], 'invalid_limit'), kind + ' limit 0: expected HTTP 400, got 200'
            yield 'limit_clamped:' + kind, (['invalid_limit'], 'invalid_limit'), kind + ' limit ' + str(maximum + 1) + ': expected HTTP 400, got 200'
            yield 'no_tag_check:' + kind, (['invalid_cursor'], 'invalid_cursor'), kind + ' tampered cursor tag: expected HTTP 400, got 200'
            yield 'malformed_cursor_ignored:' + kind, (['invalid_cursor'], 'invalid_cursor'), kind + ' malformed cursor: expected HTTP 400, got 200'
            if kind == 'drives':  # the drives route has its own conditional case besides the shared one
                yield 'any_validator_304:' + kind, (['page-conditional', 'drive-conditional'], ('page-conditional', 'drive-conditional')), kind + ' repeat: expected HTTP 200, got 304'
            else:
                yield 'any_validator_304:' + kind, (['page-conditional'], 'page-conditional'), kind + ' repeat: expected HTTP 200, got 304'
            yield 'keyset_duplicates:' + kind, ([cls.LIMITED[kind]], cls.LIMITED[kind]), 'an item appears on two pages'
            yield 'keyset_skips:' + kind, ([cls.LIMITED[kind]], cls.LIMITED[kind]), cls.SKIPPED[kind]
            yield 'unstable_etag:' + kind, (['page-conditional'], 'page-conditional'), kind + ': the same read changed its ETag'
            yield 'query_before_access:' + kind, (['ungranted-vehicle-404'], 'ungranted-vehicle-404'), 'ungranted ' + kind + ': expected HTTP 404, got 400'
        for kind, case in cls.EMPTY_WINDOW.items():
            yield 'empty_window_items:' + kind, ([case], case), 'an empty window is not an empty page'
            yield 'empty_window_cursor:' + kind, ([case], case), 'an empty window is not an empty page'  # the other half of the rule: next_cursor is null
        case = 'malformed-identifier-404'
        json_routes = ('drives', 'drive', 'drive_positions', 'charges', 'charge', 'charge_samples')
        for kind in IDENTIFIER_KINDS:
            where = 'pack %FF' if kind == 'pack' else 'vehicle %FF on ' + kind
            yield 'undecodable_400:' + kind, ([case], case), where + ': expected HTTP 404, got 400'
            yield 'undecodable_unauthenticated_400:' + kind, ([case], case), 'without a bearer on ' + kind + ': expected HTTP 401, got 400'
            if kind == 'pack':
                wrong = 'pack abc: expected empty response body'
            else:
                wrong = 'vehicle abc on ' + kind + ': ' + {'current': 'expected empty response body', 'sync_manifest': 'expected an empty 404'}.get(kind, 'expected JSON content type')
            yield 'malformed_vehicle_wrong_form:' + kind, ([case], case), wrong
            if kind != 'pack':
                yield 'nil_vehicle_503:' + kind, ([case], case), 'vehicle ' + NIL_VEHICLE + ' on ' + kind + ': expected HTTP 404, got 503'
        assert set(json_routes) | {'current', 'sync_manifest', 'pack'} == set(IDENTIFIER_KINDS)
        for label, name in CLAIM_REFUSED:  # each refused name must be able to fail the claim case on its own
            yield 'claim_name_accepted=' + ascii(name), (['claim-success'], 'claim-success'), 'claim with a refused device_name (' + label + '): expected HTTP 422, got 200'
        for kind in ('drives', 'charges'):
            yield 'cursor_ignores_vehicle:' + kind, (['cursor-vehicle-binding'], 'cursor-vehicle-binding'), kind + ' cursor on another vehicle: expected HTTP 400, got 200'
        yield 'cursor_ignores_kind:drives', (['cursor-kind-binding'], 'cursor-kind-binding'), 'charges cursor on drives: expected HTTP 400, got 200'
        yield 'cursor_ignores_kind:charges', (['cursor-kind-binding'], 'cursor-kind-binding'), 'drives cursor on charges: expected HTTP 400, got 200'
        for kind, label in (('drive_positions', 'positions cursor of another drive'), ('charge_samples', 'samples cursor of another charge')):
            yield 'cursor_ignores_scope:' + kind, (['cursor-scope-binding'], 'cursor-scope-binding'), label + ': expected HTTP 400, got 200'
        yield 'cursor_ignores_window:drives', (['cursor-time-binding'], 'cursor-time-binding'), 'cursor window: expected HTTP 400, got 200'
        for kind in ('charges', 'drive_positions', 'charge_samples'):
            yield 'cursor_ignores_window:' + kind, (['cursor-window-binding'], 'cursor-window-binding'), kind + ' cursor window: expected HTTP 400, got 200'
        for kind in ('drive', 'drive_positions', 'charge', 'charge_samples'):
            yield 'ungranted_wrong_code:' + kind, (['ungranted-vehicle-404'], 'ungranted-vehicle-404'), 'ungranted ' + kind + ': wrong not-found code'
        for kind in ('drives', 'drive', 'drive_positions', 'charges', 'charge', 'charge_samples'):
            yield 'ungranted_differs:' + kind, (['ungranted-vehicle-404'], 'ungranted-vehicle-404'), 'ungranted ' + kind + ': an ungranted vehicle differs from an unknown vehicle'

    @classmethod
    def every_expectation(cls):
        table = {quirk: (cases, failing, cls.DETAILS.get(quirk)) for quirk, (cases, failing) in cls.EXPECTATIONS.items()}
        for quirk, (cases, failing), detail in cls.per_route():
            table[quirk] = (cases, failing, detail)
        return table

    def test_each_broken_rule_fails_its_case_without_leaking_a_secret(self):
        for quirk, (case_ids, failing, detail_text) in self.every_expectation().items():
            with self.subTest(quirk):
                hub = FakeHub({quirk})
                results = self.run_cases(hub, [*self.BASE, *case_ids])
                failing = (failing,) if isinstance(failing, str) else failing
                for case_id in failing:
                    self.assertEqual(results[case_id]['status'], 'failed', (case_id, results[case_id]))
                detail = results[failing[0]]['detail']
                # A deliberate label, not the fallback for an exception the case did not expect.
                for case_id in failing:
                    self.assertNotIn(results[case_id]['detail'], ('check raised an unexpected error', 'response could not be interpreted'))
                if detail_text is not None:
                    self.assertIn(detail_text, detail)
                self.assert_clean(hub, results)

    # -- checks that need a changed profile, a changed descriptor or a long history ------------------------------------
    def run_with_profile(self, hub, profile, case_ids, **options):
        descriptor = self.environment(hub, **options)
        patcher = mock.patch.object(conf, 'NativeClient', lambda *arguments, **keywords: hub)
        patcher.start()
        self.addCleanup(patcher.stop)
        run = conf.NativeRun(profile, descriptor, client=hub)
        for case_id in case_ids:
            run.execute(case_id)
        return run.results

    def test_a_page_read_without_a_limit_is_held_to_the_default_limit_of_the_profile(self):
        root = self.scratch_profile()
        self.edit(root, 'profile.json', lambda d: next(r for r in d['routes'] if r['kind'] == 'drives')['limit'].update(default=2))
        results = self.run_with_profile(FakeHub(), conf.Profile(root, verify=False), ['discovery-success', 'drives-success'])
        self.assertEqual(results['drives-success']['status'], 'failed')
        self.assertIn('page exceeds the requested limit', results['drives-success']['detail'])

    def test_the_claim_request_is_held_to_the_byte_limit_of_the_profile(self):
        root = self.scratch_profile()
        self.edit(root, 'profile.json', lambda d: d.update(max_claim_request_bytes=10))
        hub = FakeHub()
        results = self.run_with_profile(hub, conf.Profile(root, verify=False), ['claim-success'])
        self.assertEqual(results['claim-success'], {'scope': 'native', 'case_id': 'claim-success', 'status': 'failed',
                                                    'detail': 'claim request exceeds the profile byte limit'})
        self.assertFalse(hub.claimed)

    def test_a_bad_invitation_fails_the_claim_before_anything_is_sent(self):
        def invitation(hub, **changes):
            value = {**example('invitation'), 'pairingId': PAIRING_ID, 'secret': hub.secrets['invitation_secret'], 'endpoint': self.ENDPOINT,
                     'expiresAtMs': int(time.time() * 1000) + 900000, **changes}
            value['pairingUri'] = 'teslatlas-hub://pair?' + urllib.parse.urlencode(
                {'endpoint': value['endpoint'], 'pairing_id': value['pairingId'], 'secret': value['secret'], 'tls_pin': value['tlsPin']})
            return value

        def without_secret(hub):
            value = invitation(hub)
            del value['secret']
            return value

        cases = (
            ('does not match the schema', without_secret, 'invitation does not match its schema'),
            ('has expired', lambda hub: invitation(hub, expiresAtMs=1), 'invitation endpoint or expiry is invalid'),
            ('names another endpoint', lambda hub: invitation(hub, endpoint='https://127.0.0.1:9999'), 'invitation endpoint or expiry is invalid'))
        for label, build, detail in cases:
            with self.subTest(label):
                hub = FakeHub()
                temp = tempfile.TemporaryDirectory()
                self.addCleanup(temp.cleanup)
                path = Path(temp.name) / 'invitation.json'
                write_private(path, build(hub))
                exchanges = []
                original = hub.exchange
                hub.exchange = lambda *arguments, **options: (exchanges.append(arguments), original(*arguments, **options))[1]
                results = self.run_cases(hub, ['claim-success'], overrides={'invitation_path': str(path)})
                self.assertEqual(results['claim-success'], {'scope': 'native', 'case_id': 'claim-success', 'status': 'failed', 'detail': detail})
                self.assertEqual(exchanges, [])
                self.assertFalse(hub.claimed)
        results = self.run_cases(FakeHub(), ['claim-success'], overrides={'invitation_path': '/nonexistent/synthetic-invitation.json'})
        self.assertEqual(results['claim-success']['detail'], 'invitation file is unreadable')

    def test_a_harness_that_reports_failure_or_appends_nothing_fails_later_current(self):
        for mode, detail in (('failed', 'the harness did not commit the update'), ('stale', 'current did not advance after the update')):
            with self.subTest(mode):
                results = self.run_cases(FakeHub(), ['discovery-success', 'later-current'], receipt_mode=mode)
                self.assertEqual(results['later-current'], {'scope': 'native', 'case_id': 'later-current', 'status': 'failed', 'detail': detail})

    def test_the_maximum_limit_case_follows_the_cursor_across_pages_of_a_long_history(self):
        def long_history(quirks=()):
            hub = FakeHub(quirks)
            template = hub.drives[0]
            for index in range(6, 601):
                drive = dict(template, id=100 + index, start_date_ms=T0 + index * 1000000, end_date_ms=T0 + index * 1000000 + 60000)
                hub.drives.append(drive)
                hub.positions[drive['id']] = []
            return hub

        passing = self.run_cases(long_history(), ['discovery-success', 'page-byte-cap'])
        self.assertEqual(passing['page-byte-cap']['status'], 'passed', passing['page-byte-cap'])
        broken = self.run_cases(long_history({'keyset_duplicates:drives'}), ['discovery-success', 'page-byte-cap'])
        self.assertEqual(broken['page-byte-cap']['status'], 'failed')
        self.assertIn('drives: an item appears on two pages', broken['page-byte-cap']['detail'])

    def test_cases_that_need_a_second_item_are_not_run_when_the_data_is_too_short(self):
        # With one drive, one position, one charge and one sample no cursor exists; a request with a missing cursor would "pass" meaninglessly.
        hub = FakeHub()
        hub.drives = [drive for drive in hub.drives if drive['id'] == 105]
        hub.positions[105] = hub.positions[105][:1]
        hub.charges = [charge for charge in hub.charges if charge['id'] == 203]
        hub.samples[203] = hub.samples[203][:1]
        needs = {
            'invalid_cursor': 'at least two drives for vehicle_id', 'cursor-time-binding': 'at least two drives for vehicle_id',
            'cursor-kind-binding': 'at least two drives for vehicle_id', 'cursor-vehicle-binding': 'at least two drives for vehicle_id',
            'cursor-scope-binding': 'at least two positions for drive_id', 'cursor-window-binding': 'a page route with at least two items',
            'positions-keyset-three-pages': 'at least three positions for drive_id', 'scenario-page-2': 'at least three drives for vehicle_id',
            'scenario-page-3': 'at least five drives for vehicle_id'}
        results = self.run_cases(hub, ['discovery-success', *needs])
        for case_id, data in needs.items():
            self.assertEqual(results[case_id], {'scope': 'native', 'case_id': case_id, 'status': 'not_run', 'missing': ['data: ' + data]}, case_id)

    def test_a_grant_case_whose_target_does_not_exist_is_not_run_and_never_passed(self):
        # Every unknown vehicle and every unknown digest answers the same 404, so a comparison against a made-up target proves nothing.
        ids = ['discovery-success', 'ungranted-vehicle-404', 'ungranted-vehicles-list', 'ungranted-pack-404']
        results = self.run_cases(FakeHub(), ids, overrides={'ungranted_vehicle_id': str(uuid.uuid4()), 'ungranted_pack_digest': token('no such pack')})
        vehicle = {'scope': 'native', 'status': 'not_run', 'missing': ['data: ungranted_vehicle_id is not readable by bearer']}
        pack = {'scope': 'native', 'status': 'not_run', 'missing': ['data: ungranted_pack_digest is not downloadable by bearer']}
        self.assertEqual(results['ungranted-vehicle-404'], dict(vehicle, case_id='ungranted-vehicle-404'))
        self.assertEqual(results['ungranted-vehicles-list'], dict(vehicle, case_id='ungranted-vehicles-list'))
        self.assertEqual(results['ungranted-pack-404'], dict(pack, case_id='ungranted-pack-404'))

    def test_a_grant_case_is_not_run_when_bearer_itself_cannot_read_the_target(self):
        hub = FakeHub()
        hub.devices['bearer'].grants.discard(VEHICLE_HIDDEN)
        results = self.run_cases(hub, ['discovery-success', 'ungranted-vehicle-404', 'ungranted-vehicles-list', 'ungranted-pack-404'])
        for case_id in ('ungranted-vehicle-404', 'ungranted-vehicles-list', 'ungranted-pack-404'):
            self.assertEqual(results[case_id]['status'], 'not_run', case_id)
            self.assertTrue(results[case_id]['missing'][0].startswith('data: '), case_id)

    def test_a_grant_case_passes_once_bearer_reads_the_target_and_second_bearer_cannot(self):
        results = self.run_cases(FakeHub(), ['discovery-success', 'ungranted-vehicle-404', 'ungranted-vehicles-list', 'ungranted-pack-404'])
        self.assertEqual({item['status'] for item in results.values()}, {'passed'})

    def test_an_unexpected_error_inside_a_case_is_a_failure_never_a_pass(self):
        class Crashing(FakeHub):
            def exchange(self, method, path, **options):
                if path == '/healthz':
                    raise RuntimeError('synthetic-' + token('crash'))
                return super().exchange(method, path, **options)

        hub = Crashing()
        results = self.run_cases(hub, ['health-success', 'ready-success'])
        self.assertEqual(results['health-success'], {'scope': 'native', 'case_id': 'health-success', 'status': 'failed', 'detail': 'check raised an unexpected error'})
        self.assertEqual(results['ready-success']['status'], 'passed')
        self.assertNotIn(token('crash'), json.dumps(results))

    def test_every_way_a_case_can_end_has_a_fixed_outcome(self):
        outcomes = (
            (conf.NotRun('a thing'), {'status': 'not_run', 'missing': ['a thing']}),
            (conf.CheckFailure('a label'), {'status': 'failed', 'detail': 'a label'}),
            (conf.hub_http.AcceptanceError('a transport label'), {'status': 'failed', 'detail': 'a transport label'}),
            (KeyError('secret-' + token('key')), {'status': 'failed', 'detail': 'response could not be interpreted'}),
            (IndexError('secret-' + token('index')), {'status': 'failed', 'detail': 'response could not be interpreted'}),
            (conf.ContractError('document is not strict JSON'), {'status': 'failed', 'detail': 'response could not be interpreted'}),
            (RuntimeError('secret-' + token('runtime')), {'status': 'failed', 'detail': 'check raised an unexpected error'}))
        for error, expected in outcomes:
            with self.subTest(type(error).__name__):
                with mock.patch.object(conf.NativeRun, 'case_health_success', side_effect=error):
                    results = self.run_cases(FakeHub(), ['health-success'])
                self.assertEqual(results['health-success'], dict(expected, scope='native', case_id='health-success'))
                self.assertNotIn('secret-', json.dumps(results))

    def test_the_304_of_the_drives_route_must_carry_the_validator_etag(self):
        hub = FakeHub({'etag_304_differs'})
        results = self.run_cases(hub, ['discovery-success', 'drive-conditional'])
        self.assertEqual(results['drive-conditional']['status'], 'failed')
        self.assertIn('the 304 does not carry the validator ETag', results['drive-conditional']['detail'])

    def test_a_pack_name_without_the_object_suffix_is_a_404_before_any_lookup(self):
        # The Hub strips .sqlite.zst from the object name first, so a bare digest never reaches the digest lookup.
        hub = FakeHub({'pack_streams_ungranted'})
        bearer = hub.devices['second_bearer'].token
        self.assertEqual(hub.exchange('GET', '/v1/packs/sha256/' + token('pack'), bearer=bearer).status, 404)
        self.assertEqual(hub.exchange('GET', '/v1/packs/sha256/' + token('pack') + '.sqlite.zst', bearer=bearer).status, 200)
        self.assertEqual(conf.NativeRun.pack_path(token('pack')), '/v1/packs/sha256/' + token('pack') + '.sqlite.zst')

    def test_the_same_cases_pass_against_the_conforming_hub(self):
        for quirk, (case_ids, failing, _) in self.every_expectation().items():
            with self.subTest(quirk):
                hub = FakeHub()
                results = self.run_cases(hub, [*self.BASE, *case_ids])
                for case_id in (failing,) if isinstance(failing, str) else failing:
                    self.assertEqual(results[case_id]['status'], 'passed', results[case_id])

    def test_a_revoked_bearer_is_rejected_on_every_checked_route_and_a_repeat_is_401(self):
        hub = FakeHub()
        results = self.run_cases(hub, ['discovery-success', 'revoke-then-401', 'revoke-again-401'])
        self.assertEqual({item['status'] for item in results.values()}, {'passed'})
        self.assertTrue(hub.devices['disposable_bearer'].revoked)
        self.assertFalse(hub.devices['bearer'].revoked)

    def test_revoke_with_a_body_is_400_and_revokes_nothing(self):
        hub = FakeHub()
        response = hub.exchange('POST', '/v1/device/revoke', bearer=hub.devices['disposable_bearer'].token, body=b'{}')
        self.assertEqual(response.status, 400)
        self.assertFalse(hub.devices['disposable_bearer'].revoked)
        self.assertEqual(hub.exchange('POST', '/v1/device/revoke', bearer=hub.devices['disposable_bearer'].token).status, 204)
        self.assertEqual(hub.exchange('POST', '/v1/device/revoke', bearer=hub.devices['disposable_bearer'].token).status, 401)

    def test_the_grace_window_is_exclusive_at_its_end(self):
        hub = FakeHub()
        first = hub.devices['rotation_bearer'].token
        self.assertEqual(hub.exchange('POST', '/v1/device/rotate', bearer=first).status, 200)
        grace_end = hub.devices['rotation_bearer'].grace[first][1]
        hub.now = grace_end - 1
        self.assertEqual(hub.exchange('GET', '/v1/vehicles', bearer=first).status, 200)
        hub.now = grace_end
        self.assertEqual(hub.exchange('GET', '/v1/vehicles', bearer=first).status, 401)
        self.assertEqual(hub.exchange('POST', '/v1/device/rotate', bearer=first).status, 401)

    def test_a_rotation_at_the_maximum_lifetime_is_reported_not_run_rather_than_passed(self):
        class CappedHub(FakeHub):
            def rotate(self, device, bearer):
                return self.json(200, {'device_id': device.device_id, 'access_token': bearer, 'expires_at_ms': self.now + 1, 'reason': 'max_lifetime'})

        results = self.run_cases(CappedHub(), ['discovery-success', 'rotate-success', 'rotation-lost-response-retry'])
        self.assertEqual(results['rotate-success'], {'scope': 'native', 'case_id': 'rotate-success', 'status': 'not_run',
                                                     'missing': ['rotation_bearer below its absolute maximum lifetime']})
        self.assertEqual(results['rotation-lost-response-retry']['status'], 'not_run')


    def test_a_rotation_at_the_maximum_that_issues_a_new_bearer_fails_the_hub(self):
        # At the absolute maximum a Hub issues nothing: the answer carries the bearer that was presented. A new token is the Hub's fault.
        class IssuingHub(FakeHub):
            def rotate(self, device, bearer):
                issued = token('issued at the maximum')
                self.secrets['issued at the maximum'] = issued
                return self.json(200, {'device_id': device.device_id, 'access_token': issued, 'expires_at_ms': self.now + 1, 'reason': 'max_lifetime'})

        hub = IssuingHub()
        results = self.run_cases(hub, ['discovery-success', 'rotate-success', 'rotation-lost-response-retry'])
        self.assertEqual(results['rotate-success'], {'scope': 'native', 'case_id': 'rotate-success', 'status': 'failed',
                                                     'detail': 'rotate: a response at the maximum lifetime carries a new bearer'})
        self.assertEqual(results['rotation-lost-response-retry']['status'], 'not_run')
        self.assert_clean(hub, results)

    def test_each_malformed_query_value_is_held_to_invalid_query_on_its_own(self):
        for name, value in conf.MALFORMED_VALUES:
            with self.subTest(name + '=' + value):
                results = self.run_cases(FakeHub({'malformed_accepted=' + name + '=' + value}), ['discovery-success', 'invalid_query'])
                self.assertEqual(results['invalid_query']['status'], 'failed')
                self.assertEqual(results['invalid_query']['detail'], 'drives ' + name + '=' + value + ': expected HTTP 400, got 200')
        results = self.run_cases(FakeHub(), ['discovery-success', 'invalid_query', 'invalid_limit', 'invalid_time_range'])
        self.assertEqual({item['status'] for item in results.values()}, {'passed'})

    def test_a_malformed_value_answered_with_the_code_of_a_range_check_fails_on_every_page_route(self):
        for kind in PAGE_LIMITS:
            for quirk, detail in (('malformed_wrong_code', kind + ' limit=abc: wrong error code'), ('limit_parsed_signed', kind + ' limit=-1: wrong error code')):
                with self.subTest(kind=kind, quirk=quirk):
                    results = self.run_cases(FakeHub({quirk + ':' + kind}), ['discovery-success', 'invalid_query'])
                    self.assertEqual(results['invalid_query'], {'scope': 'native', 'case_id': 'invalid_query', 'status': 'failed', 'detail': detail})

    def test_a_drive_or_charge_id_that_is_not_utf8_is_held_to_the_not_found_json(self):
        self.assertIn('%FF', conf.UNKNOWN_IDENTIFIERS)
        for quirk, case, detail in (('undecodable_400:drive', 'drive-unknown', 'drive %FF: expected HTTP 404, got 400'),
                                    ('undecodable_400:charge', 'charge-unknown', 'charge %FF: expected HTTP 404, got 400'),
                                    ('undecodable_400:drive_positions', 'drive-unknown', 'drive nested: expected HTTP 404, got 400'),
                                    ('undecodable_400:charge_samples', 'charge-unknown', 'charge nested: expected HTTP 404, got 400')):
            with self.subTest(quirk):
                results = self.run_cases(FakeHub({quirk}), ['discovery-success', case])
                self.assertEqual(results[case], {'scope': 'native', 'case_id': case, 'status': 'failed', 'detail': detail})
        results = self.run_cases(FakeHub(), ['discovery-success', 'drive-unknown', 'charge-unknown'])
        self.assertEqual({item['status'] for item in results.values()}, {'passed'})

    def test_the_nil_vehicle_is_an_unknown_vehicle_with_json_on_the_manifest_and_a_malformed_id_is_empty_there(self):
        class EmptyNil(FakeHub):
            def vehicle_route(self, device, segments, query, headers):
                if segments[2] == NIL_VEHICLE and segments[3:] == ['sync', 'manifest']:
                    return self.empty(404)
                return super().vehicle_route(device, segments, query, headers)

        results = self.run_cases(EmptyNil(), ['discovery-success', 'malformed-identifier-404'])
        self.assertEqual(results['malformed-identifier-404'], {'scope': 'native', 'case_id': 'malformed-identifier-404', 'status': 'failed',
                                                               'detail': 'vehicle ' + NIL_VEHICLE + ' on sync_manifest: wrong not-found code'})
        hub = FakeHub()
        for segment, form in (('abc', 'empty'), ('%FF', 'empty'), (NIL_VEHICLE, 'json')):  # the conforming Hub answers as the profile says
            response = hub.exchange('GET', '/v1/vehicles/' + segment + '/sync/manifest', bearer=hub.devices['bearer'].token)
            self.assertEqual((response.status, bool(response.body)), (404, form == 'json'), segment)

    EVERYTHING = set(CAPABILITIES)

    def start_advertising(self, hub, advertised, **options):
        """A run whose discovery said `advertised`: a Hub that advertises only part of the cursor-key group fails discovery-success, so the
        gating of cases by capability is exercised here with the set given directly."""
        run = self.start(hub, **options)
        run.advertised = set(advertised)
        return run

    def recording(self, hub):
        sent, original = [], hub.exchange
        hub.exchange = lambda method, path, **options: (sent.append((method, path, options.get('bearer'))), original(method, path, **options))[1]
        return sent

    def test_a_hub_with_paired_devices_but_no_cursor_key_gets_not_run_for_revoke_and_nothing_is_revoked(self):
        hub = FakeHub({'revoke_capability_only'})
        sent = self.recording(hub)
        results = self.run_cases(hub, ['discovery-success', 'discovery-profile', 'revoke-then-401', 'revoke-again-401'])
        self.assertEqual(results['discovery-profile']['status'], 'passed')
        self.assertEqual(results['revoke-then-401'], {'scope': 'native', 'case_id': 'revoke-then-401', 'status': 'not_run',
                                                      'missing': ['capability query.charges', 'capability sync.packs']})
        self.assertEqual(results['revoke-again-401']['status'], 'not_run')
        self.assertFalse(hub.devices['disposable_bearer'].revoked)
        self.assertEqual([item for item in sent if item[2] == hub.devices['disposable_bearer'].token], [])  # no request used the bearer it would revoke

    def test_a_case_that_declares_a_step_the_hub_does_not_advertise_is_not_run_before_anything_is_sent(self):
        hub = FakeHub()
        sent = self.recording(hub)
        run = self.start_advertising(hub, self.EVERYTHING - {'sync.packs'})
        run.execute('revoke-then-401')
        self.assertEqual(run.results['revoke-then-401'], {'scope': 'native', 'case_id': 'revoke-then-401', 'status': 'not_run', 'missing': ['capability sync.packs']})
        self.assertFalse(hub.devices['disposable_bearer'].revoked)
        self.assertEqual(sent, [])  # not even the read that precedes the revoke
        run.execute('revoke-again-401')
        self.assertEqual(run.results['revoke-again-401']['status'], 'not_run')

    def test_the_sync_manifest_is_checked_whether_or_not_sync_packs_is_advertised(self):
        hub = FakeHub()
        sent = self.recording(hub)
        run = self.start_advertising(hub, self.EVERYTHING - {'sync.packs'})
        run.execute('ungranted-vehicle-404')
        self.assertEqual(run.results['ungranted-vehicle-404'], {'scope': 'native', 'case_id': 'ungranted-vehicle-404', 'status': 'passed'})
        manifests = [item for item in sent if item[1].endswith('/sync/manifest')]
        self.assertEqual([item[2] for item in manifests], [hub.devices['second_bearer'].token] * 2)  # the ungranted vehicle and an unknown one
        self.assertEqual(manifests[0][1], '/v1/vehicles/' + VEHICLE_HIDDEN + '/sync/manifest')
        # and every step the revoke case declares runs when everything is advertised, the manifest and the pack included
        full = FakeHub()
        sent_full = self.recording(full)
        self.run_cases(full, ['discovery-success', 'revoke-then-401'])
        after = [item[1] for item in sent_full if item[2] == full.devices['disposable_bearer'].token][-5:]
        self.assertEqual([path.split('/')[-1] for path in after], ['vehicles', 'current', 'charges', 'manifest', '0' * 64 + '.sqlite.zst'])
        self.assertTrue(after[3].endswith('/sync/manifest'))

    def test_a_hub_that_advertises_only_some_page_routes_gets_not_run_never_a_partial_pass(self):
        every_page = ['page-conditional', 'page-byte-cap', 'ungranted-vehicle-404', 'malformed-identifier-404', 'invalid_limit', 'invalid_time_range',
                      'invalid_query', 'invalid_cursor']
        declared = {
            'query.drives': every_page + ['cursor-kind-binding', 'cursor-vehicle-binding'],
            'query.charges': every_page + ['revoke-then-401', 'cursor-kind-binding', 'cursor-vehicle-binding', 'cursor-window-binding'],
            'query.positions': every_page + ['cursor-scope-binding', 'cursor-window-binding'],
            'query.charge.samples': every_page + ['cursor-scope-binding', 'cursor-window-binding']}
        everything = sorted({case_id for case_ids in declared.values() for case_id in case_ids})
        for capability, not_run in declared.items():
            with self.subTest(capability):
                hub = FakeHub()
                run = self.start_advertising(hub, self.EVERYTHING - {capability})
                for case_id in everything:
                    run.execute(case_id)
                    if case_id in not_run:
                        self.assertEqual(run.results[case_id], {'scope': 'native', 'case_id': case_id, 'status': 'not_run', 'missing': ['capability ' + capability]}, case_id)
                    else:  # a case that does not declare the route is untouched
                        self.assertEqual(run.results[case_id]['status'], 'passed', case_id)

    def test_the_capabilities_a_case_needs_come_from_its_kind_its_kinds_and_its_steps(self):
        run = self.start_advertising(FakeHub(), {'query.vehicles', 'query.current'})
        self.assertEqual(run.unavailable({'kind': 'vehicles'}), [])
        self.assertEqual(run.unavailable({'kind': 'drives'}), ['capability query.drives'])
        self.assertEqual(run.unavailable({'kind': 'vehicles', 'kinds': ['charges', 'drives', 'charges']}), ['capability query.charges', 'capability query.drives'])
        steps = [{'kind': 'pack', 'status': 401}, {'kind': 'sync_manifest', 'status': 401}]
        self.assertEqual(run.unavailable({'kind': 'vehicles', 'steps': steps}), ['capability sync.packs'])  # the manifest needs no token
        self.assertEqual(run.unavailable({'kind': 'revoke', 'kinds': ['pack'], 'steps': steps}), ['capability auth.revoke', 'capability sync.packs'])
        run.advertised = None  # before discovery has said anything nothing is held back
        self.assertEqual(run.unavailable({'kind': 'drives', 'kinds': ['pack'], 'steps': steps}), [])

    def test_an_exchange_starts_only_with_the_minimum_slice_of_the_run_budget_left(self):
        for timeout, expected in ((conf.MIN_EXCHANGE_SECONDS * 0.99, 'not_run'), (conf.MIN_EXCHANGE_SECONDS * 1.01, 'passed')):
            with self.subTest(timeout=timeout):
                hub = FakeHub()
                exchanges, original = [], hub.exchange
                hub.exchange = lambda *arguments, **options: (exchanges.append(arguments), original(*arguments, **options))[1]
                run = self.start(hub, timeout=timeout, clock=lambda: 100.0)  # a frozen clock: the budget left is exactly the timeout
                run.execute('discovery-success')
                self.assertEqual(run.results['discovery-success']['status'], expected)
                self.assertEqual(len(exchanges), 0 if expected == 'not_run' else 1)
                if expected == 'not_run':
                    self.assertEqual(run.results['discovery-success']['missing'], ['time budget'])
        # the control that precedes a comparison obeys the same rule
        hub = FakeHub()
        run = self.start(hub, timeout=conf.MIN_EXCHANGE_SECONDS * 0.5, clock=lambda: 100.0)
        with self.assertRaises(conf.NotRun):
            run.probe('/v1/vehicles')


# ---------------------------------------------------------------------------
# The real HTTPS transport, against a TLS listener on the loopback address
# ---------------------------------------------------------------------------
def make_certificate(root, name):
    """A self-signed certificate for 127.0.0.1 (the recipe of test_hub_matrix) and its key; returns both paths."""
    config = root / (name + '.cnf')
    config.write_text('[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=v3\n[dn]\nCN=127.0.0.1\n[v3]\nbasicConstraints=critical,CA:TRUE\n'
                      'keyUsage=critical,digitalSignature,keyCertSign\nsubjectKeyIdentifier=hash\nsubjectAltName=IP:127.0.0.1\n', encoding='utf-8')
    certificate, key = root / (name + '.pem'), root / (name + '-key.pem')
    done = subprocess.run(['openssl', 'req', '-x509', '-nodes', '-newkey', 'rsa:2048', '-keyout', str(key), '-out', str(certificate), '-days', '1',
                           '-config', str(config)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if done.returncode != 0:
        raise AssertionError('synthetic TLS certificate generation failed')
    return certificate, key


# A text only the garbage answers of the listener carry: a label that quoted the answer of the server would show it.
GARBAGE_MARKER = b'SERVER-TEXT-4c1f9a'
# Answers that are not HTTP framing: a status line that is not HTTP, a status code that is not a number, and a chunked body whose first chunk size is not hex.
GARBAGE_ANSWERS = {
    'a status line that is not HTTP': b'NOT-HTTP-' + GARBAGE_MARKER + b'\r\n\r\n',
    'a status code that is not a number': b'HTTP/1.1 2x0 ' + GARBAGE_MARKER + b'\r\nContent-Length: 0\r\n\r\n',
    'a chunk size that is not hexadecimal': b'HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nzz-' + GARBAGE_MARKER + b'\r\n'}


class TlsListener:
    """A TLS listener on 127.0.0.1 that records the request bytes it receives and answers as `mode` says.

    Modes: `ok` (200 with `body`), `big` (a 3 MiB body), `exact` (a body of exactly 1048576 bytes), `stall` (reads the request, never answers) and
    `garbage` (reads the request, then writes the bytes of `garbage`, which are not an HTTP response, and hangs up).
    `delay` seconds pass between the request and the answer; `respond(head, body)` may choose the answer as (status line, content type, body).
    """

    def __init__(self, certificate, key):
        self.received, self.mode, self.body = [], 'ok', b'{}'
        self.garbage = GARBAGE_ANSWERS['a status line that is not HTTP']
        self.delay, self.respond = 0.0, None
        self.release, self.lock, self.active = threading.Event(), threading.Condition(), 0
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(certificate, key)
        owner = self

        class Handler(socketserver.BaseRequestHandler):
            def handle(self):
                with owner.lock:
                    owner.active += 1
                try:
                    owner.serve(context, self.request)
                finally:
                    with owner.lock:
                        owner.active -= 1
                        owner.lock.notify_all()

        class Server(socketserver.ThreadingTCPServer):
            allow_reuse_address = True
            daemon_threads = False  # server_close() joins every handler thread

        self.server = Server(('127.0.0.1', 0), Handler)
        self.endpoint = 'https://127.0.0.1:' + str(self.server.server_address[1])
        # a daemon, so that a process that stops before close() runs (an interrupt, a harness that crashed) is not held by it for ever; the handler threads
        # stay non-daemon, so that server_close() on the normal path still joins them
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={'poll_interval': 0.05}, daemon=True)
        self.thread.start()

    def serve(self, context, raw):
        try:
            connection = context.wrap_socket(raw, server_side=True)
        except (ssl.SSLError, OSError):
            return  # the client refused the certificate or hung up during the handshake
        try:
            connection.settimeout(3)
            data = b''
            while b'\r\n\r\n' not in data:
                chunk = connection.recv(65536)
                if not chunk:
                    return
                data += chunk
            head, _, rest = data.partition(b'\r\n\r\n')
            length = next((int(line.split(b':')[1]) for line in head.split(b'\r\n') if line.lower().startswith(b'content-length:')), 0)
            while len(rest) < length:
                chunk = connection.recv(65536)
                if not chunk:
                    break
                rest += chunk
            self.received.append(head + b'\r\n\r\n' + rest)
            if self.mode == 'stall':
                self.release.wait(6)
                return
            if self.mode == 'garbage':
                connection.sendall(self.garbage)
                return
            if self.delay:
                self.release.wait(self.delay)  # a Hub that answers late, whatever its own bound
            body = {'big': b'x' * (3 * 1048576), 'exact': b'x' * 1048576}.get(self.mode, self.body)
            status, content_type = b'200 OK', b'application/json'
            if self.respond is not None:
                status, content_type, body = self.respond(head, rest)
            connection.sendall(b'HTTP/1.1 ' + status + b'\r\nContent-Type: ' + content_type + b'\r\nContent-Length: ' + str(len(body)).encode() + b'\r\nConnection: close\r\n\r\n')
            connection.sendall(body)
        except (OSError, ssl.SSLError):
            return  # the client stopped reading
        finally:
            try:
                connection.close()
            except OSError:
                pass

    def settle(self, seconds=3.0):
        """Wait until every connection the listener accepted has been handled."""
        end = time.monotonic() + seconds
        with self.lock:
            while self.active and time.monotonic() < end:
                self.lock.wait(0.05)
            return not self.active

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)


class LoopbackTransportTests(unittest.TestCase):
    """NativeClient.exchange itself: the pin, the bearer header, the byte cap, the deadlines and the fixed labels."""

    @classmethod
    def setUpClass(cls):
        temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(temp.cleanup)
        cls.root = Path(temp.name)
        cls.certificate, key = make_certificate(cls.root, 'trusted')
        cls.other_certificate, _ = make_certificate(cls.root, 'other')
        cls.pin = hashlib.sha256(ssl.PEM_cert_to_DER_cert(cls.certificate.read_text())).hexdigest()
        cls.listener = TlsListener(cls.certificate, key)
        cls.addClassCleanup(cls.listener.close)
        cls.profile = conf.Profile(PROFILE)
        cls.files = 0

    def setUp(self):
        self.listener.release.set()  # let a handler left over from a stalled exchange finish
        self.listener.settle()
        self.listener.release.clear()
        self.listener.received.clear()
        self.listener.mode, self.listener.body = 'ok', b'{}'
        self.listener.delay, self.listener.respond = 0.0, None
        self.listener.garbage = GARBAGE_ANSWERS['a status line that is not HTTP']

    def client(self, certificate=None, pin=None, **options):
        options.setdefault('total_timeout', 3.0)
        return conf.NativeClient(self.listener.endpoint, str(certificate or self.certificate), pin, **options)

    def test_the_bearer_goes_out_as_exactly_one_authorization_header_and_the_right_pin_is_accepted(self):
        bearer = token('loopback bearer')
        client = self.client(pin=self.pin)
        self.assertEqual(client.exchange('GET', '/v1/vehicles', bearer=bearer).status, 200)
        self.assertEqual(client.exchange('GET', '/healthz').status, 200)
        self.listener.settle()
        first, second = (item.split(b'\r\n') for item in self.listener.received)
        self.assertEqual(first[0], b'GET /v1/vehicles HTTP/1.1')
        self.assertEqual([line for line in first if line.lower().startswith(b'authorization:')], [b'Authorization: Bearer ' + bearer.encode()])
        self.assertEqual([line for line in second if line.lower().startswith(b'authorization:')], [])  # no bearer, no header
        for request in (first, second):  # the client sends no Origin header, so the Hub's browser origin answer (403) never reaches a native run
            self.assertEqual([line for line in request if line.lower().startswith(b'origin:')], [])
        self.assertEqual(client.requests, 2)

    def test_a_pin_that_is_not_the_leaf_certificate_fails_before_any_request_byte_is_sent(self):
        secret = token('loopback secret')
        client = self.client(pin='1' * 64)
        with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
            client.exchange('POST', '/v1/pairings/' + PAIRING_ID + '/claim', body=json.dumps({'secret': secret}).encode(), headers={'Content-Type': 'application/json'})
        self.assertEqual(str(failure.exception), 'connected TLS leaf identity mismatch')
        self.listener.settle()
        self.assertEqual(self.listener.received, [])  # the secret never left the client
        self.assertEqual(client.requests, 0)

    def test_a_certificate_the_client_does_not_trust_gives_a_fixed_label_without_a_path_host_or_exception_text(self):
        client = self.client(certificate=self.other_certificate)
        with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
            client.exchange('GET', '/v1/vehicles', bearer=token('loopback bearer'))
        self.assertEqual(str(failure.exception), 'TLS verification or exchange failed')
        self.assertIsNone(failure.exception.__cause__)
        self.assertTrue(failure.exception.__suppress_context__)
        for text in ('127.0.0.1', str(self.other_certificate), str(self.certificate), 'self-signed', 'SSL'):
            self.assertNotIn(text, str(failure.exception))
        self.listener.settle()
        self.assertEqual(self.listener.received, [])

    def test_the_host_name_is_checked_against_the_certificate_when_no_pin_is_given(self):
        # The certificate names only the address 127.0.0.1. Reaching the same listener as localhost, with no pin to fall back on, must be refused:
        # the check of the name is what stands between a descriptor without tls_pin and any certificate the trusted authority signed.
        client = conf.NativeClient('https://localhost:' + self.listener.endpoint.rsplit(':', 1)[1], str(self.certificate), total_timeout=3.0)
        with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
            client.exchange('GET', '/v1/vehicles', bearer=token('loopback bearer'))
        self.assertEqual(str(failure.exception), 'TLS verification or exchange failed')
        self.listener.settle()
        self.assertEqual(self.listener.received, [])  # no request byte, so no bearer, reached the server under the wrong name
        self.assertEqual(self.client().exchange('GET', '/healthz').status, 200)  # the same listener is accepted under the name its certificate carries

    def test_a_response_that_arrives_as_the_deadline_passes_is_not_accepted(self):
        offset = [0.0]
        original = http.client.HTTPResponse.read

        def read_then_expire(response, *arguments, **keywords):
            data = original(response, *arguments, **keywords)
            offset[0] += 1000.0  # the deadline of the exchange passes just after the last byte was read
            return data

        client = self.client(clock=lambda: time.monotonic() + offset[0], total_timeout=5.0)
        self.assertEqual(client.exchange('GET', '/healthz').status, 200)  # control: with no hook the same exchange is accepted
        with mock.patch.object(http.client.HTTPResponse, 'read', read_then_expire):
            with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
                client.exchange('GET', '/healthz')
        self.assertEqual(str(failure.exception), 'bounded HTTPS exchange failed')
        self.assertGreater(offset[0], 0)  # the hook ran: the exchange had read the response before it was refused

    def test_a_response_over_the_byte_limit_is_read_one_byte_past_the_limit_and_reported_through_validate_raw(self):
        bearer = token('loopback bearer')
        self.listener.mode = 'big'
        response = self.client().exchange('GET', '/v1/vehicles', bearer=bearer)
        self.assertEqual(len(response.body), conf.MAX_BYTES + 1)  # the read stops one byte past the limit of a 3 MiB response
        self.assertEqual(conf.validate_raw(self.profile, 'vehicles', 200, response.headers, response.body), ['response exceeds byte limit'])
        self.listener.settle()
        self.listener.mode = 'exact'
        response = self.client().exchange('GET', '/v1/vehicles', bearer=bearer)
        self.assertEqual(len(response.body), conf.MAX_BYTES)  # a body of exactly the limit is not over it
        self.assertNotIn('response exceeds byte limit', conf.validate_raw(self.profile, 'vehicles', 200, response.headers, response.body))

    def test_a_response_that_is_not_http_gives_a_fixed_framing_label_with_none_of_the_text_the_server_sent(self):
        # The branch for http.client.HTTPException and ValueError: the label is fixed and `from None`, so nothing the server wrote (the status line of a
        # BadStatusLine, say) reaches a report through the label or through the cause.
        for label, answer in GARBAGE_ANSWERS.items():
            with self.subTest(label):
                self.listener.settle()
                self.listener.received.clear()
                self.listener.mode, self.listener.garbage = 'garbage', answer
                client = self.client()
                with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
                    client.exchange('GET', '/v1/vehicles', bearer=token('loopback bearer'))
                self.assertEqual(str(failure.exception), 'HTTP response framing is invalid')
                self.assertIsNone(failure.exception.__cause__)  # `from None`: no chain that carries the text of the answer
                self.assertTrue(failure.exception.__suppress_context__)
                for text in (GARBAGE_MARKER.decode(), 'NOT-HTTP', 'BadStatusLine', 'IncompleteRead', '127.0.0.1'):
                    self.assertNotIn(text, str(failure.exception))
                self.assertEqual(client.requests, 1)  # the request went out: it is the answer that is refused
                self.listener.settle()
                self.assertEqual(len(self.listener.received), 1)

    def test_a_native_run_fails_a_case_whose_answer_is_not_http_with_the_fixed_label_only(self):
        self.listener.mode = 'garbage'
        run = conf.NativeRun(self.profile, self.descriptor())
        run.execute('health-success')
        self.assertEqual(run.results['health-success'], {'scope': 'native', 'case_id': 'health-success', 'status': 'failed', 'detail': 'HTTP response framing is invalid'})
        self.assertNotIn(GARBAGE_MARKER.decode(), json.dumps(run.results))
        summary = conf.summarize([run.results['health-success']], 'x', native_requested=True)
        self.assertEqual((summary['failed'], summary['not_run'], conf.exit_code(summary)), (1, 0, 1))  # a malformed answer is a failure of the Hub

    def test_the_listener_thread_is_a_daemon_so_a_run_that_stops_early_does_not_hang_the_interpreter(self):
        self.assertTrue(self.listener.thread.daemon)
        self.assertFalse(self.listener.server.daemon_threads)  # the handler threads are joined by server_close() on the normal path
        # a process that ends without closing its listener (an interrupt, a harness that crashed) must still exit: a non-daemon serve_forever thread would hold it
        code = ('import sys, tempfile\nfrom pathlib import Path\nsys.path.insert(0, %r)\nfrom tests import test_hub_http_12_profile as t\n'
                'with tempfile.TemporaryDirectory() as temp:\n    certificate, key = t.make_certificate(Path(temp), "x")\n    listener = t.TlsListener(certificate, key)\n'
                'print(listener.thread.is_alive())\n') % str(ROOT)  # no close(): the interpreter is left to end with the thread running
        try:
            done = subprocess.run([sys.executable, '-c', code], cwd=ROOT, capture_output=True, timeout=20)
        except subprocess.TimeoutExpired:
            self.fail('the interpreter did not exit while the listener thread was still serving')
        self.assertEqual(done.returncode, 0, done.stderr.decode())
        self.assertEqual(done.stdout.decode().strip(), 'True')  # the thread was serving when the interpreter ended

    def test_a_server_that_never_answers_fails_the_exchange_within_its_timeout(self):
        self.listener.mode = 'stall'
        started = time.monotonic()
        with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
            self.client(total_timeout=0.5).exchange('GET', '/v1/vehicles', bearer=token('loopback bearer'))
        elapsed = time.monotonic() - started
        self.assertEqual(str(failure.exception), 'bounded HTTPS exchange failed')
        self.assertGreaterEqual(elapsed, 0.4)
        self.assertLess(elapsed, 3.0)

    def test_an_exchange_ends_with_the_budget_of_the_run_even_when_its_own_timeout_is_longer(self):
        self.listener.mode = 'stall'
        started = time.monotonic()
        with self.assertRaises(conf.NotRun) as skip:
            self.client(total_timeout=30.0, run_deadline=time.monotonic() + 0.5).exchange('GET', '/v1/vehicles', bearer=token('loopback bearer'))
        self.assertEqual(skip.exception.missing, ['time budget'])  # the budget of the run ended it: the Hub is not at fault
        self.assertLess(time.monotonic() - started, 3.0)
        # a run deadline that has already passed ends the exchange the same way, before a byte is sent
        with self.assertRaises(conf.NotRun) as skip:
            self.client(run_deadline=time.monotonic() - 1).exchange('GET', '/v1/vehicles')
        self.assertEqual(skip.exception.missing, ['time budget'])

    def test_an_exchange_that_reaches_its_own_limit_with_budget_to_spare_is_a_failure_of_the_hub(self):
        self.listener.mode = 'stall'
        started = time.monotonic()
        with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
            self.client(total_timeout=0.5, run_deadline=time.monotonic() + 60).exchange('GET', '/v1/vehicles', bearer=token('loopback bearer'))
        self.assertEqual(str(failure.exception), 'bounded HTTPS exchange failed')
        self.assertGreaterEqual(time.monotonic() - started, 0.4)
        self.assertLess(time.monotonic() - started, 3.0)

    def test_an_exchange_that_fails_with_budget_to_spare_is_a_failure_not_a_skip(self):
        # nothing listens on this port: the connection is refused at once, long before the budget of the run is spent
        probe = socket.socket()
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
        probe.close()
        client = conf.NativeClient('https://127.0.0.1:' + str(port), str(self.certificate), total_timeout=3.0, run_deadline=time.monotonic() + 60)
        with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
            client.exchange('GET', '/healthz')
        self.assertEqual(str(failure.exception), 'bounded HTTPS exchange failed')
        self.assertEqual(client.requests, 0)

    def test_a_response_read_as_the_run_budget_passes_is_not_run_while_one_read_past_its_own_deadline_is_a_failure(self):
        offset = [0.0]
        original = http.client.HTTPResponse.read

        def read_then_expire(response, *arguments, **keywords):
            data = original(response, *arguments, **keywords)
            offset[0] += 1000.0
            return data

        def clock():
            return time.monotonic() + offset[0]

        # the run deadline is the nearer bound: the answer arrived whole, but after the budget of the run was spent
        client = self.client(clock=clock, total_timeout=30.0, run_deadline=clock() + 10.0)
        self.assertEqual(client.exchange('GET', '/healthz').status, 200)  # control: with no hook the same exchange is accepted
        with mock.patch.object(http.client.HTTPResponse, 'read', read_then_expire):
            with self.assertRaises(conf.NotRun) as skip:
                client.exchange('GET', '/healthz')
        self.assertEqual(skip.exception.missing, ['time budget'])
        self.assertGreater(offset[0], 0)
        # the exchange's own deadline is the nearer bound and the run still has budget: the late answer is a failure of the Hub
        offset[0] = 0.0
        far = self.client(clock=clock, total_timeout=5.0, run_deadline=clock() + 1e6)
        with mock.patch.object(http.client.HTTPResponse, 'read', read_then_expire):
            with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
                far.exchange('GET', '/healthz')
        self.assertEqual(str(failure.exception), 'bounded HTTPS exchange failed')

    def descriptor(self, **inputs):
        return {'endpoint': self.listener.endpoint, 'certificate_path': str(self.certificate), 'tls_pin': None, 'inputs': inputs}

    def test_a_native_run_gives_its_client_the_run_budget_so_a_stalled_exchange_cannot_outlast_it(self):
        self.listener.mode = 'stall'
        run = conf.NativeRun(self.profile, self.descriptor(), timeout=1.0)  # the client keeps its own 5 second timeout
        started = time.monotonic()
        run.execute('health-success')
        self.assertLess(time.monotonic() - started, 3.0)
        self.assertEqual(run.results['health-success'], {'scope': 'native', 'case_id': 'health-success', 'status': 'not_run', 'missing': ['time budget']})

    def test_a_run_budget_that_ends_an_exchange_in_flight_is_not_run_though_the_hub_answers_inside_its_own_bound(self):
        # The Hub answers after 0.8 s, well inside the 5 second bound of an exchange; the run budget (0.5 s) is what ends it, so the case did not run.
        self.listener.body = (PROFILE / 'examples/health.json').read_bytes()
        self.listener.delay = 0.8
        run = conf.NativeRun(self.profile, self.descriptor())
        run.execute('health-success')
        self.assertEqual(run.results['health-success'], {'scope': 'native', 'case_id': 'health-success', 'status': 'passed'})  # control: the same Hub passes with no budget
        self.listener.settle()
        run = conf.NativeRun(self.profile, self.descriptor(), timeout=0.5)
        run.execute('health-success')  # had the answer been waited for it would have passed, as in the control: not_run says the budget ended it
        self.assertEqual(run.results['health-success'], {'scope': 'native', 'case_id': 'health-success', 'status': 'not_run', 'missing': ['time budget']})
        summary = conf.summarize([run.results['health-success']], 'x', native_requested=True)
        self.assertEqual((summary['failed'], summary['not_run'], conf.exit_code(summary)), (0, 1, 3))  # incomplete, not failed

    def test_an_exchange_that_reaches_its_own_limit_inside_a_native_run_is_still_a_failure(self):
        self.listener.mode = 'stall'
        client = conf.NativeClient(self.listener.endpoint, str(self.certificate), total_timeout=0.5, run_deadline=time.monotonic() + 60)
        run = conf.NativeRun(self.profile, self.descriptor(), timeout=60, client=client)
        run.execute('health-success')
        self.assertEqual(run.results['health-success'], {'scope': 'native', 'case_id': 'health-success', 'status': 'failed', 'detail': 'bounded HTTPS exchange failed'})

    def invitation(self, pin, secret):
        type(self).files += 1
        path = self.root / ('invitation-%d.json' % self.files)
        value = {**example('invitation'), 'pairingId': PAIRING_ID, 'secret': secret, 'endpoint': self.listener.endpoint, 'tlsPin': pin,
                 'expiresAtMs': int(time.time() * 1000) + 900000}
        value['pairingUri'] = 'teslatlas-hub://pair?' + urllib.parse.urlencode({'endpoint': value['endpoint'], 'pairing_id': PAIRING_ID, 'secret': secret, 'tls_pin': pin})
        write_private(path, value)
        return str(path)

    def test_the_claim_exchange_also_ends_with_the_budget_of_the_run(self):
        # The claim case builds a client of its own, pinned to the invitation; it must carry the run deadline as well.
        self.listener.mode = 'stall'
        run = conf.NativeRun(self.profile, self.descriptor(invitation_path=self.invitation(self.pin, token('loopback invitation secret'))), timeout=1.0)
        started = time.monotonic()
        run.execute('claim-success')
        self.assertLess(time.monotonic() - started, 3.0)
        self.assertEqual(run.results['claim-success'], {'scope': 'native', 'case_id': 'claim-success', 'status': 'not_run', 'missing': ['time budget']})

    def test_a_claim_is_not_sent_unless_the_certificate_is_the_one_the_invitation_pins(self):
        secret = token('loopback invitation secret')
        run = conf.NativeRun(self.profile, self.descriptor(invitation_path=self.invitation('1' * 64, secret)))
        run.execute('claim-success')
        self.assertEqual(run.results['claim-success'], {'scope': 'native', 'case_id': 'claim-success', 'status': 'failed',
                                                        'detail': 'connected TLS leaf identity mismatch'})
        self.listener.settle()
        self.assertEqual(self.listener.received, [])  # the invitation secret never reached the server
        self.assertNotIn(secret, json.dumps(run.results))

    def test_a_claim_with_the_pinned_certificate_reaches_the_server_with_the_secret_and_passes(self):
        secret = token('loopback invitation secret')
        claimed = json.dumps({'device_id': VEHICLE_MAIN, 'access_token': '0' * 64, 'expires_at_ms': T0}).encode()

        def respond(head, rest):  # the Hub's rule, written out in this module: a refused name is 422 text, any other claim is 200
            if refused_device_name(json.loads(rest)['device_name']):
                return b'422 Unprocessable Entity', b'text/plain; charset=utf-8', b'Failed to deserialize the JSON body into the target type'
            return b'200 OK', b'application/json', claimed

        self.listener.respond = respond
        run = conf.NativeRun(self.profile, self.descriptor(invitation_path=self.invitation(self.pin, secret)))
        run.execute('claim-success')
        self.assertEqual(run.results['claim-success'], {'scope': 'native', 'case_id': 'claim-success', 'status': 'passed'})
        self.listener.settle()
        # six refused names, in order, then the claim itself: seven requests on the pinned connection, each carrying the secret and no bearer
        self.assertEqual(len(self.listener.received), len(CLAIM_REFUSED) + 1)
        names = []
        for request in self.listener.received:
            self.assertTrue(request.startswith(b'POST /v1/pairings/' + PAIRING_ID.encode() + b'/claim HTTP/1.1\r\n'))
            self.assertIn(b'Content-Type: application/json', request)
            self.assertIn(secret.encode(), request)
            self.assertNotIn(b'Authorization', request)
            names.append(json.loads(request.partition(b'\r\n\r\n')[2])['device_name'])
        self.assertEqual(names[:-1], [name for _, name in CLAIM_REFUSED])
        self.assertFalse(refused_device_name(names[-1]))  # the claim itself carries a name that is fine

    def test_a_hub_that_refuses_no_device_name_is_failed_at_the_first_refused_name_before_the_claim_is_sent(self):
        secret = token('loopback invitation secret')  # the listener answers 200 to everything: the first refused name is not refused
        self.listener.body = json.dumps({'device_id': VEHICLE_MAIN, 'access_token': '0' * 64, 'expires_at_ms': T0}).encode()
        run = conf.NativeRun(self.profile, self.descriptor(invitation_path=self.invitation(self.pin, secret)))
        run.execute('claim-success')
        self.assertEqual(run.results['claim-success'], {'scope': 'native', 'case_id': 'claim-success', 'status': 'failed',
                                                        'detail': 'claim with a refused device_name (empty): expected HTTP 422, got 200'})
        self.listener.settle()
        self.assertEqual(len(self.listener.received), 1)  # the case stops there: the invitation is not offered again


class ExitStatusTests(NativeTests):
    """0 only when everything asked for passed, 1 on a failure, 3 when nothing failed but a requested check did not run."""

    def run_main(self, hub, *arguments, drop=()):
        descriptor = self.environment(hub, drop=drop)
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        path = Path(temp.name) / 'descriptor.json'
        write_private(path, {'schema_version': 1, 'kind': 'hub-http-v1-actual-hub', 'profile_id': PROFILE_ID, 'endpoint': descriptor['endpoint'],
                             'certificate_path': descriptor['certificate_path'], 'inputs': descriptor['inputs']})
        patcher = mock.patch.object(conf, 'NativeClient', lambda *args, **options: hub)
        patcher.start()
        self.addCleanup(patcher.stop)
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            code = conf.main(['--config', str(path), '--json', *arguments])
        return code, json.loads(output.getvalue())

    def test_a_run_that_passed_everything_exits_0(self):
        code, summary = self.run_main(FakeHub())
        self.assertEqual((code, summary['passed'], summary['failed'], summary['not_run'], summary['complete']), (0, 61, 0, 0, True))

    def test_a_run_whose_time_budget_ended_before_any_native_case_ran_exits_3(self):
        code, summary = self.run_main(FakeHub(), '--timeout-seconds', '0')
        self.assertEqual((summary['native_requested'], summary['passed'], summary['failed'], summary['not_run'], summary['complete']), (True, 11, 0, 50, False))
        self.assertEqual(summary['provenance'], 'offline-profile')
        self.assertEqual(code, 3)

    def test_a_run_with_missing_inputs_exits_3_and_never_0(self):
        code, summary = self.run_main(FakeHub(), drop=['second_bearer', 'ungranted_vehicle_id', 'ungranted_pack_digest'])
        self.assertEqual(summary['failed'], 0)
        self.assertGreater(summary['not_run'], 0)
        self.assertEqual(code, 3)

    def test_a_failure_exits_1_even_when_other_cases_did_not_run(self):
        code, summary = self.run_main(FakeHub({'extra_field'}), drop=['second_bearer'])
        self.assertGreater(summary['failed'], 0)
        self.assertGreater(summary['not_run'], 0)
        self.assertEqual(code, 1)

    def test_the_offline_run_exits_0_and_an_offline_check_that_did_not_run_exits_3(self):
        offline = [{'scope': 'offline', 'status': 'passed'}, {'scope': 'native', 'status': 'not_run'}]
        self.assertEqual(conf.exit_code({'failed': 0, 'native_requested': False, 'results': offline}), 0)
        self.assertEqual(conf.exit_code({'failed': 0, 'native_requested': True, 'results': offline}), 3)
        self.assertEqual(conf.exit_code({'failed': 0, 'native_requested': False, 'results': [{'scope': 'offline', 'status': 'not_run'}]}), 3)
        self.assertEqual(conf.exit_code({'failed': 1, 'native_requested': False, 'results': offline}), 1)

    def test_the_runner_passes_the_exit_status_through(self):
        # A descriptor path that cannot be read is a failure, so the child and the runner both exit 1.
        result = run_script(ROOT / 'conformance/runner.py', '--profile', PROFILE_ID, '--config', '/nonexistent/synthetic-descriptor.json', '--json')
        self.assertEqual(result.returncode, 1)


class DescriptorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile = conf.Profile(PROFILE)

    def descriptor(self, **changes):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        certificate = Path(temp.name) / 'ca.pem'
        certificate.write_text('synthetic')
        value = {'schema_version': 1, 'kind': 'hub-http-v1-actual-hub', 'profile_id': PROFILE_ID, 'endpoint': 'https://127.0.0.1:8443',
                 'certificate_path': str(certificate), 'inputs': {'bearer': token('b'), 'vehicle_id': VEHICLE_MAIN, 'drive_id': 7}}
        value.update(changes)
        path = Path(temp.name) / 'descriptor.json'
        write_private(path, value)
        return path, value

    def test_a_private_descriptor_loads(self):
        path, value = self.descriptor()
        loaded = conf.load_descriptor(path, self.profile)
        self.assertEqual(loaded['endpoint'], 'https://127.0.0.1:8443')
        self.assertEqual(loaded['inputs'], value['inputs'])

    def test_descriptor_problems_are_labelled_without_echoing_values(self):
        secret = token('descriptor secret')
        bad_inputs = {
            'unknown input': {'bearer': secret, 'invented_input': 'x'}, 'short bearer': {'bearer': secret[:-1]}, 'upper-case bearer': {'bearer': secret.upper()},
            'vehicle id': {'vehicle_id': 'not-a-uuid-' + secret}, 'drive id zero': {'drive_id': 0}, 'drive id text': {'drive_id': '7'},
            'drive id bool': {'drive_id': True}, 'drive id range': {'drive_id': 2 ** 63}, 'relative path': {'invitation_path': 'relative/' + secret},
            'duplicate bearers': {'bearer': secret, 'second_bearer': secret}, 'digest': {'ungranted_pack_digest': secret[:-2]}}
        for label, inputs in bad_inputs.items():
            path, _ = self.descriptor(inputs=inputs)
            with self.subTest(label):
                with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
                    conf.load_descriptor(path, self.profile)
                self.assertNotIn(secret, str(failure.exception))
        for label, changes in {'plain http': {'endpoint': 'http://127.0.0.1:8443'}, 'remote host': {'endpoint': 'https://example.com'},
                               'wrong profile': {'profile_id': 'hub-http-v1@1.1.0'}, 'wrong kind': {'kind': 'other'},
                               'unknown key': {'unexpected': 1}, 'relative certificate': {'certificate_path': 'ca.pem'},
                               'bad pin': {'tls_pin': 'abc'}}.items():
            path, _ = self.descriptor(**changes)
            with self.subTest(label):
                with self.assertRaises(conf.hub_http.AcceptanceError):
                    conf.load_descriptor(path, self.profile)

    def test_a_descriptor_that_others_can_read_is_refused(self):
        path, _ = self.descriptor()
        path.chmod(0o640)
        with self.assertRaises(conf.hub_http.AcceptanceError) as failure:
            conf.load_descriptor(path, self.profile)
        self.assertEqual(str(failure.exception), 'private file permissions invalid')
        with self.assertRaises(conf.hub_http.AcceptanceError):
            conf.load_descriptor(path.with_name('missing.json'), self.profile)

    def test_a_run_with_an_unusable_certificate_fails_the_descriptor_and_runs_nothing(self):
        path, _ = self.descriptor()
        summary = conf.run(str(path))
        self.assertEqual(summary['failed'], 1)
        self.assertEqual(summary['not_run'], 50)
        self.assertEqual([item['case_id'] for item in summary['results'] if item['status'] == 'failed'], ['native/descriptor'])
        self.assertNotIn(token('b'), json.dumps(summary))
        self.assertTrue(summary['native_requested'])
        self.assertFalse(summary['complete'])
        self.assertEqual(summary['provenance'], 'offline-profile')  # no Hub was reached, so nothing here is evidence about one


class RunGuardTests(Scratch):
    def test_native_cases_do_not_start_when_an_offline_check_failed(self):
        """The native cases revoke and rotate bearers: a profile that failed its own checks must never reach a Hub."""
        root = self.scratch_profile()
        self.edit(root, 'examples/drive.json', lambda d: d.update(undocumented_field=1))
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        certificate = Path(temp.name) / 'ca.pem'
        certificate.write_text('synthetic')
        path = Path(temp.name) / 'descriptor.json'
        write_private(path, {'schema_version': 1, 'kind': 'hub-http-v1-actual-hub', 'profile_id': PROFILE_ID, 'endpoint': 'https://127.0.0.1:8443',
                             'certificate_path': str(certificate), 'inputs': {'bearer': token('guard'), 'vehicle_id': VEHICLE_MAIN}})
        built = []
        with mock.patch.object(conf, 'NativeClient', lambda *arguments, **options: built.append(arguments)), \
                mock.patch.object(conf.NativeRun, 'run', side_effect=AssertionError('a native case started')):
            summary = conf.run(str(path), profile_dir=root)
        failed = {item['case_id']: item for item in summary['results'] if item['status'] == 'failed'}
        self.assertEqual(sorted(failed), ['native/descriptor', 'offline/examples-server-strict'])
        self.assertEqual(failed['native/descriptor']['detail'], 'profile checks failed; native checks were not started')
        self.assertEqual((built, summary['not_run'], summary['native_requested'], summary['provenance']), ([], 50, True, 'offline-profile'))
        self.assertEqual(conf.exit_code(summary), 1)


# ---------------------------------------------------------------------------
# Documentation
# ---------------------------------------------------------------------------
class DocumentationTests(unittest.TestCase):
    def test_generators_readme_documents_the_three_generators_and_the_integer_rule(self):
        text = (ROOT / 'generators/README.md').read_text()
        for phrase in ('openapi-generator', 'swift5', 'typescript-fetch', 'python', 'profiles/hub-http-v1/1.2.0/openapi.json',
                       '9007199254740991', 'signed 64-bit', 'ignore unknown', 'No generated code is committed'):
            self.assertIn(phrase, text)

    def test_generators_readme_makes_the_python_client_ignore_unknown_fields(self):
        text = (ROOT / 'generators/README.md').read_text()
        flag = '--additional-properties=disallowAdditionalPropertiesIfNotPresent=false'
        python = text[text.index('-g python'):]
        self.assertLess(python.index(flag), python.index('hub-http-python'))  # the flag is part of the python invocation itself
        self.assertEqual(text.count(flag), 2)  # the invocation and the explanation
        unknown = text[text.index('## Unknown fields'):]
        for phrase in (flag, 'raises', 'to_ms'):
            self.assertIn(phrase, text if phrase == 'to_ms' else unknown)

    def test_generators_readme_tells_clients_to_treat_an_empty_503_as_transient(self):
        text = (ROOT / 'generators/README.md').read_text()
        section = text[text.index('## Empty 503'):]
        for phrase in ('empty body', 'handler', 'handler_timeout_response', 'retryable',
                       'except on the claim route, which is never retried automatically (the invitation is single use and the claim may already have consumed it)'):
            self.assertIn(phrase, ' '.join(section.split()))

    def test_generators_readme_says_the_device_name_pattern_is_one_plain_character_class(self):
        text = ' '.join((ROOT / 'generators/README.md').read_text().split())
        for phrase in ('The schema states the control-character rule as `not` over a `pattern` that is one plain character class (no anchor, no look-ahead, no escape class)',
                       'the regular-expression subset that JSON Schema 2020-12 Core section 6.4 recommends',
                       'so an engine without look-ahead (Go `regexp`, Rust `regex`) can load the schema'):
            self.assertIn(phrase, text)

    def test_readme_lists_the_1_2_0_profile(self):
        text = (ROOT / 'README.md').read_text()
        self.assertIn('hub-http-v1@1.2.0', text)
        self.assertIn('profiles/hub-http-v1/1.2.0/', text)
        self.assertIn('a Hub that does not serve it yet serves `1.1.0`.', ' '.join(text.split()))
        self.assertNotIn('a Hub that does not yet serves', ' '.join(text.split()))

    def test_generators_readme_names_the_int64_format_and_the_413_of_a_claim(self):
        text = ' '.join((ROOT / 'generators/README.md').read_text().split())
        for phrase in ('carries `format: int64`', 'The format is an annotation', 'changes no verdict',
                       'A pairing claim body over 4096 bytes is `413` with a bounded `text/plain` body', 'takes no body at all (any body is `400`)',
                       'a claim whose `Content-Type` is missing or is not a JSON media type, `application/json` or `application/*+json`, is `415` whatever its size',
                       # the path rule is scoped to the bearer routes: the pairing_id of the claim route is another matter (C5-N1)
                       'On the bearer routes, a `vehicle_id`, `drive_id`, `charge_id` or pack name that is malformed, including one that is not valid UTF-8 (`%FF`), '
                       'and the nil UUID are a `404`', 'The `pairing_id` of the claim route is not covered by this rule',
                       # the device name (r4-reality-S1)
                       'A claim\'s `device_name` is 1 to 128 UTF-8 bytes with no control character; a name that breaks the rule is `422` with a bounded `text/plain` body '
                       'and the invitation is not consumed', 'check the byte length yourself'):
            self.assertIn(phrase, text)
        self.assertNotIn('A path segment that is not a UUID or an id', text)
        # not every generator has a 64-bit type: typescript-fetch maps int64 to number (a double), so the JavaScript rule still applies
        self.assertIn('where the target language has one (`swift5` `Int64`, `python` `int`; `typescript-fetch` still maps it to `number`, a double, '
                      'so follow the JavaScript rule below)', text)
        self.assertNotIn('to choose a 64-bit type for any target', text)

    def test_current_hub_reference_has_a_1_2_0_section(self):
        text = (ROOT / 'docs/reference/current-hub.md').read_text()
        self.assertIn('hub-http-v1@1.2.0', text)
        for phrase in ('/v1/device/revoke', 'max_lifetime', 'tqp1', 'query.charges', 'query.positions', 'query.charge.samples', 'auth.revoke',
                       'conformance/run --profile hub-http-v1@1.2.0', 'number'):
            self.assertIn(phrase, text)
        section = text[text.index('## hub-http-v1@1.2.0'):text.index('## Choose the profile')]
        for phrase in ('**Handler time budget.**', '15 second handler budget', 'empty body', '`/healthz` lists `503`', 'Only closed sessions are served',
                       '`404 drive_not_found`', '`404 charge_not_found`', 'Eighteen choices', 'to_ms', 'no schema `default`', 'is `not_run`, not a pass',
                       'attested by the descriptor', 'not a failure'):
            self.assertIn(phrase, section)
        self.assertNotIn('Six choices', section)
        self.assertNotIn('Twelve choices', section)
        self.assertNotIn('Fifteen choices', section)
        self.assertNotIn('Seventeen choices', section)
        flat = ' '.join(section.split())
        for phrase in (
                # the drives route keeps the two 1.1.0 forms (N3), and the empty 503 of the handler budget besides
                'its two `1.1.0` `503` forms (empty, and JSON `service_unavailable`)', 'can also answer the empty handler-timeout `503`',
                # a malformed query value (HR3-2)
                'does not parse as a decimal integer in the parameter\'s range', '`limit=abc`', '`to_ms=9223372036854775808`) is also `400 invalid_query`',
                '`invalid_limit` and `invalid_time_range` apply only to well-formed values',
                # the sync routes: asserted, not inherited (S1)
                'Each sync route keeps the `404` the Hub gives an unknown vehicle today',
                '`hub-sync-v1@1.3.0` lists the JSON `vehicle_not_found` `404` on changes-since and an empty `404` on signing-keys and prepared-artefacts',
                '(its `status-tables.json`; `hub-sync-v1@1.4.0` publishes no status table and its OpenAPI lists no `404` on changes-since or the manifest)',
                'It lists no `404` on manifest and no-op', '`1.2.0` asserts those two statuses (it does not inherit them)',
                # the current drives route parses the query before it looks the vehicle up (r4-reality-N1)
                'The current drives route does not check the vehicle before it reads the query yet: it parses the query first, so an unknown or ungranted vehicle '
                'with an invalid query is `400` today, and the later Hub batch must move the check first, as the case `ungranted-vehicle-404` holds a Hub to.',
                'whether or not `sync.packs` is advertised',
                # one grace row per device (HR3-4)
                'Only the bearer replaced by the latest rotation has a grace window: a further rotation ends the earlier bearer\'s grace, and it is then `401`',
                # the claim limit and the revoke rule (HR3-1)
                '**Claim size.**', '`413` is therefore part of the claim route\'s status set in `1.2.0`', 'does not apply to `1.2.0`',
                'including one over 4096 bytes, is `400` and revokes nothing, never `413`',
                'no layer of the Hub rejects an over-limit body it does not read',
                # what conformance checks of the sync 404s, and what the manifest answer does not depend on (R4-contract-N1)
                '`1.2.0` asserts those two statuses (it does not inherit them). Conformance checks the manifest\'s (JSON `vehicle_not_found` for a well-formed '
                'unknown, ungranted or nil vehicle id, an empty `404` for a malformed one) and does not check the no-op\'s',
                'so its `401` and `404` answers do not depend on that token',
                # the claim 413 needs the JSON content type, and its text is the extractor's (R4-reality-N2, R4-contract-N3)
                'A larger body with a JSON `Content-Type` (`application/json` or any `application/*+json`) is `413` with a bounded `text/plain` body '
                '(axum\'s "Failed to buffer the request body: length limit exceeded")',
                'a missing `Content-Type`, or one that is not a JSON media type (neither `application/json` nor `application/*+json`), is `415` whatever the size',
                # the device name, and the claim that is never retried (r4-reality-S1)
                '**Device name.**', 'is 1 to 128 UTF-8 bytes with no control character (U+0000 to U+001F and U+007F to U+009F)',
                'A name that breaks the rule is `422` with a bounded `text/plain` body, answered before the invitation is consulted: the refusal consumes nothing, '
                'and the same invitation still claims with a valid name', '`maxLength` counts characters and cannot state the byte limit, so a client checks the byte length itself',
                'The current Hub does this: the claim handler judges the name with the same function the store applies as its first statement',
                'answers `422` with a short `text/plain` sentence that states the rule; the name itself never causes a `503`',
                '(A Hub before this change left the check to the store, where the same names ended as an empty `503`, which no client can correct by retrying.)',
                'Conformance sends six such names before the real claim and expects `422` for each, then a `200` for the claim itself',
                'Treat an empty `503` on any route except the claim as a transient outage and retry later. A claim is never retried automatically',
                'The claim case first sends the six `device_name` values a Hub must refuse (see Device name) and then the claim itself, which must still succeed because a refusal consumed nothing',
                # malformed identifiers: an undecodable segment, the nil UUID, and the bearer first (HR4-1, r4-reality-N1)
                '**Malformed identifiers.**', 'percent-decodes to bytes that are not UTF-8 (for example `%FF`)', 'the nil UUID `00000000-0000-0000-0000-000000000000`',
                'are answered as not found (each route\'s malformed-identifier answer, or for the nil UUID its unknown-vehicle answer), after the bearer check',
                'A request without a valid bearer is `401` whatever its path, never `400`',
                'the other sync routes answer as `access.sync_routes` of `profile.json` says',
                'Only the lowercase hyphenated form of a `vehicle_id` and the plain decimal form of a `drive_id` or `charge_id` are specified',
                'a UUID in upper case, without hyphens, in braces or as `urn:uuid:`; an integer with a plus sign or leading zeros', 'client must not rely on being refused',
                'axum answers `400` with a `text/plain` body for an undecodable segment before the handler runs, so before authentication',
                '`Result<Path<_>, PathRejection>`', 'the nil UUID is `503` on `/current` and the sync routes (the drives route already answers it as unknown)',
                # a request with an Origin the Hub does not admit (r4-reality-N4)
                '**Browser origins.**', 'is answered `403` with an empty body and `Cache-Control: no-store`, before the route runs, on any route',
                'Which routes admit browser origins, and how the Hub answers a CORS preflight (an `OPTIONS` request), are set by the Hub and lie outside this profile',
                'A native client sends no `Origin` header and never sees any of them',
                '(13) A path segment that is not valid UTF-8 and the nil UUID', 'never `400`, and never `503` because of the value', '(14) A request with an `Origin` header the Hub does not admit is `403` on any route',
                '(15) Conformance has one case beyond the specification\'s list, `malformed-identifier-404`', 'The 50 native cases',
                '(16) A claim whose `device_name` is empty, over 128 UTF-8 bytes or has a control character is `422`', 'so the empty `503` rule excludes it',
                '(17) Every vehicle-scoped route checks the grant before it reads the query',
                # the identifier spellings are the 18th choice beyond the specification (R5-text-N5)
                '(18) Only the lowercase hyphenated UUID and plain decimal ids are specified: other spellings a Hub\'s parser accepts may be served, '
                'and a client must not rely on their refusal',
                # the manifest's empty 404 is not only the answer for a malformed id (R5-reality-N1)
                '(the manifest also answers an empty `404` for a known vehicle that has no published full snapshot yet, so on the manifest an empty `404` means '
                'a malformed id or that no snapshot is published yet)', 'does not check the no-op\'s or the no-snapshot answer',
                '(the manifest\'s empty `404` is also what a known vehicle with no published full snapshot answers; see Access)',
                'and on the manifest also empty for a known vehicle with no published full snapshot yet',
                # int64 (HR3-3) and the declared routes of a case (N2)
                'carries `format: int64`', 'runs on all of them or is `not_run`',
                'An exchange never outlasts that budget and does not start with less than a quarter of a second of it left',
                # a budget that ends an exchange in flight is not a failure of the Hub (R4-rigour-S1)
                'An exchange that the budget ends while it is in flight makes its case `not_run` for the time budget, never a failure of the Hub; an exchange that '
                'reaches its own five second limit with budget to spare is still a failure'):
            self.assertIn(phrase, flat)
        self.assertNotIn('that `hub-sync-v1` gives them', flat)
        self.assertNotIn('and both `503` forms', flat)
        self.assertNotIn('are checked on every Hub', flat)
        self.assertNotIn('and its conformance holds a Hub to them', flat)
        self.assertNotIn('The 49 native cases', flat)
        self.assertNotIn('answered as an unknown vehicle, after the bearer check', flat)
        self.assertNotIn('malformed-identifier answer), after the bearer check', flat)  # the nil UUID gets its unknown-vehicle answer, not the malformed one (R5-text-N6)
        self.assertNotIn('Treat an empty `503` on any route as a transient outage', flat)
        self.assertNotIn('The current Hub does not do this yet, and the later Hub batch must: it validates the name inside the store call', flat)
        self.assertNotIn('consumes nothing (a requirement on the later Hub batch; see Device name)', flat)
        self.assertNotIn('never `400` or `503` (a requirement', flat)
        self.assertNotIn('`hub-sync-v1` lists', flat)  # every statement about what hub-sync-v1 lists names the version it is read from
        self.assertNotIn('every one of the 49 cases', flat)
        self.assertNotIn('(the extractor\'s "Failed to buffer the request body")', flat)  # the text carries the inner error

    def test_current_hub_reference_says_the_1_1_0_walkthrough_does_not_fit_a_1_2_0_hub(self):
        flat = ' '.join((ROOT / 'docs/reference/current-hub.md').read_text().split())
        for phrase in (
                # the introduction: which profile a Hub built from the current source names, and where the 1.1.0 walkthrough applies
                'A Hub built from the current source names `hub-http-v1@1.2.0` in its discovery document',
                'applies as written only to a Hub whose discovery has no `profile` member; its discovery check rejects a `1.2.0` Hub',
                # the 1.2.0 section no longer says the 1.1.0 text stays true for discovery
                'stays true for the routes `1.2.0` keeps, except discovery, which the Discovery paragraph below replaces',
                # the profile choice and the first walkthrough block
                'A Hub whose discovery names `hub-http-v1@1.2.0` has a `profile` member',
                'so it exits on a Hub whose discovery names `hub-http-v1@1.2.0`',
                'the `1.1.0` schema rejects the `profile` member and the token set',
                'validate with `$PWD/profiles/hub-http-v1/1.2.0` instead'):
            self.assertIn(phrase, flat)
        # the discovery block reports the profile it was given, so the same block serves both directories
        self.assertIn('raise SystemExit(f"discovery does not match {profile.parent.name}@{profile.name}")', (ROOT / 'docs/reference/current-hub.md').read_text())

    def test_the_documents_name_the_same_page_limits_as_the_profile(self):
        text = (ROOT / 'docs/reference/current-hub.md').read_text()
        section = text[text.index('hub-http-v1@1.2.0'):]
        for fragment in ('2048', '1048576', '404'):
            self.assertIn(fragment, section)


if __name__ == '__main__':
    unittest.main()
