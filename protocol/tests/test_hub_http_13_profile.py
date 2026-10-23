"""Independent hub-http-v1@1.3.0 contract tests; no Hub implementation is imported.

The profile directory is the base minor (hub-http-v1@1.2.0) plus the changes of C6 R47 to R49, R58, R61, R62, R66 and R75. Every expected list is
written out literally here, so the builder cannot agree with itself. The native cases run against an in-memory client that follows the contract,
and against variants of it that each break one rule.
"""
import copy
import difflib
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from jsonschema import Draft202012Validator, FormatChecker  # noqa: E402

from conformance import hub_http_13 as conf  # noqa: E402

PROFILE = ROOT / 'profiles/hub-http-v1/1.3.0'
BASE = ROOT / 'profiles/hub-http-v1/1.2.0'
GENERATOR = ROOT / 'tools/build_hub_http_profile_13.py'
PROFILE_ID = 'hub-http-v1@1.3.0'
# The checksum list of the published 1.2.0 profile, which this minor never edits.
BASE_SHA256SUMS_DIGEST = '7aa26deeaa9052bc77e0a6f58b89422fa21ef0ec2fd53cec1adab0361db5f7e1'
I64_MAX = 2 ** 63 - 1

CURRENT_RENAMES = [  # C6 R47: 1.2.0 key, 1.3.0 key
    ('state', 'activity_state'), ('since', 'state_since_ms'), ('healthy', 'processing_healthy'), ('geofence', 'geofence_name'),
    ('model', 'model_code'), ('version', 'software_version'), ('update_available', 'software_update_available'),
    ('update_status', 'software_update_status'), ('update_version', 'software_update_version'),
    ('download_perc', 'software_update_download_perc'), ('install_perc', 'software_update_install_perc'), ('speed', 'speed_kmh'),
    ('odometer', 'odometer_km'), ('rated_battery_range_km', 'battery_range_km'),
    ('active_route_miles_to_arrival', 'active_route_km_to_arrival'), ('plugged_in', 'charge_cable_connected'), ('doors_open', 'any_door_open'),
    ('windows_open', 'any_window_open'), ('driver_front_door_open', 'door_driver_front_open'), ('driver_rear_door_open', 'door_driver_rear_open'),
    ('passenger_front_door_open', 'door_passenger_front_open'), ('passenger_rear_door_open', 'door_passenger_rear_open'),
    ('driver_front_window_open', 'window_driver_front_open'), ('driver_rear_window_open', 'window_driver_rear_open'),
    ('passenger_front_window_open', 'window_passenger_front_open'), ('passenger_rear_window_open', 'window_passenger_rear_open'),
    ('frunk_open', 'front_trunk_open'), ('trunk_open', 'rear_trunk_open')]
CAR_RENAMES = [('model', 'model_code'), ('marketing_name', 'variant_name'), ('source_eid', 'api_vehicle_id'), ('source_vid', 'stream_vehicle_id')]
SETTINGS_RENAMES = [  # C6 R49
    ('enabled', 'collection_enabled'), ('use_streaming_api', 'streaming_enabled'), ('suspend_min', 'sleep_window_min'),
    ('suspend_after_idle_min', 'sleep_after_idle_min'), ('suspend_min_resolved', 'sleep_window_min_resolved'),
    ('req_not_unlocked', 'sleep_requires_locked'), ('free_supercharging', 'supercharging_free'), ('lfp_battery', 'battery_lfp')]
HISTORY_RENAMES = {  # C6 R58 (a)
    'drive': [
        ('start_date_ms', 'started_at_ms'), ('end_date_ms', 'ended_at_ms'), ('start_ideal_range_km', 'start_ideal_battery_range_km'),
        ('end_ideal_range_km', 'end_ideal_battery_range_km'), ('start_rated_range_km', 'start_battery_range_km'),
        ('end_rated_range_km', 'end_battery_range_km'), ('start_geofence', 'start_geofence_name'), ('end_geofence', 'end_geofence_name'),
        ('speed_max', 'speed_max_kmh'), ('power_max', 'power_max_kw'), ('power_min', 'power_min_kw'), ('outside_temp_avg', 'outside_temp_avg_c'),
        ('inside_temp_avg', 'inside_temp_avg_c'), ('ascent', 'ascent_m'), ('descent', 'descent_m')],
    'charge': [
        ('start_date_ms', 'started_at_ms'), ('end_date_ms', 'ended_at_ms'), ('start_ideal_range_km', 'start_ideal_battery_range_km'),
        ('end_ideal_range_km', 'end_ideal_battery_range_km'), ('start_rated_range_km', 'start_battery_range_km'),
        ('end_rated_range_km', 'end_battery_range_km'), ('geofence', 'geofence_name'), ('billing_type', 'tariff_type'),
        ('cost_per_unit', 'tariff_rate'), ('session_fee', 'tariff_session_fee'), ('charge_energy_added', 'charge_energy_added_kwh'),
        ('outside_temp_avg', 'outside_temp_avg_c')],
    'position': [('date_ms', 'timestamp_ms'), ('speed', 'speed_kmh'), ('odometer', 'odometer_km'), ('rated_battery_range_km', 'battery_range_km')],
    'charge_sample': [('ideal_range_km', 'ideal_battery_range_km'), ('rated_range_km', 'battery_range_km')]}
# C6 R58 (e): property and required counts of each item.
ITEM_COUNTS = {'drive': (45, 43), 'charge': (40, 39), 'position': (31, 30), 'charge_sample': (21, 21)}
# The keys whose field-semantics `source` text the hand-overs of C1 R95, C2 R82 and C8 R48 replace, by item.
HANDOVER_KEYS = {
    'drive': {'ascent_m', 'descent_m', 'duration_min', 'distance_km', 'ended_at_ms', 'start_geofence_name', 'end_geofence_name'},
    'charge': {'started_at_ms', 'ended_at_ms', 'charge_energy_used_kwh', 'cost', 'tariff_type', 'tariff_rate', 'tariff_session_fee', 'is_dc',
               'fast_charger_type', 'geofence_name'}}
# C6 R27: the class of each output field that keeps its name (T: transient, R28; P: persistent), written out from the specification table.
TRANSIENT_KEYS = ('speed_kmh power shift_state active_route_destination active_route_latitude active_route_longitude active_route_km_to_arrival '
                  'active_route_minutes_to_arrival active_route_traffic_minutes_delay active_route_energy_at_arrival charger_power charger_voltage '
                  'charger_actual_current charger_phases time_to_full_charge inside_temp outside_temp is_climate_on is_preconditioning '
                  'climate_keeper_mode sentry_mode is_user_present center_display_state software_update_download_perc software_update_install_perc').split()
PERSISTENT_KEYS = ('latitude longitude heading elevation battery_level usable_battery_level battery_range_km ideal_battery_range_km est_battery_range_km '
                   'charge_limit_soc charge_energy_added charge_current_request charge_current_request_max scheduled_charging_start_time '
                   'charge_port_door_open odometer_km locked tpms_soft_warning_fl tpms_soft_warning_fr tpms_soft_warning_rl tpms_soft_warning_rr '
                   'sun_roof_percent_open sun_roof_state tpms_pressure_fl tpms_pressure_fr tpms_pressure_rl tpms_pressure_rr sun_roof_installed '
                   'door_driver_front_open door_driver_rear_open door_passenger_front_open door_passenger_rear_open front_trunk_open rear_trunk_open '
                   'window_driver_front_open window_driver_rear_open window_passenger_front_open window_passenger_rear_open').split()
# Class P entries that are not a newest-statement field: the four software fields (first-usable source chains) and the two three-valued aggregates.
# They carry the persistence sentence and not the newest-statement sentence.
PERSISTENT_ONLY_KEYS = 'software_version software_update_available software_update_status software_update_version any_door_open any_window_open'.split()
PERSISTENT_SENTENCE = 'Persistent: the last known value is kept through sleep however old it is; compare observed_at_ms with your own clock.'
NEWEST_SENTENCE = 'The newest statement of the field decides; when it is not valid the field is null and no older value is used.'
TARIFF_RATE_UNIT = 'currency units per kWh when tariff_type is per_kwh, per minute when per_minute; decimal serialised as a JSON number'
IMPORT_ADDRESS = ("The address was copied unchanged from the history of an import source, the source database the Hub's owner migrated from. "
                  "The Hub did not look it up and does not replace it with a lookup. The Hub states nothing about how the import source obtained it.")
IMPORT_ELEVATION = ("The elevation was copied unchanged from the history of an import source, the source database the Hub's owner migrated from. "
                    "The Hub did not sample it from terrain data and does not replace it. The Hub states nothing about how the import source obtained it.")
NEW_CASES = ['current-aliases', 'drive-aliases', 'charge-aliases', 'position-aliases', 'charge-sample-aliases',
             'drives-full-page', 'charges-full-page', 'positions-full-page', 'charge-samples-full-page']
BASE_CASE_COUNT = 50


def load(name, root=PROFILE):
    return json.loads((Path(root) / name).read_text(encoding='utf-8'))


def alias_text(new):
    return 'Deprecated alias of ' + new + '; carries the same value.'


def nullable_branches(schema):
    return [branch for branch in schema.get('anyOf', []) if branch != {'type': 'null'}]


def reseal(root):
    root = Path(root)
    lines = [hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + path.relative_to(root).as_posix() + '\n'
             for path in sorted(item for item in root.rglob('*') if item.is_file() and item.name != 'SHA256SUMS')]
    (root / 'SHA256SUMS').write_text(''.join(lines))


class BuilderTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location('build_hub_http_profile_13', GENERATOR)
        cls.builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.builder)

    def test_c6_profile_builder_round_trip(self):
        # The builder reads the 1.2.0 tree, writes the 1.3.0 tree, and the committed tree is byte for byte what it writes.
        result = subprocess.run([sys.executable, str(GENERATOR), '--check'], capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        digest = hashlib.sha256((PROFILE / 'SHA256SUMS').read_bytes()).hexdigest()
        self.assertEqual(result.stdout.decode().strip(), PROFILE_ID + ' sha256=' + digest)
        files = self.builder.bundle()
        on_disk = {path.relative_to(PROFILE).as_posix() for path in PROFILE.rglob('*') if path.is_file() and path.name != 'SHA256SUMS'}
        self.assertEqual(set(files), on_disk)
        for name, data in files.items():
            self.assertEqual((PROFILE / name).read_bytes(), data, name)
        listed = {line.split('  ', 1)[1]: line.split('  ', 1)[0] for line in (PROFILE / 'SHA256SUMS').read_text().splitlines()}
        self.assertEqual(set(listed), on_disk)
        for name, digest in listed.items():
            self.assertEqual(hashlib.sha256((PROFILE / name).read_bytes()).hexdigest(), digest, name)

    def test_the_builder_reads_the_base_and_never_writes_it(self):
        sums = (BASE / 'SHA256SUMS').read_bytes()
        self.assertEqual(hashlib.sha256(sums).hexdigest(), BASE_SHA256SUMS_DIGEST)
        for line in sums.decode().splitlines():
            digest, name = line.split('  ', 1)
            self.assertEqual(hashlib.sha256((BASE / name).read_bytes()).hexdigest(), digest, name)
        self.assertEqual(self.builder.BASE, BASE)
        self.assertEqual(self.builder.ROOT, PROFILE)

    def test_the_builder_check_detects_a_stale_edited_or_extra_file(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name) / '1.3.0'
        shutil.copytree(PROFILE, root)
        original = self.builder.ROOT
        self.addCleanup(setattr, self.builder, 'ROOT', original)
        self.builder.ROOT = root
        argv = sys.argv
        self.addCleanup(setattr, sys, 'argv', argv)
        sys.argv = ['build', '--check']
        self.builder.main()  # an exact copy passes
        (root / 'cases.json').write_text((root / 'cases.json').read_text().replace('"drive-aliases"', '"drive-aliases-x"'))
        with self.assertRaises(SystemExit):
            self.builder.main()
        shutil.rmtree(root)
        shutil.copytree(PROFILE, root)
        (root / 'extra.json').write_text('{}\n')
        with self.assertRaises(SystemExit):
            self.builder.main()


class IdentityTests(unittest.TestCase):
    def test_identity_fields(self):
        profile = load('profile.json')
        self.assertEqual((profile['profile_id'], profile['contract_version'], profile['previous_profile'], profile['status']),
                         (PROFILE_ID, '1.3.0', 'hub-http-v1@1.2.0', 'served'))
        self.assertEqual(load('openapi.json')['info']['version'], '1.3.0')
        self.assertEqual(load('cases.json')['profile_id'], PROFILE_ID)
        self.assertEqual(load('field-semantics.json')['profile_id'], PROFILE_ID)
        self.assertEqual(load('sync-regression.json')['profile_id'], PROFILE_ID)
        for path in sorted(PROFILE.glob('*.schema.json')):
            self.assertEqual(load(path.name)['$id'], 'urn:teslatlas:hub-http-v1:1.3.0:' + path.name.removesuffix('.schema.json'))
        for name in ('discovery', 'discovery-unsigned'):
            self.assertEqual(load('examples/' + name + '.json')['profile'], PROFILE_ID)

    def test_the_base_identity_is_named_only_where_the_text_means_the_base(self):
        # The previous profile, the deprecation text of car.model ("as 1.2.0 served it") and the carried sync sentence.
        found = sorted(path.name for path in PROFILE.rglob('*.json') if '1.2.0' in path.read_text())
        self.assertEqual(found, ['profile.json', 'resources.schema.json', 'sync-regression.json'])
        self.assertEqual(load('profile.json')['profile_id'], PROFILE_ID)
        self.assertEqual(load('profile.json')['previous_profile'], 'hub-http-v1@1.2.0')

    def test_capabilities_cursor_kinds_and_discovery_are_the_base_ones(self):
        # C6 R51 and R62: the minor adds no capability token and no cursor kind.
        base, profile = load('profile.json', BASE), load('profile.json')
        self.assertEqual(profile['capabilities'], base['capabilities'])
        self.assertEqual(profile['capability_advertisement'], base['capability_advertisement'])
        self.assertEqual(profile['cursor']['payload'], base['cursor']['payload'])
        self.assertEqual(load('discovery.schema.json')['properties'], load('discovery.schema.json', BASE)['properties'])

    def test_every_untouched_document_equals_the_base_but_for_its_identity(self):
        for name in ('auth.schema.json', 'cursor.schema.json', 'errors.schema.json', 'rotation-grace.schema.json', 'sync.schema.json'):
            base, new = load(name, BASE), load(name)
            base['$id'] = new['$id'] = None
            self.assertEqual(new, base, name)
        base, new = load('openapi.json', BASE), load('openapi.json')
        base['info']['version'] = new['info']['version'] = None
        self.assertEqual(new, base)
        base, new = load('sync-regression.json', BASE), load('sync-regression.json')
        base['profile_id'] = new['profile_id'] = None
        self.assertEqual(new, base)

    def test_route_order_keys_use_the_new_names_and_nothing_else_of_the_routes_changes(self):
        base, profile = load('profile.json', BASE), load('profile.json')
        expected = {'drives': ('started_at_ms', 'id'), 'drive_positions': ('timestamp_ms', 'id'), 'charges': ('started_at_ms', 'id'),
                    'charge_samples': ('timestamp_ms', 'id')}
        for route, old in zip(profile['routes'], base['routes']):
            if route['kind'] in expected:
                self.assertEqual(tuple(route['order']['keys']), expected[route['kind']])
                self.assertEqual(route['window_key'], expected[route['kind']][0])
                route, old = copy.deepcopy(route), copy.deepcopy(old)
                for item in (route, old):
                    item['order']['keys'], item['window_key'] = None, None
            self.assertEqual(route, old)
        self.assertIn('last started_at_ms (8)', profile['cursor']['legacy']['payload_layout'])


class SchemaTests(unittest.TestCase):
    """C6 ST-S46."""

    @classmethod
    def setUpClass(cls):
        cls.resources, cls.base = load('resources.schema.json'), load('resources.schema.json', BASE)

    def definition(self, name, document=None):
        defs = (document or self.resources)['$defs']
        if name == 'car':
            return defs['current']['properties']['car']['anyOf'][0]
        if name == 'settings':
            return self.definition('car', document)['properties']['settings']
        return defs[name]

    def test_property_and_required_counts_and_the_open_item_schemas(self):
        for name, (properties, required) in ITEM_COUNTS.items():
            definition = self.definition(name)
            self.assertEqual((len(definition['properties']), len(definition['required'])), (properties, required), name)
            self.assertNotEqual(definition.get('additionalProperties'), False, name)
        self.assertEqual((len(self.definition('current')['properties']), len(self.definition('current')['required'])), (115, 115))
        self.assertEqual(len(self.definition('car')['properties']), 20)
        self.assertEqual(len(self.definition('settings')['properties']), 16)
        self.assertEqual(set(self.resources['$defs']), set(self.base['$defs']))

    def test_every_new_property_carries_its_old_schema_and_every_old_one_is_deprecated_naming_its_replacement(self):
        groups = {'current': CURRENT_RENAMES, 'car': CAR_RENAMES, 'settings': SETTINGS_RENAMES, **HISTORY_RENAMES}
        total = 0
        for name, renames in groups.items():
            new_definition, old_definition = self.definition(name), self.definition(name, self.base)
            for old, new in renames:
                total += 1
                base_schema = old_definition['properties'][old]
                alias = new_definition['properties'][old]
                self.assertIs(alias['deprecated'], True, (name, old))
                self.assertIn(new, alias['description'])
                self.assertEqual({key: value for key, value in alias.items() if key not in ('deprecated', 'description')}, base_schema)
                self.assertEqual(old in old_definition['required'], new in new_definition['required'], (name, old))
                self.assertIn(old, new_definition['required'] if old in old_definition['required'] else new_definition['properties'])
                if (name, new) == ('car', 'model_code'):
                    self.assertEqual(new_definition['properties'][new], {'anyOf': [base_schema, {'type': 'null'}]})  # nullable: the Hub code can be absent
                else:
                    self.assertEqual(new_definition['properties'][new], base_schema, (name, old))
        self.assertEqual(total, 28 + 4 + 8 + 33)

    def test_the_description_of_each_history_alias_is_the_specified_sentence(self):
        for item, renames in HISTORY_RENAMES.items():
            for old, new in renames:
                self.assertEqual(self.definition(item)['properties'][old]['description'], alias_text(new))
        for old, new in CURRENT_RENAMES + SETTINGS_RENAMES:
            description = (self.definition('current')['properties'] | self.definition('settings')['properties'])[old]['description']
            if old == 'active_route_miles_to_arrival':
                self.assertIn('miles value as given', description)
            else:
                self.assertEqual(description, alias_text(new))
        self.assertIn('stored model text', self.definition('car')['properties']['model']['description'])

    def test_the_additions_are_required_and_nullable_and_the_car_view_matches_r48(self):
        for definition, name, kind in ((self.definition('current'), 'model_name', 'string'), (self.definition('current'), 'model_year', 'integer'),
                                       (self.definition('car'), 'model_name', 'string'), (self.definition('car'), 'model_year', 'integer')):
            self.assertIn(name, definition['required'])
            self.assertEqual(nullable_branches(definition['properties'][name])[0]['type'], kind)
            self.assertEqual(definition['properties'][name]['anyOf'][-1], {'type': 'null'})
        self.assertNotIn('source_model_text', self.definition('car')['properties'])
        self.assertEqual(self.definition('car')['properties']['model']['type'], 'string')  # the alias keeps the non-null stored text
        self.assertEqual(nullable_branches(self.definition('car')['properties']['model_code'])[0]['type'], 'string')

    def test_every_untouched_definition_equals_the_base(self):
        for name in ('vehicles', 'address_provenance', 'elevation_provenance', 'drives', 'positions', 'charges', 'charge_samples', 'health', 'ready'):
            self.assertEqual(self.resources['$defs'][name], self.base['$defs'][name], name)
        self.assertEqual(self.resources['$schema'], self.base['$schema'])

    def test_the_two_provenance_source_enums_equal_the_base(self):
        for name, values in (('address_provenance', ['import', 'geocoder', 'manual']), ('elevation_provenance', ['vehicle', 'import', 'terrain'])):
            self.assertEqual(self.resources['$defs'][name]['properties']['source']['enum'], values)

    def test_every_example_validates_against_this_minor_and_every_item_against_the_base_schema_too(self):
        for name, definition in (('current', 'current'), ('drive', 'drive'), ('drives', 'drives'), ('charge', 'charge'), ('charges', 'charges'),
                                 ('positions', 'positions'), ('charge-samples', 'charge_samples')):
            value = load('examples/' + name + '.json')
            for resources in (self.resources, self.base):
                validator = Draft202012Validator({**resources, '$ref': '#/$defs/' + definition}, format_checker=FormatChecker())
                self.assertEqual([error.message[:80] for error in validator.iter_errors(value)], [], (name, resources is self.base))

    def test_an_item_without_a_new_key_fails_this_minor_and_passes_the_base(self):
        value = load('examples/drive.json')
        del value['started_at_ms']
        mine = Draft202012Validator({**self.resources, '$ref': '#/$defs/drive'}, format_checker=FormatChecker())
        base = Draft202012Validator({**self.base, '$ref': '#/$defs/drive'}, format_checker=FormatChecker())
        self.assertFalse(mine.is_valid(value))
        self.assertTrue(base.is_valid(value))


class SemanticsTests(unittest.TestCase):
    """C6 ST-S47 and ST-S50."""

    @classmethod
    def setUpClass(cls):
        cls.semantics, cls.base = load('field-semantics.json'), load('field-semantics.json', BASE)

    def test_every_new_key_has_an_entry_and_every_alias_entry_is_the_deprecation_sentence_with_its_replacement_nullability_and_unit(self):
        for item, renames in HISTORY_RENAMES.items():
            self.assertEqual(set(self.semantics[item]), set(self.base[item]) | {new for _, new in renames})
            for old, new in renames:
                entry = self.semantics[item][old]
                self.assertEqual(set(entry), {'nullable', 'source', 'unit'})
                self.assertEqual(entry['source'], alias_text(new))
                self.assertEqual((entry['nullable'], entry['unit']), (self.semantics[item][new]['nullable'], self.semantics[item][new]['unit']))
        self.assertEqual(self.semantics['current']['active_route_miles_to_arrival']['unit'], 'miles, provider value unchanged')
        for old, new in CURRENT_RENAMES:
            if old != 'active_route_miles_to_arrival':
                self.assertEqual(self.semantics['current'][old]['source'], alias_text(new), old)
            self.assertIn(new, self.semantics['current'])
        self.assertEqual(len(self.semantics['current']), 115)

    def test_the_hand_over_texts_sit_under_the_new_keys_only_and_the_rest_of_each_entry_moves_unchanged(self):
        for item, keys in HANDOVER_KEYS.items():
            changed = {key for key, entry in self.semantics[item].items()
                       if key in self.base[item] and entry != self.base[item][key] and not str(entry['source']).startswith('Deprecated alias of')}
            moved_changed = {new for old, new in HISTORY_RENAMES[item] if self.semantics[item][new] != self.base[item][old]}
            self.assertEqual(changed | moved_changed, keys, item)
            for old, new in HISTORY_RENAMES[item]:
                self.assertTrue(self.semantics[item][old]['source'].startswith('Deprecated alias of'), old)
        for item in ('position', 'charge_sample'):
            for old, new in HISTORY_RENAMES[item]:
                self.assertEqual(self.semantics[item][new], self.base[item][old])
        for item in ('drive', 'charge'):
            for old, new in HISTORY_RENAMES[item]:
                if new not in HANDOVER_KEYS[item]:
                    self.assertEqual(self.semantics[item][new], self.base[item][old], (item, new))
        self.assertEqual(self.semantics['charge']['tariff_rate']['unit'], TARIFF_RATE_UNIT)
        self.assertEqual(self.semantics['charge']['cost_per_unit']['unit'], TARIFF_RATE_UNIT)
        self.assertIn('copied from the owner', self.semantics['charge']['tariff_type']['source'])
        self.assertIn('Saturated at 32767', self.semantics['drive']['ascent_m']['source'])

    def test_the_two_import_texts_are_exact_and_the_phrase_occurs_exactly_twice(self):
        self.assertEqual(self.semantics['address_provenance']['source']['values']['import'], IMPORT_ADDRESS)
        self.assertEqual(self.semantics['elevation_provenance']['source']['values']['import'], IMPORT_ELEVATION)
        text = (PROFILE / 'field-semantics.json').read_text()
        self.assertEqual(text.count('copied unchanged from the history of an import source'), 2)
        for name in ('address_provenance', 'elevation_provenance'):
            base_values = self.base[name]['source']['values']
            values = self.semantics[name]['source']['values']
            self.assertEqual({key: value for key, value in values.items() if key != 'import'}, {key: value for key, value in base_values.items() if key != 'import'})

    def test_every_other_current_entry_is_the_base_entry_or_a_derived_one_written_from_the_specification(self):
        current, base = self.semantics['current'], self.base['current']
        aliases = {old for old, _ in CURRENT_RENAMES}
        written = set(base) - aliases - {'vehicle_id'}  # every output field of the summary but the identifier: C6 R27, R28 to R41
        self.assertEqual(current['vehicle_id'], base['vehicle_id'])
        for key in written - {'car'}:  # the nested car view keeps its own unit text (its keys are renamed)
            self.assertEqual({k: v for k, v in current[key].items() if k != 'source'}, {k: v for k, v in base[key].items() if k != 'source'}, key)
            self.assertNotIn('materialised identity', current[key]['source'], key)  # no generic 1.2.0 sentence is left
        for key in written | {new for _, new in CURRENT_RENAMES} | {'model_name', 'model_year'}:
            self.assertNotIn('C6 R', current[key]['source'], key)  # no internal unit or requirement label is published
            self.assertNotIn('(C6', current[key]['source'], key)
        transient = set(TRANSIENT_KEYS)
        persistent = set(PERSISTENT_KEYS)
        for key in transient:
            text = current[key]['source']
            self.assertIn(' Transient: null while activity_state is asleep or offline; null while the online interval of the car is open and the statement was '
                          "received more than 60 seconds before that interval began; and null when the statement is older than the Hub's staleness bound "
                          '(the configured offline drive timeout, 600 seconds by default), measured at the later of observed_at_ms and the Hub\'s clock.', text, key)
            self.assertNotIn('Persistent:', text, key)
            self.assertRegex(text, r'\. The newest statement of the field decides; when it is not valid the field is null and no older value is used\. Transient:', key)
        for key in persistent:
            text = current[key]['source']
            self.assertTrue(text.endswith('Persistent: the last known value is kept through sleep however old it is; compare observed_at_ms with your own clock.'), key)
            self.assertNotIn('Transient:', text, key)
        self.assertEqual(len(transient), 25)  # the class T rows of C6 R27: the 21 the base listed, speed_kmh, active_route_km_to_arrival and the two update percents
        # Two entries written out literally, so the builder cannot agree with itself.
        self.assertEqual(current['battery_level']['source'],
                         'Battery level in whole percent, from 0 to 100, from the charge state or from the import source battery level. '
                         'The newest statement of the field decides; when it is not valid the field is null and no older value is used. '
                         'Persistent: the last known value is kept through sleep however old it is; compare observed_at_ms with your own clock.')
        self.assertEqual(current['inside_temp']['source'],
                         'Cabin temperature in degrees Celsius, rounded to 1 decimal, from -999.9 to 999.9. '
                         'The newest statement of the field decides; when it is not valid the field is null and no older value is used. '
                         'Transient: null while activity_state is asleep or offline; null while the online interval of the car is open and the statement was '
                         "received more than 60 seconds before that interval began; and null when the statement is older than the Hub's staleness bound "
                         "(the configured offline drive timeout, 600 seconds by default), measured at the later of observed_at_ms and the Hub's clock.")
        for key in PERSISTENT_ONLY_KEYS:
            text = current[key]['source']
            self.assertTrue(text.endswith(' ' + PERSISTENT_SENTENCE), key)
            self.assertNotIn(NEWEST_SENTENCE, text, key)
            self.assertNotIn('Transient:', text, key)
        # charging_state is class P with the R34 exception: the asleep exception is stated, and so is the persistence of every other state.
        self.assertEqual(current['charging_state']['source'],
                         'The charging state, normalised: Charging, Starting, Stopped, Complete, Disconnected and NoPower in that spelling; Unknown and null give '
                         'null; any other text is passed through trimmed. While activity_state is asleep, Charging and Starting are null. '
                         'Otherwise persistent: the last known value is kept through sleep however old it is; compare observed_at_ms with your own clock.')
        # display_name is a fallback chain (C6 R39): no newest-statement sentence, which would say null where the Hub serves a fallback name.
        self.assertEqual(current['display_name']['source'],
                         'The name the owner gave the car: the newest display name the vehicle list states, else the newest vehicle name of the vehicle state, '
                         'else the stored car name when it is not empty; trimmed; null when none is usable. A source whose newest statement is not usable text '
                         '(not a string, empty after trimming or longer than 65536 characters) passes to the next source. ' + PERSISTENT_SENTENCE)
        self.assertNotIn(NEWEST_SENTENCE, current['display_name']['source'])
        self.assertIn('compare it with your own clock', current['observed_at_ms']['source'])
        self.assertIn('upper bound', current['state_since_ms']['source'])
        self.assertIn('lower bound', current['state_since_ms']['source'])
        self.assertIn('No age rule', current['activity_state']['source'].replace('There is no age rule', 'No age rule'))

    def test_the_other_top_level_texts_are_the_base_texts(self):
        for key in ('null_rule',):
            self.assertEqual(self.semantics[key], self.base[key])


def between(text, left, right):
    """The text between two anchors, or None when the anchors do not hold exactly once."""
    if text.count(left) != 1 or text.count(right) != 1 or text.index(left) + len(left) > text.index(right):
        return None
    return text[text.index(left) + len(left):text.index(right)]


def neutral_problems(base, new):
    """C6 ST-S50 (a) to (d), without spelling the name: the one span of the base text is found between its neighbours in both texts."""
    problems = []
    left, right = 'are whole numbers in ', ' but are typed number'
    span, replaced = between(base['number_rule'], left, right), between(new['number_rule'], left, right)
    if 'whole numbers in the source database' not in new['number_rule']:
        problems.append('number_rule lacks the neutral phrase')
    if not span or ' ' in span or replaced != 'the source database' or base['number_rule'].replace(left + span + right, left + replaced + right) != new['number_rule']:
        problems.append('number_rule differs from the base by more than the one span')
    trace_base, trace_new = base['source_trace'], new['source_trace']
    if len(trace_new) != 5:
        problems.append('source_trace does not have five entries')
        return problems
    if trace_new[:4] != trace_base[:4]:
        problems.append('source_trace entries 0 to 3 differ')
    if 'Hub import projection: ' not in trace_new[4]:
        problems.append('source_trace entry 4 lacks the neutral label')
    left, right = 'Hub import', ' projection: '
    span, replaced = between(trace_base[4], left, right), between(trace_new[4], left, right)
    if not span or not span.startswith('/') or ' ' in span or replaced != '' or trace_base[4].replace(left + span + right, left + right) != trace_new[4]:
        problems.append('source_trace entry 4 differs from the base by more than the span and its slash')
    return problems


class NeutralEntryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.base, cls.new = load('field-semantics.json', BASE), load('field-semantics.json')

    def test_the_two_carried_forward_entries_are_the_neutral_transformation(self):
        self.assertEqual(neutral_problems(self.base, self.new), [])
        self.assertEqual(self.new['number_rule'], 'position.power and the charge_sample fields charger_power_kw, charger_voltage, charger_actual_current '
                         'and charger_pilot_current are whole numbers in the source database but are typed number, because the page routes serve the '
                         'materialised projections of the Hub, which hold them as floating point: a whole value may carry a fraction digit (12 or 12.0). '
                         'Clients accept both. Retyping these fields later is a new major profile.')
        self.assertTrue(self.new['source_trace'][4].startswith('Hub import projection: '))

    def test_each_defect_of_the_two_entries_is_reported(self):
        def mutated(change):
            document = copy.deepcopy(self.new)
            change(document)
            return neutral_problems(self.base, document)
        self.assertEqual(neutral_problems(self.base, self.base).count('number_rule lacks the neutral phrase'), 1)
        self.assertTrue(mutated(lambda d: d.__setitem__('number_rule', self.base['number_rule'])))
        self.assertTrue(mutated(lambda d: d['source_trace'].__setitem__(4, self.base['source_trace'][4])))
        self.assertTrue(mutated(lambda d: d['source_trace'].__setitem__(4, d['source_trace'][4].replace('Hub import projection', 'Hub import/ projection'))))
        self.assertTrue(mutated(lambda d: d['source_trace'].pop()))
        self.assertTrue(mutated(lambda d: d['source_trace'].insert(0, d['source_trace'].pop())))
        self.assertTrue(mutated(lambda d: d['source_trace'].append(d['source_trace'][4])))
        self.assertTrue(mutated(lambda d: d.__setitem__('number_rule', d['number_rule'].replace('Clients accept both', 'Clients accept either'))))
        self.assertTrue(mutated(lambda d: d['source_trace'].__setitem__(0, d['source_trace'][0] + ' ')))


class ReadinessTests(unittest.TestCase):
    """C6 ST-S54 (a): nothing of the collection minor, and every readiness entry equals the base."""

    def test_the_readiness_entries_equal_the_base_as_json_values(self):
        base_openapi, openapi = load('openapi.json', BASE), load('openapi.json')
        self.assertEqual(openapi['paths']['/readyz'], base_openapi['paths']['/readyz'])
        self.assertEqual(load('resources.schema.json')['$defs']['ready'], load('resources.schema.json', BASE)['$defs']['ready'])
        self.assertEqual(load('examples/ready.json'), load('examples/ready.json', BASE))
        base_cases = {case['id']: case for case in load('cases.json', BASE)['cases']}
        cases = {case['id']: case for case in load('cases.json')['cases']}
        self.assertEqual(cases['ready-success'], base_cases['ready-success'])
        profile, base_profile = load('profile.json'), load('profile.json', BASE)
        self.assertEqual([route for route in profile['routes'] if route['kind'] == 'ready'], [route for route in base_profile['routes'] if route['kind'] == 'ready'])
        self.assertEqual(profile['routes'][2]['path'], '/readyz')

    def test_no_collection_object_gaps_resource_detail_or_cadence_field_exists(self):
        resources = load('resources.schema.json')
        self.assertEqual(set(resources['$defs']), {'vehicles', 'current', 'address_provenance', 'elevation_provenance', 'drive', 'drives', 'position',
                                                   'positions', 'charge', 'charges', 'charge_sample', 'charge_samples', 'health', 'ready'})
        self.assertNotIn('collection', resources['$defs']['current']['properties'])
        for branch in resources['$defs']['ready']['oneOf']:
            self.assertEqual(set(branch['properties']), {'status', 'reason'} if 'reason' in branch['properties'] else {'status'})
        text = ''.join(path.read_text() for path in PROFILE.rglob('*.json'))
        for word in ('collectorCadenceError', 'collector_cadence_invalid', '"detail"', '/gaps', 'cadence'):
            self.assertNotIn(word, text, word)
        self.assertEqual([route['path'] for route in load('profile.json')['routes']], [route['path'] for route in load('profile.json', BASE)['routes']])
        self.assertEqual(load('profile.json')['capabilities'], load('profile.json', BASE)['capabilities'])


class CaseTests(unittest.TestCase):
    def test_the_case_set_is_the_base_fifty_plus_the_nine_new_ones(self):
        base = load('cases.json', BASE)
        cases = load('cases.json')
        self.assertEqual(len(base['cases']), BASE_CASE_COUNT)
        self.assertEqual([case['id'] for case in cases['cases']][:BASE_CASE_COUNT], [case['id'] for case in base['cases']])
        self.assertEqual([case['id'] for case in cases['cases']][BASE_CASE_COUNT:], NEW_CASES)
        self.assertEqual(cases['cases'][:BASE_CASE_COUNT], base['cases'])
        self.assertEqual(set(cases['inputs']) - set(base['inputs']), {'full_page_vehicle_id', 'full_page_drive_id', 'full_page_charge_id'})

    def test_the_current_alias_case_lists_the_28_4_and_8_pairs_and_the_two_exceptions(self):
        case = {case['id']: case for case in load('cases.json')['cases']}['current-aliases']
        self.assertEqual((case['kind'], case['method'], case['status'], case['route'], case['inputs']),
                         ('current', 'GET', 200, '/v1/vehicles/{vehicle_id}/current', ['bearer', 'vehicle_id']))
        self.assertEqual([(pair['scope'], pair['alias'], pair['replacement']) for pair in case['aliases']],
                         [('current',) + pair for pair in CURRENT_RENAMES] + [('car',) + pair for pair in CAR_RENAMES]
                         + [('settings',) + pair for pair in SETTINGS_RENAMES])
        self.assertEqual(len(case['aliases']), 28 + 4 + 8)
        self.assertEqual(case['exceptions'], ['current.active_route_miles_to_arrival', 'current.car.model'])

    def test_the_alias_cases_list_exactly_the_33_pairs_and_name_their_route_and_inputs(self):
        cases = {case['id']: case for case in load('cases.json')['cases']}
        expected = {'drive-aliases': ('drive', 'drive'), 'charge-aliases': ('charge', 'charge'), 'position-aliases': ('drive_positions', 'position'),
                    'charge-sample-aliases': ('charge_samples', 'charge_sample')}
        total = 0
        for case_id, (kind, item) in expected.items():
            case = cases[case_id]
            self.assertEqual((case['kind'], case['method'], case['status']), (kind, 'GET', 200))
            self.assertEqual([(pair['alias'], pair['replacement']) for pair in case['aliases']], HISTORY_RENAMES[item])
            total += len(case['aliases'])
        self.assertEqual(total, 33)
        for case_id in ('drives-full-page', 'charges-full-page', 'positions-full-page', 'charge-samples-full-page'):
            self.assertIn('full_page_vehicle_id', cases[case_id]['inputs'])
            self.assertIn('1 MiB', cases[case_id]['description'])
        self.assertIn('full_page_drive_id', cases['positions-full-page']['inputs'])
        self.assertIn('full_page_charge_id', cases['charge-samples-full-page']['inputs'])

    def test_the_profile_lists_every_alias_and_names_the_two_exceptions(self):
        aliases = load('profile.json')['deprecated_aliases']
        self.assertEqual(aliases['current'], dict(CURRENT_RENAMES))
        self.assertEqual(aliases['car'], dict(CAR_RENAMES))
        self.assertEqual(aliases['settings'], dict(SETTINGS_RENAMES))
        for item, renames in HISTORY_RENAMES.items():
            self.assertEqual(aliases[item], dict(renames))
        self.assertEqual(sorted(aliases['exceptions']), ['current.active_route_miles_to_arrival', 'current.car.model'])


class OfflineTests(unittest.TestCase):
    def test_offline_checks_pass_and_include_the_alias_check(self):
        results = conf.check_offline()
        self.assertEqual([item['status'] for item in results], ['passed'] * len(results), results)
        self.assertIn('offline/aliases', [item['case_id'] for item in results])

    def test_the_alias_check_reports_each_defect(self):
        def check(change):
            temp = tempfile.TemporaryDirectory()
            self.addCleanup(temp.cleanup)
            root = Path(temp.name) / '1.3.0'
            shutil.copytree(PROFILE, root)
            for name, edit in change.items():
                document = json.loads((root / name).read_text())
                edit(document)
                (root / name).write_text(json.dumps(document, indent=2, sort_keys=True) + '\n')
            reseal(root)
            return {item['case_id']: item['status'] for item in conf.check_offline(root)}['offline/aliases']

        def resources(change):
            return {'resources.schema.json': lambda d: change(d['$defs'])}
        self.assertEqual(check({}), 'passed')
        self.assertEqual(check(resources(lambda defs: defs['drive']['properties']['ascent'].pop('deprecated'))), 'failed')
        self.assertEqual(check(resources(lambda defs: defs['drive']['properties']['ascent'].__setitem__('description', 'Deprecated.'))), 'failed')
        self.assertEqual(check(resources(lambda defs: defs['drive']['properties'].__setitem__('ascent_m', {'type': 'string'}))), 'failed')
        self.assertEqual(check(resources(lambda defs: defs['drive']['required'].remove('ascent_m'))), 'failed')
        self.assertEqual(check({'cases.json': lambda d: d['cases'][-5]['aliases'].pop()}), 'failed')
        self.assertEqual(check({'cases.json': lambda d: next(c for c in d['cases'] if c['id'] == 'current-aliases')['aliases'].pop()}), 'failed')
        self.assertEqual(check({'cases.json': lambda d: next(c for c in d['cases'] if c['id'] == 'current-aliases')['exceptions'].pop()}), 'failed')
        self.assertEqual(check({'examples/drive.json': lambda d: d.__setitem__('ascent_m', 7)}), 'failed')
        self.assertEqual(check({'examples/current.json': lambda d: d.__setitem__('speed_kmh', 7)}), 'failed')


# ---------------------------------------------------------------------------
# The new native cases against an in-memory client
# ---------------------------------------------------------------------------
VEHICLE = '11111111-1111-4111-8111-111111111111'
FIRST_TIME = 1_788_566_000_000
CAP_ITEMS = 40  # the number of items that fit in the byte cap of the in-memory Hub: every item is padded to the same size, so 40 fit and a 41st would not
EARLY_ITEMS = 20  # a Hub that cuts its pages at half the cap
ITEM_BYTES = (conf.MAX_BYTES - 700) // CAP_ITEMS
TOTAL = 130


def compact(value):
    return json.dumps(value, separators=(',', ':'))


def padded(kind, value):
    """The item grown through one free text member to exactly ITEM_BYTES compact bytes, so the byte cap cuts a page at a known item count."""
    member = {'drives': ('start_address',), 'charges': ('address',), 'drive_positions': ('elevation_provenance', 'dataset'), 'charge_samples': ('charge_cable',)}[kind]
    if len(member) == 2:
        value[member[0]] = {'source': 'vehicle', 'dataset': '', 'dataset_version': None, 'tile': None}
        target = value[member[0]]
    else:
        target = value
    target[member[-1]] = ''
    target[member[-1]] = 'x' * (ITEM_BYTES - len(compact(value).encode()))
    return value


def item_for(kind, number):
    """One synthetic item of a page route; every alias holds its replacement's value."""
    if kind == 'drives':
        value = json.loads((PROFILE / 'examples/drive.json').read_text())
        start = FIRST_TIME - number * 1000
        value.update(id=number, started_at_ms=start, start_date_ms=start, ended_at_ms=start + 100, end_date_ms=start + 100)
    elif kind == 'charges':
        value = json.loads((PROFILE / 'examples/charges.json').read_text())['items'][0]
        start = FIRST_TIME - number * 1000
        value.update(id=number, started_at_ms=start, start_date_ms=start, ended_at_ms=start + 100, end_date_ms=start + 100)
    elif kind == 'drive_positions':
        value = json.loads((PROFILE / 'examples/positions.json').read_text())['items'][0]
        value.update(id=number, drive_id=7, timestamp_ms=FIRST_TIME + number, date_ms=FIRST_TIME + number)
    else:
        value = json.loads((PROFILE / 'examples/charge-samples.json').read_text())['items'][0]
        value.update(id=number, charge_id=9, timestamp_ms=FIRST_TIME + number)
    return padded(kind, value)


class StubClient:
    """What a Hub of this profile answers to the new cases, in memory; `fault` selects one defect."""

    KINDS = {'drives': (1, 'drives'), 'charges': (2, 'charges'), 'drive_positions': (3, 'drive_positions'), 'charge_samples': (4, 'charge_samples')}

    def __init__(self, profile, fault=None, total=TOTAL):
        self.profile, self.fault, self.total = profile, fault, total
        self.requests = 0

    def response(self, status, document):
        body = json.dumps(document, separators=(',', ':')).encode()
        headers = {'content-type': 'application/json', 'cache-control': 'no-store', 'etag': '"' + hashlib.sha256(body).hexdigest() + '"'}
        return conf.Response(status, headers, body)

    def cursor(self, kind, last):
        payload = bytes([1, self.KINDS[kind][0]]) + bytes.fromhex(VEHICLE.replace('-', ''))
        scope = {'drive_positions': 7, 'charge_samples': 9}.get(kind, 0)
        key_a = last['started_at_ms'] if kind in ('drives', 'charges') else last['timestamp_ms']
        import struct
        payload += struct.pack('>qqqqq', scope, 0, I64_MAX, key_a, last['id'])
        return 'tqp1.' + payload.hex() + '.' + '0' * 64

    def exchange(self, method, path, *, bearer=None, body=None, headers=None):
        self.requests += 1
        route, _, query = path.partition('?')
        options = dict(part.split('=', 1) for part in query.split('&') if part)
        segments = route.strip('/').split('/')
        if segments[-1] == 'current':
            return self.response(200, self.current())
        if len(segments) == 4:
            kind = segments[3]
        elif len(segments) == 6:
            kind = 'drive_positions' if segments[3] == 'drives' else 'charge_samples'
        elif len(segments) == 5:  # one drive or one charge
            kind = segments[3]
            value = item_for(kind, int(segments[4]))
            if self.fault == 'alias':
                value[HISTORY_RENAMES['drive' if kind == 'drives' else 'charge'][12 if kind == 'drives' else 0][0]] = 'wrong'
            if self.fault == 'missing':
                value.pop('started_at_ms')
            return self.response(200, value)
        else:
            raise AssertionError(path)
        limit = int(options.get('limit', self.profile.routes[kind]['limit']['default']))
        after = 0
        if 'cursor' in options:
            from conformance.hub_http_13 import decode_cursor
            after = decode_cursor(options['cursor'])['key_b']
        cut = min(limit, (EARLY_ITEMS if self.fault == 'early' else CAP_ITEMS) if limit > 60 else limit)
        numbers = [number for number in range(after + 1, self.total + 1)][:cut]
        if self.fault == 'skip' and limit > 60 and after:
            numbers = numbers[1:]
        if self.fault == 'repeat' and limit > 60 and after:
            numbers = [after] + numbers[:-1]
        items = [item_for(kind, number) for number in numbers]
        if self.fault == 'alias' and kind in ('drive_positions', 'charge_samples') and items:
            items[0]['speed_kmh' if kind == 'drive_positions' else 'battery_range_km'] = -1
        last = numbers[-1] if numbers else after
        more = bool(items) and last < self.total
        if kind in ('drive_positions', 'charge_samples') and limit > 60 and self.fault == 'no_cursor':
            more = False
        return self.response(200, {'items': items, 'next_cursor': self.cursor(kind, items[-1]) if more else None})

    def current(self):
        document = json.loads((PROFILE / 'examples/current.json').read_text())
        document['vehicle_id'] = VEHICLE
        if self.fault == 'current':
            document['speed'] = 5
        if self.fault == 'current_car':
            document['car']['source_vid'] = 5
        if self.fault == 'current_settings':
            document['car']['settings']['enabled'] = not document['car']['settings']['collection_enabled']
        if self.fault == 'miles_exception':  # the one alias that keeps its own value may differ, but must be present
            document['active_route_miles_to_arrival'] = 99.5
        if self.fault == 'model_exception':
            document['car']['model'] = 'stored text'
        if self.fault == 'missing_alias':
            del document['car']['settings']['battery_lfp']
        if self.fault == 'model_missing':
            del document['car']['model']
        if self.fault == 'no_car':
            document['car'] = None
        return document


def native_run(fault=None, total=TOTAL, inputs=None):
    profile = conf.Profile(PROFILE)
    given = {'bearer': 'a' * 64, 'vehicle_id': VEHICLE, 'drive_id': 7, 'charge_id': 9, 'full_page_vehicle_id': VEHICLE,
             'full_page_drive_id': 7, 'full_page_charge_id': 9}
    given.update(inputs or {})
    run = conf.NativeRun(profile, {'inputs': given, 'endpoint': 'https://127.0.0.1:1', 'certificate_path': '/x', 'tls_pin': None}, client=StubClient(profile, fault, total))
    run.advertised = None
    for case_id in NEW_CASES + ['current-success']:
        run.execute(case_id)
    return {case_id: result['status'] for case_id, result in run.results.items()}, run


class NativeCaseTests(unittest.TestCase):
    def test_a_hub_that_follows_the_contract_passes_every_new_case_and_the_current_alias_check(self):
        statuses, run = native_run()
        self.assertEqual(statuses, {case_id: 'passed' for case_id in NEW_CASES + ['current-success']}, run.results)

    def test_a_missing_input_makes_a_case_not_run_naming_it(self):
        profile = conf.Profile(PROFILE)
        descriptor = {'inputs': {'bearer': 'a' * 64, 'vehicle_id': VEHICLE}, 'endpoint': 'https://127.0.0.1:1', 'certificate_path': '/x', 'tls_pin': None}
        run = conf.NativeRun(profile, descriptor, client=StubClient(profile))
        run.execute('positions-full-page')
        run.execute('drive-aliases')
        self.assertEqual(run.results['positions-full-page']['status'], 'not_run')
        self.assertEqual(run.results['positions-full-page']['missing'], ['full_page_vehicle_id', 'full_page_drive_id'])
        self.assertEqual(run.results['drive-aliases']['missing'], ['drive_id'])

    def test_a_fixture_that_fits_in_one_page_is_not_run_never_passed(self):
        statuses, run = native_run(total=30)
        for case_id in ('drives-full-page', 'charges-full-page', 'positions-full-page', 'charge-samples-full-page'):
            self.assertEqual(statuses[case_id], 'not_run', case_id)
            self.assertTrue(run.results[case_id]['missing'][0].startswith('data: '))
        for case_id in ('drive-aliases', 'position-aliases'):
            self.assertEqual(statuses[case_id], 'passed')

    def test_every_defect_of_an_alias_is_reported(self):
        statuses, _ = native_run('alias')
        self.assertEqual(statuses['drive-aliases'], 'failed')
        self.assertEqual(statuses['charge-aliases'], 'failed')
        self.assertEqual(statuses['position-aliases'], 'failed')
        self.assertEqual(statuses['charge-sample-aliases'], 'failed')
        statuses, _ = native_run('missing')
        self.assertEqual(statuses['drive-aliases'], 'failed')
        self.assertEqual(statuses['charge-aliases'], 'failed')

    def test_an_alias_that_differs_on_the_current_body_fails_the_current_alias_case_only(self):
        for fault in ('current', 'current_car', 'current_settings'):
            statuses, run = native_run(fault)
            self.assertEqual(statuses['current-aliases'], 'failed', fault)
            self.assertIn('current aliases', run.results['current-aliases']['detail'], fault)
            self.assertEqual(statuses['current-success'], 'passed', fault)  # the base case is the 1.2.0 one: the body is valid, only an alias is wrong
        for fault in ('missing_alias', 'model_missing'):
            statuses, _ = native_run(fault)
            self.assertEqual(statuses['current-aliases'], 'failed', fault)

    def test_the_two_exceptions_may_differ_from_their_replacements_but_must_be_present(self):
        for fault in ('miles_exception', 'model_exception'):
            statuses, run = native_run(fault)
            self.assertEqual(statuses['current-aliases'], 'passed', (fault, run.results['current-aliases']))
        statuses, _ = native_run('model_missing')
        self.assertEqual(statuses['current-aliases'], 'failed')

    def test_a_current_body_without_a_car_is_not_run_never_passed(self):
        statuses, run = native_run('no_car')
        self.assertEqual(statuses['current-aliases'], 'not_run')
        self.assertTrue(run.results['current-aliases']['missing'][0].startswith('data: '))

    def test_a_page_that_skips_repeats_or_loses_its_cursor_at_the_byte_cap_fails_each_full_page_case(self):
        for fault in ('skip', 'repeat'):
            statuses, run = native_run(fault)
            for case_id in ('drives-full-page', 'charges-full-page', 'positions-full-page', 'charge-samples-full-page'):
                self.assertEqual(statuses[case_id], 'failed', (fault, case_id, run.results[case_id]))
        statuses, _ = native_run('no_cursor')
        self.assertEqual(statuses['positions-full-page'], 'not_run')
        self.assertEqual(statuses['charge-samples-full-page'], 'not_run')

    def test_a_hub_that_cuts_its_pages_before_the_byte_cap_fails_each_full_page_case(self):
        statuses, run = native_run('early')
        for case_id in ('drives-full-page', 'charges-full-page', 'positions-full-page', 'charge-samples-full-page'):
            self.assertEqual(statuses[case_id], 'failed', (case_id, run.results[case_id]))
            self.assertIn('before the byte cap', run.results[case_id]['detail'], case_id)
        # The same pages cut by the cap itself pass: the first page leaves no room for the next item.
        statuses, _ = native_run()
        self.assertEqual({statuses[case_id] for case_id in ('drives-full-page', 'positions-full-page')}, {'passed'})

    def test_the_in_memory_hub_pages_hold_forty_items_that_fill_the_cap_and_a_forty_first_that_would_not_fit(self):
        profile = conf.Profile(PROFILE)
        client = StubClient(profile)
        for kind in ('drives', 'charges', 'drive_positions', 'charge_samples'):
            sizes = {len(compact(item_for(kind, number)).encode()) for number in range(1, 130)}
            self.assertEqual(sizes, {ITEM_BYTES}, kind)
            raw = client.exchange('GET', '/v1/vehicles/%s/%s' % (VEHICLE, {'drives': 'drives', 'charges': 'charges'}.get(kind, 'drives/7/positions' if kind == 'drive_positions' else 'charges/9/samples')) + '?limit=' + str(profile.routes[kind]['limit']['max'])).body
            self.assertEqual(len(json.loads(raw)['items']), CAP_ITEMS, kind)
            self.assertLessEqual(len(raw), conf.MAX_BYTES, kind)
            self.assertGreater(len(raw) + 1 + ITEM_BYTES, conf.MAX_BYTES, kind)

    def test_the_new_cases_are_in_the_execution_order_and_have_methods(self):
        for case_id in NEW_CASES:
            self.assertIn(case_id, conf.EXECUTION_ORDER)
            self.assertTrue(hasattr(conf.NativeRun, 'case_' + case_id.replace('-', '_')), case_id)
        self.assertEqual(set(conf.EXECUTION_ORDER), {case['id'] for case in load('cases.json')['cases']})
        self.assertEqual(len(conf.EXECUTION_ORDER), BASE_CASE_COUNT + 9)


class RunnerTests(unittest.TestCase):
    def test_the_runner_runs_this_profile_by_name_and_refuses_to_mix_it(self):
        result = subprocess.run([str(ROOT / 'conformance/run'), '--profile', PROFILE_ID, '--json'], capture_output=True, timeout=120)
        summary = json.loads(result.stdout.decode().splitlines()[-1])
        self.assertEqual(summary['profile_id'], PROFILE_ID)
        self.assertEqual((summary['failed'], summary['runs']), (0, 12 + BASE_CASE_COUNT + 9))
        mixed = subprocess.run([str(ROOT / 'conformance/run'), '--profile', PROFILE_ID, '--profile', 'hub-sync-v1@1.3.0'], capture_output=True, timeout=120)
        self.assertEqual(mixed.returncode, 2)
        self.assertIn('must run separately', mixed.stdout.decode())

    def test_the_base_profile_still_runs_by_name(self):
        result = subprocess.run([str(ROOT / 'conformance/run'), '--profile', 'hub-http-v1@1.2.0', '--json'], capture_output=True, timeout=120)
        summary = json.loads(result.stdout.decode().splitlines()[-1])
        self.assertEqual((summary['profile_id'], summary['failed']), ('hub-http-v1@1.2.0', 0))


if __name__ == '__main__':
    unittest.main()
