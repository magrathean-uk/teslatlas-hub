#!/usr/bin/env python3
"""Export the independently authored hub-http-v1@1.2.0 wire contract (Apache-2.0).

This authoring source contains public field/route facts, not Hub implementation.
Do not generate schemas or expected values by importing server source. It is
self-contained (standard library only) so the earlier profile generators can be
retired without touching it; the 1.0.0 and 1.1.0 profile trees are never written.

    python tools/build_hub_http_profile_12.py          # write profiles/hub-http-v1/1.2.0
    python tools/build_hub_http_profile_12.py --check  # fail when the tree differs
"""
import argparse
import hashlib
import hmac
import json
import struct
import urllib.parse
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1] / 'profiles/hub-http-v1/1.2.0'
VERSION = '1.2.0'
ID = 'hub-http-v1@' + VERSION
DRAFT = 'https://json-schema.org/draft/2020-12/schema'
I64_MIN = -(2 ** 63)
I64_MAX = 2 ** 63 - 1
MAX_PAGE_BYTES = 1048576

INT = {'type': 'integer', 'format': 'int64', 'minimum': I64_MIN, 'maximum': I64_MAX}  # format is an annotation: no validator in this package assigns it a check
STR = {'type': 'string', 'maxLength': 65536}
UUID = {'type': 'string', 'format': 'uuid', 'pattern': r'^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'}
NUM = {'type': 'number'}
BOOL = {'type': 'boolean'}
HEX64 = {'type': 'string', 'pattern': '^[0-9a-f]{64}$'}
URI = {'type': 'string', 'format': 'uri', 'maxLength': 4096}
# A claim's device_name: 1 to 128 UTF-8 bytes, no control character (the Cc category: U+0000 to U+001F and U+007F to U+009F). maxLength counts
# characters and cannot state the byte limit; it is a necessary condition (a name over 128 characters is over 128 bytes). The pattern is a
# negative look-ahead over the whole string so that no anchor quirk (a trailing newline before $) lets a control character through.
DEVICE_NAME = {'type': 'string', 'minLength': 1, 'maxLength': 128, 'pattern': r'^(?![\s\S]*[\u0000-\u001f\u007f-\u009f])',
               'description': 'A name of 1 to 128 UTF-8 bytes with no control character (U+0000 to U+001F, U+007F to U+009F). maxLength counts characters and cannot '
                              'state the byte limit: a name of at most 128 characters can still be over 128 bytes, and is refused all the same (422, nothing consumed).'}


def nullable(schema):
    return {'anyOf': [schema, {'type': 'null'}]}


def ref(target):
    return {'$ref': target}


def open_obj(fields, required=None):
    """Response object: additive fields are tolerated by clients."""
    return {'type': 'object', 'properties': fields, 'required': list(fields) if required is None else list(required)}


def closed_obj(fields, required=None):
    """Request and fixture objects stay closed."""
    return {**open_obj(fields, required), 'additionalProperties': False}


def schema(name, content):
    return {'$schema': DRAFT, '$id': 'urn:teslatlas:hub-http-v1:' + VERSION + ':' + name, **content}


def fields(groups):
    types = {'integer': INT, 'number': NUM, 'boolean': BOOL, 'string': STR}
    return {name: nullable(types[kind]) for kind, names in groups for name in names.split()}


# ---------------------------------------------------------------------------
# Resource field tables (names and types only; order is the documented order).
# ---------------------------------------------------------------------------
CURRENT_GROUPS = [
    ('integer', 'observed_at_ms since battery_level usable_battery_level speed scheduled_charging_start_time charge_limit_soc charger_phases charge_current_request charge_current_request_max center_display_state sun_roof_percent_open download_perc install_perc'),
    ('number', 'latitude longitude heading ideal_battery_range_km est_battery_range_km rated_battery_range_km charge_energy_added outside_temp inside_temp charger_power odometer time_to_full_charge charger_actual_current charger_voltage elevation power tpms_pressure_fl tpms_pressure_fr tpms_pressure_rl tpms_pressure_rr active_route_latitude active_route_longitude active_route_energy_at_arrival active_route_miles_to_arrival active_route_minutes_to_arrival active_route_traffic_minutes_delay'),
    ('boolean', 'healthy is_climate_on is_preconditioning locked sentry_mode plugged_in windows_open driver_front_window_open driver_rear_window_open passenger_front_window_open passenger_rear_window_open doors_open driver_front_door_open driver_rear_door_open passenger_front_door_open passenger_rear_door_open charge_port_door_open update_available is_user_present trunk_open frunk_open tpms_soft_warning_fl tpms_soft_warning_fr tpms_soft_warning_rl tpms_soft_warning_rr service_mode sun_roof_installed'),
    ('string', 'display_name state charging_state shift_state version update_version update_status geofence model trim_badging exterior_color wheel_type spoiler_type climate_keeper_mode active_route_destination sun_roof_state'),
]
DRIVE_GROUPS = [
    ('integer', 'duration_min speed_max start_soc end_soc ascent descent'),
    ('number', 'distance_km efficiency outside_temp_avg inside_temp_avg power_max power_min start_ideal_range_km end_ideal_range_km start_latitude start_longitude end_latitude end_longitude start_rated_range_km end_rated_range_km'),
    ('string', 'start_address end_address start_geofence end_geofence'),
]
POSITION_FIELDS = [  # documented order; types follow the public TeslaMate column types except where field-semantics number_rule says otherwise
    ('latitude', NUM), ('longitude', NUM), ('speed', INT), ('power', NUM), ('battery_level', INT),
    ('usable_battery_level', INT), ('elevation', INT), ('odometer', NUM), ('ideal_battery_range_km', NUM),
    ('est_battery_range_km', NUM), ('rated_battery_range_km', NUM), ('inside_temp', NUM), ('outside_temp', NUM),
    ('is_climate_on', BOOL), ('fan_status', INT), ('driver_temp_setting', NUM), ('passenger_temp_setting', NUM),
    ('battery_heater', BOOL), ('battery_heater_on', BOOL), ('tpms_pressure_fl', NUM), ('tpms_pressure_fr', NUM),
    ('tpms_pressure_rl', NUM), ('tpms_pressure_rr', NUM),
]
CHARGE_FIELDS = [
    ('duration_min', INT), ('charge_energy_added', NUM), ('charge_energy_used_kwh', NUM), ('start_battery_level', INT),
    ('end_battery_level', INT), ('start_ideal_range_km', NUM), ('end_ideal_range_km', NUM), ('start_rated_range_km', NUM),
    ('end_rated_range_km', NUM), ('start_latitude', NUM), ('start_longitude', NUM), ('address', STR),
    ('location_name', STR), ('geofence', STR), ('fast_charger_type', STR), ('is_dc', BOOL),
    ('charge_rate_km_per_hour', NUM), ('max_charger_power_kw', NUM), ('outside_temp_avg', NUM), ('cost', NUM),
    ('billing_type', STR), ('cost_per_unit', NUM), ('session_fee', NUM),
]
CHARGE_SAMPLE_FIELDS = [
    ('battery_level', INT), ('usable_battery_level', INT), ('charge_energy_added_kwh', NUM), ('charger_power_kw', NUM),
    ('charger_voltage', NUM), ('charger_actual_current', NUM), ('charger_pilot_current', NUM), ('charger_phases', INT),
    ('ideal_range_km', NUM), ('rated_range_km', NUM), ('outside_temp_c', NUM), ('battery_heater_on', BOOL),
    ('fast_charger_present', BOOL), ('fast_charger_brand', STR), ('fast_charger_type', STR), ('charge_cable', STR),
]

CAPABILITIES = ['query.vehicles', 'query.current', 'query.drives', 'query.charges', 'query.charge.samples',
                'query.positions', 'query.enrichment', 'sync.packs', 'auth.revoke']
CURSOR_KEY_CAPABILITIES = ['query.drives', 'query.charges', 'query.positions', 'query.charge.samples', 'sync.packs']
ERROR_CODES = ['invalid_query', 'invalid_time_range', 'invalid_limit', 'invalid_cursor', 'vehicle_not_found',
               'service_unavailable', 'drive_not_found', 'charge_not_found']
QUERY_ERRORS = ['invalid_query', 'invalid_time_range', 'invalid_limit', 'invalid_cursor']
AUTH_STORE_FORM = 'empty authentication-store failure'
QUERY_FORM = 'JSON service_unavailable query failure'
TIMEOUT_FORM = 'empty handler time-budget expiry'

# Cursor construction facts (public wire layout; the key is never published).
CURSOR_KIND = {'drives': 1, 'charges': 2, 'positions': 3, 'charge_samples': 4}
CURSOR_DOMAIN = b'teslatlas-hub/public-query-cursor/v1\x00'
EXAMPLE_CURSOR_KEY = bytes(range(32))  # synthetic key used only for the example cursors


def tqp1_cursor(kind, vehicle_id, scope_id, from_ms, to_ms, key_a, key_b):
    payload = bytes([1, kind]) + uuid.UUID(vehicle_id).bytes + struct.pack('>qqqqq', scope_id, from_ms, to_ms, key_a, key_b)
    assert len(payload) == 58
    tag = hmac.new(EXAMPLE_CURSOR_KEY, CURSOR_DOMAIN + payload, hashlib.sha256).digest()
    return 'tqp1.' + payload.hex() + '.' + tag.hex()


# ---------------------------------------------------------------------------
# Schemas
# ---------------------------------------------------------------------------
def build_schemas():
    car_fields = {'id': INT, 'name': STR, 'model': STR}
    car_fields.update({x: nullable(STR) for x in 'vin trim_badging marketing_name exterior_color wheel_type spoiler_type firmware_version'.split()})
    car_fields.update({x: nullable(INT) for x in ('source_eid', 'source_vid')})
    car_fields['efficiency_wh_per_km'] = nullable(NUM)
    car_fields['settings'] = open_obj({**{x: BOOL for x in 'enabled use_streaming_api suspend_min_resolved req_not_unlocked free_supercharging lfp_battery'.split()}, 'suspend_after_idle_min': INT, 'suspend_min': INT})
    current = open_obj({'vehicle_id': UUID, **fields(CURRENT_GROUPS), 'car': nullable(open_obj(car_fields))})

    address_provenance = open_obj({'source': {'enum': ['import', 'geocoder', 'manual']}, 'provider': nullable(STR),
                                   'endpoint_host': nullable(STR), 'language': nullable(STR), 'licence': nullable(STR),
                                   'looked_up_at_ms': nullable(INT)})
    elevation_provenance = open_obj({'source': {'enum': ['vehicle', 'import', 'terrain']}, 'dataset': nullable(STR),
                                     'dataset_version': nullable(STR), 'tile': nullable(STR)})

    def item(first, tail, optional=None):
        optional = optional or {}
        properties = {**first, **{name: nullable(kind) for name, kind in tail}, **optional}
        return open_obj(properties, [name for name in properties if name not in optional])

    drive_properties = {'id': INT, 'vehicle_id': UUID, 'start_date_ms': INT, 'end_date_ms': INT, **fields(DRIVE_GROUPS)}
    optional_drive = {'start_address_provenance': nullable(ref('#/$defs/address_provenance')),
                      'end_address_provenance': nullable(ref('#/$defs/address_provenance'))}
    drive = open_obj({**drive_properties, **optional_drive}, list(drive_properties))
    position = item({'id': INT, 'drive_id': INT, 'date_ms': INT}, POSITION_FIELDS,
                    {'elevation_provenance': nullable(ref('#/$defs/elevation_provenance'))})
    charge = item({'id': INT, 'vehicle_id': UUID, 'start_date_ms': INT, 'end_date_ms': INT}, CHARGE_FIELDS,
                  {'address_provenance': nullable(ref('#/$defs/address_provenance'))})
    charge_sample = item({'id': INT, 'charge_id': INT, 'timestamp_ms': INT}, CHARGE_SAMPLE_FIELDS)

    def page(item_name, maximum):
        return open_obj({'items': {'type': 'array', 'maxItems': maximum, 'items': ref('#/$defs/' + item_name)},
                         'next_cursor': nullable({'type': 'string', 'minLength': 1, 'maxLength': 4096})})

    resources = schema('resources', {'$defs': {
        'vehicles': open_obj({'vehicles': {'type': 'array', 'maxItems': 10000, 'items': open_obj({'vehicle_id': UUID, 'display_name': nullable(STR)})}}),
        'current': current,
        'address_provenance': address_provenance,
        'elevation_provenance': elevation_provenance,
        'drive': drive, 'drives': page('drive', 500),
        'position': position, 'positions': page('position', 2048),
        'charge': charge, 'charges': page('charge', 500),
        'charge_sample': charge_sample, 'charge_samples': page('charge_sample', 2048),
        'health': open_obj({'status': {'type': 'string', 'const': 'ok'}, 'version': STR}),
        'ready': {'oneOf': [
            open_obj({'status': {'type': 'string', 'const': 'ready'}}),
            open_obj({'status': {'type': 'string', 'const': 'not_ready'}, 'reason': {'enum': ['catalogue_unavailable', 'lifecycle_quarantined', 'published_content_unservable', 'collector_absent', 'collector_stale', 'collector_auth_terminal']}})]}}})

    token = {'type': 'string', 'pattern': '^[a-z0-9.]{1,64}$'}
    discovery = schema('discovery', open_obj({
        'hub_id': UUID,
        'protocol': {'type': 'string', 'const': 'teslatlas-sync'},
        'protocol_major': {'type': 'integer', 'const': 1},
        'api_versions': {'type': 'array', 'const': ['1.0'], 'minItems': 1, 'maxItems': 1, 'items': {'type': 'string'}},
        'capabilities': {'type': 'array', 'maxItems': 16, 'uniqueItems': True, 'items': token,
                         'allOf': [{'contains': {'const': 'query.vehicles'}}, {'contains': {'const': 'query.current'}}]},
        'version': STR, 'sourceUrl': URI,
        'pack_format': {'type': 'string', 'const': 'sqlite-zstd'},
        'profile': {'type': 'string', 'pattern': r'^hub-http-v1@1\.[0-9]+\.[0-9]+$', 'maxLength': 64},
        'manifestPublicKey': HEX64,
        'attributions': {'type': 'array', 'maxItems': 8, 'items': open_obj({
            'id': {'type': 'string', 'maxLength': 64}, 'text': {'type': 'string', 'maxLength': 1024},
            'licence': {'type': 'string', 'maxLength': 128}, 'url': URI})}},
        ['hub_id', 'protocol', 'protocol_major', 'api_versions', 'capabilities', 'version', 'sourceUrl', 'pack_format', 'profile']))

    auth = schema('auth', {'$defs': {
        'invitation': open_obj({'pairingId': UUID, 'secret': HEX64, 'expiresAtMs': INT, 'endpoint': {'type': 'string', 'format': 'uri', 'maxLength': 4096},
                                'tlsPin': HEX64, 'pairingUri': {'type': 'string', 'pattern': '^teslatlas-hub://pair\\?', 'maxLength': 16384}}),
        'claim_request': closed_obj({'secret': STR, 'device_name': DEVICE_NAME}),
        'claim': open_obj({'device_id': UUID, 'access_token': HEX64, 'expires_at_ms': INT, 'reason': {'enum': ['max_lifetime']}},
                          ['device_id', 'access_token', 'expires_at_ms'])}})

    rotation_grace = schema('rotation-grace', {'$defs': {'fixture': closed_obj({
        'fixture_id': {'const': 'paired-bearer-rotation-grace-24h'}, 'rotation_at_ms': INT,
        'grace_duration_ms': {'const': 86400000}, 'old_bearer_valid_from_ms': INT, 'old_bearer_valid_until_ms': INT,
        'lost_response_retry': closed_obj({'request': {'const': 'POST /v1/device/rotate'}, 'status': {'const': 200}, 'returns': {'const': 'same_successor'}}),
        'after_grace': closed_obj({'status': {'const': 401}})})}})

    sync = schema('sync', {'$defs': {'schema_2_2_noop_unavailable_fixture': closed_obj({
        'fixture_id': {'const': 'schema-2-2-noop-unavailable'}, 'route': {'const': 'GET /v1/vehicles/{vehicle_id}/sync/noop'},
        'requested_schema': {'const': '2.2'},
        'response': closed_obj({'status': {'const': 406}, 'headers': closed_obj({'cache_control': {'const': 'no-store'}}),
                                'body_bytes': {'const': 0}, 'manifest_signature': {'const': 'absent'}})})}})

    errors = schema('errors', open_obj({'error': open_obj({'code': {'type': 'string', 'pattern': '^[a-z_]{1,64}$'}, 'message': STR})}))

    cursor = schema('cursor', {
        'description': 'Page cursors are opaque to clients. These grammars serve Hubs and conformance only: tqp1 is issued for every page route; tqd1 is the legacy drives cursor, accepted for one release and never issued.',
        'oneOf': [ref('#/$defs/tqp1'), ref('#/$defs/tqd1')],
        '$defs': {
            'tqp1': {'type': 'string', 'minLength': 186, 'maxLength': 186, 'pattern': '^tqp1\\.[0-9a-f]{116}\\.[0-9a-f]{64}$',
                     'description': '"tqp1." + 116 lowercase hex (58-byte payload) + "." + 64 lowercase hex (32-byte HMAC-SHA256 tag).'},
            'tqd1': {'type': 'string', 'minLength': 168, 'maxLength': 168, 'pattern': '^tqd1\\.[0-9a-f]{98}\\.[0-9a-f]{64}$',
                     'description': '"tqd1." + 98 lowercase hex (49-byte payload) + "." + 64 lowercase hex (32-byte tag). Legacy drives cursor.'}}})

    return {'resources.schema.json': resources, 'discovery.schema.json': discovery, 'auth.schema.json': auth,
            'rotation-grace.schema.json': rotation_grace, 'sync.schema.json': sync, 'errors.schema.json': errors,
            'cursor.schema.json': cursor}, current, drive, position, charge, charge_sample


# ---------------------------------------------------------------------------
# Routes
# ---------------------------------------------------------------------------
def page_route(kind, path, capability, limit, order, window_key, item, not_found, in_progress, parent=None):
    return {
        'kind': kind, 'method': 'GET', 'path': path, 'bearer': True, 'capability': capability, 'etag_required': True,
        'statuses': [200, 304, 400, 401, 404, 503],
        'responses': {'200': 'resource', '304': 'empty', '400': 'error', '401': 'empty', '404': 'error', '503': 'empty_or_error'},
        'error_codes': {'400': QUERY_ERRORS, '404': not_found, '503': ['service_unavailable']},
        'error_503_forms': [AUTH_STORE_FORM, QUERY_FORM, TIMEOUT_FORM],
        'limit': limit, 'order': order, 'window_key': window_key, 'item': item, 'in_progress': in_progress,
        **({'parent': parent} if parent else {}),
    }


def item_route(kind, path, capability, item, not_found, in_progress):
    return {
        'kind': kind, 'method': 'GET', 'path': path, 'bearer': True, 'capability': capability, 'etag_required': False,
        'statuses': [200, 401, 404, 503],
        'responses': {'200': 'resource', '401': 'empty', '404': 'error', '503': 'empty_or_error'},
        'error_codes': {'404': not_found, '503': ['service_unavailable']},
        'error_503_forms': [AUTH_STORE_FORM, QUERY_FORM, TIMEOUT_FORM],
        'item': item, 'in_progress': in_progress,
    }


def plain_route(kind, method, path, bearer, responses, **extra):
    return {'kind': kind, 'method': method, 'path': path, 'bearer': bearer, 'etag_required': False,
            'statuses': sorted(int(status) for status in responses), 'responses': responses, **extra}


DRIVES = '/v1/vehicles/{vehicle_id}/drives'
CHARGES = '/v1/vehicles/{vehicle_id}/charges'
# Only closed sessions are served: the drive and charge schemas require a non-null end_date_ms.
IN_PROGRESS = {
    'drives': 'Only drives that have ended are listed; a drive in progress is left out.',
    'drive': 'A drive in progress is not served: its id is 404 drive_not_found.',
    'drive_positions': 'The positions of a drive in progress are not served: its id is 404 drive_not_found.',
    'charges': 'Only closed charge sessions (end_date_ms set) are listed; a charge in progress is left out.',
    'charge': 'A charge in progress is not served: its id is 404 charge_not_found.',
    'charge_samples': 'The samples of a charge in progress are not served: its id is 404 charge_not_found.',
}
ROUTES = [
    plain_route('discovery', 'GET', '/.well-known/teslatlas-hub', False, {'200': 'resource', '503': 'empty_or_error'},
                error_codes={'503': ['service_unavailable']}, error_503_forms=[TIMEOUT_FORM, 'JSON service_unavailable']),
    plain_route('health', 'GET', '/healthz', False, {'200': 'resource', '503': 'empty'}),
    plain_route('ready', 'GET', '/readyz', False, {'200': 'resource', '503': 'empty_or_ready'}),
    plain_route('vehicles', 'GET', '/v1/vehicles', True, {'200': 'resource', '401': 'empty', '503': 'empty'}, capability='query.vehicles'),
    plain_route('current', 'GET', '/v1/vehicles/{vehicle_id}/current', True,
                {'200': 'resource', '401': 'empty', '404': 'empty', '503': 'empty'}, capability='query.current'),
    page_route('drives', DRIVES, 'query.drives', {'default': 100, 'min': 1, 'max': 500},
               {'keys': ['start_date_ms', 'id'], 'direction': 'descending'}, 'start_date_ms', 'drive', ['vehicle_not_found'],
               IN_PROGRESS['drives']),
    plain_route('claim', 'POST', '/v1/pairings/{pairing_id}/claim', False,
                {'200': 'resource', '400': 'text', '401': 'empty', '404': 'empty', '413': 'text', '415': 'text', '422': 'text', '503': 'empty'},
                request_limit='A request body of a JSON media type (application/json or application/*+json) over 4096 bytes is answered 413 with a bounded text/plain body; the request is refused before the claim is attempted. A request whose Content-Type is missing or is not a JSON media type (application/json or application/*+json) is 415 whatever its size.',
                request_fields={'device_name': 'See pairing.device_name: a name the Hub refuses is 422 and consumes nothing.'},
                retry='Never retry a claim automatically after an empty 503, a timeout or a lost response: the invitation is single use and the claim may already have consumed it. A 400, 413, 415 or 422 answer means the claim was not attempted, so the same invitation can be used for a corrected request.'),
    plain_route('rotate', 'POST', '/v1/device/rotate', True, {'200': 'resource', '401': 'empty', '404': 'empty', '503': 'empty'}),
    item_route('drive', DRIVES + '/{drive_id}', 'query.drives', 'drive', ['vehicle_not_found', 'drive_not_found'], IN_PROGRESS['drive']),
    page_route('drive_positions', DRIVES + '/{drive_id}/positions', 'query.positions', {'default': 1000, 'min': 1, 'max': 2048},
               {'keys': ['date_ms', 'id'], 'direction': 'ascending'}, 'date_ms', 'position', ['vehicle_not_found', 'drive_not_found'],
               IN_PROGRESS['drive_positions'], 'drive'),
    page_route('charges', CHARGES, 'query.charges', {'default': 100, 'min': 1, 'max': 500},
               {'keys': ['start_date_ms', 'id'], 'direction': 'descending'}, 'start_date_ms', 'charge', ['vehicle_not_found'],
               IN_PROGRESS['charges']),
    item_route('charge', CHARGES + '/{charge_id}', 'query.charges', 'charge', ['vehicle_not_found', 'charge_not_found'], IN_PROGRESS['charge']),
    page_route('charge_samples', CHARGES + '/{charge_id}/samples', 'query.charge.samples', {'default': 1000, 'min': 1, 'max': 2048},
               {'keys': ['timestamp_ms', 'id'], 'direction': 'ascending'}, 'timestamp_ms', 'charge_sample', ['vehicle_not_found', 'charge_not_found'],
               IN_PROGRESS['charge_samples'], 'charge'),
    plain_route('revoke', 'POST', '/v1/device/revoke', True, {'204': 'empty', '400': 'any', '401': 'empty', '404': 'empty', '503': 'empty'},
                capability='auth.revoke',
                request_body='none; any non-empty request body, including one over 4096 bytes, is 400, revokes nothing, and the 400 body is not constrained',
                request_body_limit='The 4096 byte ordinary request-body limit never turns an over-limit body into 413 on this route: a Hub answers 400 for every non-empty body, so it does not read the body through an extractor that answers 413.',
                availability='Clients call it only when discovery advertises auth.revoke. When paired-device authentication is off, auth.revoke is not advertised and the route answers 404 with an empty body, as rotate does.'),
]
COMPANION_ROUTES = [  # defined by hub-sync-v1; named here only so access and revocation cases can address them
    {'kind': 'sync_manifest', 'method': 'GET', 'path': '/v1/vehicles/{vehicle_id}/sync/manifest', 'bearer': True,
     'defined_by': 'hub-sync-v1', 'statuses': [401, 404],
     'responses': {'401': 'empty', '404': 'empty_or_error'}, 'error_codes': {'404': ['vehicle_not_found']},
     'availability': 'Served by every Hub of this profile whether or not sync.packs is advertised: its 401 and 404 answers do not depend on that token.',
     'note': 'Only the statuses this profile asserts are listed. hub-sync-v1@1.3.0 also defines 200 and 406 and lists no 404 for this route: the 404 (JSON vehicle_not_found for a well-formed unknown or ungranted vehicle id, an empty body for a malformed one) is asserted by this profile, not inherited from hub-sync-v1. The Hub also answers an empty 400 (an invalid x-teslatlas-sync-capability or x-teslatlas-supported-schemas header) and empty 503s on it, which hub-sync-v1@1.3.0 does not list and this profile does not assert.'},
    {'kind': 'pack', 'method': 'GET', 'path': '/v1/packs/sha256/{object_name}', 'bearer': True,
     'capability': 'sync.packs', 'defined_by': 'hub-sync-v1', 'statuses': [401, 404], 'responses': {'401': 'empty', '404': 'empty'},
     'object_name': 'The lowercase hex SHA-256 pack digest followed by .sqlite.zst; a name of any other form is an empty 404.',
     'note': 'Only the statuses this profile asserts are listed. hub-sync-v1@1.3.0 lists 200, 206, 401 and an empty 404 for this route; the Hub also answers 406 (schema publication unavailable), 416 (an unsatisfiable Range) and empty 503s (no cursor key, no free stream slot, catalogue errors) on it, which hub-sync-v1@1.3.0 does not list and this profile does not assert.'},
]
# What hub-sync-v1@1.3.0 (its status-tables.json) lists for a vehicle that is not served and what the Hub answers (it answers an unknown vehicle exactly as an ungranted one).
SYNC_VEHICLE_404 = [
    {'route': 'GET /v1/vehicles/{vehicle_id}/sync/manifest', 'hub_sync_v1_lists': 'no 404 (200, 401, 406)',
     'malformed_vehicle_id': 'empty 404', 'unknown_vehicle': '404 JSON vehicle_not_found', 'asserted_by_this_profile': True},
    {'route': 'GET /v1/vehicles/{vehicle_id}/sync/noop', 'hub_sync_v1_lists': 'no 404 (200, 401, 406)',
     'malformed_vehicle_id': 'empty 404', 'unknown_vehicle': '404 JSON vehicle_not_found', 'asserted_by_this_profile': True},
    {'route': 'POST /v1/vehicles/{vehicle_id}/sync/changes-since', 'hub_sync_v1_lists': '404 JSON vehicle_not_found',
     'malformed_vehicle_id': '404 JSON vehicle_not_found', 'unknown_vehicle': '404 JSON vehicle_not_found', 'asserted_by_this_profile': False},
    {'route': 'GET /v1/vehicles/{vehicle_id}/sync/signing-keys', 'hub_sync_v1_lists': 'empty 404',
     'malformed_vehicle_id': 'empty 404', 'unknown_vehicle': 'empty 404', 'asserted_by_this_profile': False},
    {'route': 'GET /v1/vehicles/{vehicle_id}/sync/prepared-artefacts/{artifact_id}', 'hub_sync_v1_lists': 'empty 404',
     'malformed_vehicle_id': 'empty 404', 'unknown_vehicle': '404 JSON vehicle_not_found', 'asserted_by_this_profile': False,
     'note': 'For a well-formed unknown vehicle the Hub answers the JSON envelope where hub-sync-v1@1.3.0 lists an empty 404; this profile changes neither, and an ungranted vehicle is answered as an unknown one.'},
]
EXAMPLES = {
    'discovery': 'discovery.schema.json', 'discovery-unsigned': 'discovery.schema.json',
    'vehicles': 'resources.schema.json#/$defs/vehicles', 'current': 'resources.schema.json#/$defs/current',
    'drives': 'resources.schema.json#/$defs/drives', 'drive': 'resources.schema.json#/$defs/drive',
    'positions': 'resources.schema.json#/$defs/positions', 'charges': 'resources.schema.json#/$defs/charges',
    'charge': 'resources.schema.json#/$defs/charge', 'charge-samples': 'resources.schema.json#/$defs/charge_samples',
    'claim': 'auth.schema.json#/$defs/claim', 'rotate-max-lifetime': 'auth.schema.json#/$defs/claim',
    'invitation': 'auth.schema.json#/$defs/invitation',
    'error-drive-not-found': 'errors.schema.json', 'error-charge-not-found': 'errors.schema.json',
    'health': 'resources.schema.json#/$defs/health', 'ready': 'resources.schema.json#/$defs/ready',
    'rotation-grace': 'rotation-grace.schema.json#/$defs/fixture',
    'sync-schema-2-2-noop-unavailable': 'sync.schema.json#/$defs/schema_2_2_noop_unavailable_fixture',
}


def build_profile():
    cursor_layout = [
        {'field': 'version', 'bytes': 1, 'value': 1},
        {'field': 'kind', 'bytes': 1, 'values': CURSOR_KIND},
        {'field': 'vehicle_id', 'bytes': 16, 'encoding': 'UUID bytes'},
        {'field': 'scope_id', 'bytes': 8, 'encoding': 'signed big-endian; the drive or charge id for nested pages, otherwise 0'},
        {'field': 'from_ms', 'bytes': 8, 'encoding': 'signed big-endian; exact request value'},
        {'field': 'to_ms', 'bytes': 8, 'encoding': 'signed big-endian; exact request value'},
        {'field': 'last_key_a', 'bytes': 8, 'encoding': 'signed big-endian; first ordering key of the last emitted item'},
        {'field': 'last_key_b', 'bytes': 8, 'encoding': 'signed big-endian; id of the last emitted item'},
    ]
    return {
        'profile_id': ID, 'status': 'served', 'contract_version': VERSION, 'previous_profile': 'hub-http-v1@1.1.0',
        'product_version_binding': 'independent; see ecosystem compatibility manifests',
        'license': 'Apache-2.0', 'schema_dialect': DRAFT,
        'max_response_bytes': MAX_PAGE_BYTES, 'max_claim_request_bytes': 4096,
        'additive_policy': 'Clients ignore unknown object fields and unknown capability tokens. Servers may add optional fields and tokens within 1.x. Removing or retyping a field, changing an enum value, or changing a route\'s status set is a new major profile.',
        'capabilities': CAPABILITIES,
        'capability_grammar': {'pattern': '^[a-z0-9.]{1,64}$', 'max_items': 16, 'unique': True},
        'capability_advertisement': {
            'rule': 'A token is advertised only when its routes are served.',
            'always': ['query.vehicles', 'query.current'],
            'with_cursor_key': CURSOR_KEY_CAPABILITIES,
            'with_paired_device_authentication': ['auth.revoke'],
            'with_enrichment': ['query.enrichment'],
            'attributions': 'Discovery carries attributions when query.enrichment is advertised.',
            'home_assistant': 'Discovery keeps api_versions ["1.0"], protocol "teslatlas-sync", protocol_major 1 and the query.vehicles and query.current tokens.'},
        'integers': {'wire': 'signed 64-bit JSON integers',
                     'javascript': 'decode losslessly or reject values outside [-9007199254740991,9007199254740991] before normalization; never silently round IDs'},
        'cursor': {
            'opaque': True, 'format': 'tqp1', 'grammar': 'tqp1.<116 lowercase hex>.<64 lowercase hex>', 'schema': 'cursor.schema.json',
            'payload': {'bytes': 58, 'layout': cursor_layout},
            'tag': {'bytes': 32, 'algorithm': 'HMAC-SHA256', 'key': 'the Hub installation cursor key',
                    'message': 'domain prefix followed by the payload',
                    'domain_prefix': 'teslatlas-hub/public-query-cursor/v1 followed by one NUL byte',
                    'example_key_hex': EXAMPLE_CURSOR_KEY.hex(),
                    'example_key_use': 'Example cursors only; no Hub uses this key.'},
            'binding': ['Hub installation cursor key', 'cursor kind', 'vehicle UUID', 'scope id', 'exact from_ms', 'exact to_ms'],
            'rejected_when': 'The tag, kind, vehicle, scope id or window differs, or a last key falls outside the window.',
            'legacy': {'format': 'tqd1', 'accepted_on': ['drives'], 'issued': False, 'accepted_for': 'one release',
                       'payload_bytes': 49, 'tag_bytes': 32,
                       'payload_layout': 'version 1 (1 byte), vehicle UUID (16), from_ms (8), to_ms (8), last start_date_ms (8), last drive id (8)'},
            'handling': 'Preserve byte-for-byte, percent-encode once as query value, never parse or log; limit may change. Reuse against another Hub, vehicle, scope or window fails with 400 invalid_cursor.'},
        'pages': {
            'routes': ['drives', 'drive_positions', 'charges', 'charge_samples'],
            'query': {'from_ms': 'inclusive; integer >= 0; default 0', 'to_ms': 'exclusive; integer >= 0; default 9223372036854775807 (omit the parameter for the default; JavaScript clients never send that value)',
                      'limit': 'integer between 1 and the route maximum; default per route', 'cursor': 'opaque next_cursor value, unchanged'},
            'time_window': 'from_ms inclusive, to_ms exclusive; default 0 through signed64 maximum; from_ms must be less than to_ms',
            'unknown_query_parameters': {'status': 400, 'error_code': 'invalid_query'},
            'malformed_query_values': {
                'status': 400, 'error_code': 'invalid_query',
                'rule': 'A query value that does not parse as a decimal integer in the parameter\'s range (limit 0 to 4294967295, from_ms and to_ms signed 64-bit) is 400 invalid_query, for example limit=abc, limit=-1, from_ms=1.5 or to_ms=9223372036854775808. invalid_limit and invalid_time_range apply only to well-formed values: limit=0 or above the route maximum is invalid_limit, and from_ms=-1 or from_ms not below to_ms is invalid_time_range.'},
            'errors': QUERY_ERRORS,
            'response': {'items': 'array in route order', 'next_cursor': 'string, or null on the last page'},
            'max_bytes': MAX_PAGE_BYTES,
            'item_split': 'A page is never split inside an item: it stops before the item that would exceed the byte cap and carries a cursor after the last included item. An item that alone exceeds the cap is 503 service_unavailable.',
            'etag': 'strong; quoted lowercase hex SHA-256 of the response body',
            'conditional': 'If-None-Match matching the current ETag returns 304 with an empty body, the same ETag and Cache-Control: no-store.',
            'cache_control': 'no-store', 'content_type': 'application/json'},
        'routes': ROUTES,
        'companion_routes': COMPANION_ROUTES,
        'unauthorized': {'status': 401, 'body': 'empty', 'header': 'WWW-Authenticate: Bearer',
                         'applies_to': 'every route with bearer true, including revoke',
                         'path_segments': 'The bearer is checked before the path segments are interpreted: a request without a valid bearer is 401 whatever its vehicle_id, drive_id, charge_id or object_name, including a segment that percent-decodes to invalid UTF-8.'},
        'sessions': {'served': 'Closed sessions only: a drive or charge that has an end_date_ms. The drive and charge schemas require it to be non-null.',
                     'in_progress': 'A drive or charge in progress is never listed, and its id is 404 drive_not_found or charge_not_found on the item and nested routes.'},
        'path_identifiers': {'vehicle_id': 'UUID', 'drive_id': 'signed 64-bit integer >= 1', 'charge_id': 'signed 64-bit integer >= 1',
                             'invalid': '404 with the route\'s not-found JSON (drive_not_found or charge_not_found), never 400, so path probing returns one shape',
                             'undecodable': {
                                 'rule': 'A path segment (vehicle_id, drive_id, charge_id or the pack object_name) whose percent-decoding is not valid UTF-8, for example %FF, is a malformed identifier like any other. It is answered after the bearer check, with the malformed-identifier answer of the route below, never 400 and never a framework text/plain body; a request without a valid bearer is 401 whatever its path segments.',
                                 'after_bearer_check': True,
                                 'answers': {'current': 'empty 404', 'json_routes': '404 JSON vehicle_not_found for a malformed vehicle_id, drive_not_found or charge_not_found for a malformed drive_id or charge_id',
                                             'sync_manifest': 'empty 404', 'sync_noop': 'empty 404', 'pack': 'empty 404',
                                             'other_sync_routes': 'changes-since, signing-keys and prepared-artefacts answer as access.sync_routes.routes[].malformed_vehicle_id says'}},
                             'nil_vehicle_id': {
                                 'value': '00000000-0000-0000-0000-000000000000',
                                 'rule': 'The nil UUID is a well-formed vehicle_id that no vehicle can have: it is an unknown vehicle and is answered as any other unknown vehicle, and its value never causes a 503 (a 503 for another reason, such as the handler time budget, can happen to it as to any request).',
                                 'answers': {'current': 'empty 404', 'json_routes': '404 JSON vehicle_not_found', 'sync_manifest': '404 JSON vehicle_not_found', 'sync_noop': '404 JSON vehicle_not_found'}},
                             'spellings': 'Only the lowercase hyphenated form of a vehicle_id and the plain decimal form of a drive_id or charge_id are specified. A Hub\'s parser may also accept other spellings of the same value (a UUID in upper case, without hyphens, in braces or as urn:uuid:; an integer with a plus sign or leading zeros) and serve the same resource: a client sends only the specified forms and does not rely on the others being refused.'},
        'access': {
            'text': 'Every vehicle-scoped route checks the paired device\'s vehicle grant. An ungranted vehicle is answered exactly as an unknown vehicle on the same route: 404 vehicle_not_found on JSON routes and an empty-body 404 on /current. Each sync route keeps the 404 the Hub gives an unknown vehicle today, and an ungranted vehicle gets that same 404. hub-sync-v1@1.3.0 lists the JSON vehicle_not_found 404 on changes-since and an empty 404 on signing-keys and prepared-artefacts. It lists no 404 on manifest and no-op, where the Hub answers JSON vehicle_not_found for a well-formed unknown vehicle id and an empty 404 for a malformed one; this profile records those two as asserted, not inherited, statuses. GET /v1/vehicles lists only granted vehicles. Packs resolve their owning vehicle before streaming; an ungranted digest is an empty 404 indistinguishable from an unknown digest.',
            'vehicle_scoped_routes_check_grant': True, 'check_before_query_parsing': True, 'unknown_equals_ungranted': True,
            'json_routes_not_found': {'status': 404, 'error_code': 'vehicle_not_found'},
            'empty_404_routes': ['current', 'pack'],
            'sync_routes': {
                'rule': 'Each sync route keeps the 404 the Hub gives an unknown vehicle today, and an ungranted vehicle is answered exactly as an unknown vehicle. The manifest and no-op 404s are asserted by this profile, not inherited: hub-sync-v1@1.3.0 lists no 404 for them. The other routes keep what they have.',
                'listed_by': 'hub-sync-v1@1.3.0, its status-tables.json (hub-sync-v1@1.4.0 publishes no status table, and its OpenAPI lists no 404 on changes-since or on the manifest)',
                'routes': SYNC_VEHICLE_404},
            'vehicles_list': 'granted vehicles only',
            'pack': 'The owning vehicle is resolved before streaming; an ungranted digest is an empty 404 indistinguishable from an unknown digest.'},
        'pairing': {
            'invitation_command': 'teslatlas-hub --config PRIVATE_CONFIG pair --json', 'claim_fields': ['secret', 'device_name'], 'path_field': 'pairingId',
            'device_name': {
                'rule': 'device_name is 1 to 128 UTF-8 bytes and has no control character (U+0000 to U+001F, U+007F to U+009F). The schema states the character limit (maxLength 128) and the control-character rule, but a character count cannot state the byte limit: a name of at most 128 characters can still be over 128 bytes, and is refused all the same.',
                'refused': {'status': 422, 'body': 'bounded text/plain',
                            'rule': 'A name that breaks the rule is answered 422 with a bounded text/plain body before the invitation is consulted: the refusal consumes nothing and does not reveal whether the invitation exists, so the same invitation still claims with a valid name. It is never an empty 503, which a client cannot correct.'},
                'max_bytes': 128, 'consumes_invitation': False, 'schema': 'auth.schema.json#/$defs/claim_request'},
            'invitation_lifetime': 'expiresAtMs epoch milliseconds; default invitation lifetime 900 seconds, owner-configurable; single use.',
            'lifetime': {
                'text': 'Bearer expires_at_ms slides to at most now + 30 days on use and never beyond the device\'s absolute maximum (owner-configurable, default 180 days from pairing). When rotation cannot move the expiry later, rotate returns 200 with the current bearer and expiry unchanged and reason "max_lifetime", and issues nothing; clients should ask the owner to re-pair before expires_at_ms.',
                'sliding_ms': 2592000000, 'absolute_maximum_default_days': 180,
                'at_maximum': {'status': 200, 'reason': 'max_lifetime', 'access_token': 'the current bearer, unchanged', 'expires_at_ms': 'unchanged', 'issues': 'nothing'}},
            'rotation': {
                'old_bearer_grace_ms': 86400000,
                'old_bearer_validity': 'The prior bearer stays valid for normal requests through rotation_at_ms + 86400000 exclusive. A retry of rotate with the prior bearer during the grace window returns the same successor; it never issues a different token. After the window the prior bearer is 401.',
                'lost_response_retry': 'A retry of rotate with the prior bearer during the grace window returns the same successor; it never issues a different token.',
                'after_grace': {'status': 401},
                'one_grace_per_device': 'Only the bearer replaced by the latest rotation has a grace window; a further rotation ends the earlier bearer\'s grace, and it is then 401.',
                'response_fields': 'device_id, access_token, expires_at_ms and, only at the absolute maximum, reason'},
            'revocation': 'POST /v1/device/revoke with the current or grace bearer and no request body revokes the calling device and returns 204 with an empty body. Afterwards the bearer is 401 on every route, so a repeated call returns 401 and clients treat 401 on revoke as already revoked. A request with a non-empty body, including one over 4096 bytes, is 400 and revokes nothing; it is never 413. When paired-device authentication is off, auth.revoke is not advertised and the route answers 404 with an empty body.',
            'trust': 'endpoint plus tlsPin (SHA256 of DER leaf certificate); validate TLS identity before sending secret. Fixture uses a private CA trust file and verifies hostname. pairingUri duplicates secret; keep whole invitation private.'},
        'error_codes': ERROR_CODES,
        'unsupported': {'commands': 'no public vehicle command route', 'sse': 'no public event stream or epoch', 'metadata': 'no public metadata CRUD',
                        'revisions': 'no public current or drive snapshot revision', 'protocol_version_header': 'not emitted or required',
                        'rich_protocol_versions': 'the root teslatlas-v1 family is not served'},
        'limits': {'ordinary_request_body_bytes': 4096, 'changes_since_request_body_bytes': 8192, 'changes_since_defined_by': 'hub-sync-v1',
                   'in_flight_requests': 32, 'handler_timeout_seconds': 15, 'json_page_bytes': MAX_PAGE_BYTES,
                   'handler_timeout_response': {
                       'status': 503, 'body': 'empty', 'applies_to': 'every route, including the hub-sync-v1 routes',
                       'budget': 'The 15 second handler budget includes the time spent waiting for one of the 32 in-flight request slots.',
                       'rule': 'When the budget expires the Hub answers 503 with an empty body and no content type, whatever the route. A route that lists a JSON 503 form (service_unavailable, not_ready) still answers an empty body in this case, so a client treats an empty 503 on any route except the claim as a transient outage and may retry. A claim is never retried automatically: the invitation is single use, and a claim that ended in an empty 503 may already have consumed it.'},
                   'browser_origin_response': {
                       'status': 403, 'body': 'empty', 'cache_control': 'no-store', 'applies_to': 'every route, including the hub-sync-v1 routes',
                       'rule': 'A request that carries an Origin header is answered 403 with an empty body and Cache-Control: no-store, before the route runs, when the Hub does not admit that origin (neither the Hub\'s own origin nor one of the owner\'s allowed origins) or the route has no browser access. Which routes admit browser origins, and how the Hub answers a CORS preflight (an OPTIONS request), are set by the Hub and lie outside this profile. A client that sends no Origin header, such as a native client or a conformance run, never sees any of them.'},
                   'array_max_items': {'vehicles': 10000, 'capabilities': 16, 'attributions': 8, 'drives': 500, 'charges': 500, 'positions': 2048, 'charge_samples': 2048}},
        'examples': EXAMPLES,
        'sync_regression_appendix': 'sync-regression.json'}


# ---------------------------------------------------------------------------
# OpenAPI
# ---------------------------------------------------------------------------
STATUS_TEXT = {200: 'Successful response', 204: 'No content; empty body', 304: 'Not modified; empty body', 400: 'Invalid request',
               401: 'Missing, invalid, expired or revoked bearer; empty body', 404: 'Not found', 413: 'Request body over the 4096 byte limit',
               415: 'Unsupported media type: the Content-Type is missing or is not a JSON media type (application/json or application/*+json), whatever the size of the body',
               422: 'Unprocessable request', 503: 'Service unavailable; empty body (also the answer when the handler time budget expires)'}
TIMEOUT_RULE = ('Any route may answer 503 with an empty body when the 15 second handler time budget, which includes the wait for one of the '
                '32 in-flight request slots, expires. Treat an empty 503 as a transient outage, except on the claim route, which is never retried automatically: '
                'the invitation is single use and the claim may already have consumed it.')
ORIGIN_RULE = ('Any route answers 403 with an empty body and Cache-Control: no-store to a request whose Origin header the Hub does not admit '
               '(neither its own origin nor one of the owner\'s allowed origins) or that names a route without browser access. Which routes admit browser origins, '
               'and how the Hub answers a CORS preflight (an OPTIONS request), are set by the Hub and lie outside this profile; a client that sends no Origin header never sees any of them.')
CACHE_HEADERS = {'ETag': {'required': True, 'schema': {'type': 'string', 'maxLength': 128}},
                 'Cache-Control': {'required': True, 'schema': {'type': 'string', 'const': 'no-store'}}}
ID_PARAMETER = {'type': 'integer', 'format': 'int64', 'minimum': 1, 'maximum': I64_MAX}


def body_reference(kind):
    if kind in ('claim', 'rotate'):
        return 'auth.schema.json#/$defs/claim'
    if kind == 'discovery':
        return 'discovery.schema.json'
    if kind == 'drive_positions':
        return 'resources.schema.json#/$defs/positions'
    return 'resources.schema.json#/$defs/' + kind


def build_openapi():
    paths = {}
    for route in ROUTES:
        kind, path = route['kind'], route['path']
        parameters = [{'name': name, 'in': 'path', 'required': True, 'schema': UUID} for name in ('vehicle_id', 'pairing_id') if '{' + name + '}' in path]
        for parameter in parameters:
            if parameter['name'] == 'vehicle_id':
                parameter['description'] = ('A vehicle UUID, in its lowercase hyphenated form (the only form this profile specifies: a Hub\'s UUID parser may also accept upper case, no hyphens, braces or '
                                            'urn:uuid: and resolve them to the same vehicle, which a client must not rely on). Any value that does not parse as a UUID, including one that '
                                            'percent-decodes to invalid UTF-8, and the nil UUID, is answered as an unknown vehicle (404) after the bearer check, never 400.')
        for name in ('drive_id', 'charge_id'):
            if '{' + name + '}' in path:
                parameters.append({'name': name, 'in': 'path', 'required': True, 'schema': ID_PARAMETER,
                                   'description': 'Signed 64-bit integer >= 1, in plain decimal form (the only form this profile specifies: a Hub\'s integer parser may also accept a plus sign or leading zeros, which a client must not rely on). '
                                                  'Any value that does not parse as a decimal integer >= 1, including one that percent-decodes to invalid UTF-8, is 404 with the route\'s not-found JSON, never 400.'})
        if 'limit' in route:
            limit = route['limit']
            parameters += [
                {'name': 'from_ms', 'in': 'query', 'schema': {'type': 'integer', 'format': 'int64', 'minimum': 0, 'maximum': I64_MAX, 'default': 0}, 'description': 'Inclusive lower bound on the window key.'},
                {'name': 'to_ms', 'in': 'query', 'schema': {'type': 'integer', 'format': 'int64', 'minimum': 0, 'maximum': I64_MAX}, 'description': 'Exclusive upper bound on the window key. The default is the signed 64-bit maximum, 9223372036854775807: omit the parameter to get it, and never send that value from JavaScript, which cannot represent it exactly.'},
                {'name': 'limit', 'in': 'query', 'schema': {'type': 'integer', 'minimum': limit['min'], 'maximum': limit['max'], 'default': limit['default']}},
                {'name': 'cursor', 'in': 'query', 'schema': {'type': 'string', 'maxLength': 4096}, 'description': 'Opaque next_cursor from the previous page, unchanged.'},
                {'name': 'If-None-Match', 'in': 'header', 'schema': STR}]
        operation = {'operationId': kind, 'parameters': parameters, 'responses': {},
                     'security': [{'pairedBearer': []}] if route['bearer'] else []}
        if kind == 'claim':
            operation['requestBody'] = {'required': True, 'x-max-request-bytes': 4096,
                                        'content': {'application/json': {'schema': ref('auth.schema.json#/$defs/claim_request')}}}
        if kind == 'revoke':
            operation['description'] = 'Revoke the calling device. The request has no body; any non-empty body, including one over 4096 bytes, is 400 and revokes nothing (never 413).'
        if 'in_progress' in route:
            operation['description'] = route['in_progress']
        for status in route['statuses']:
            shape = route['responses'][str(status)]
            response = {'description': STATUS_TEXT[status]}
            if shape == 'resource':
                response['content'] = {'application/json': {'schema': ref(body_reference(kind))}}
            elif shape == 'error':
                response['content'] = {'application/json': {'schema': ref('errors.schema.json')}}
            elif shape == 'empty_or_ready':
                response['description'] = 'Empty response body (the handler time budget expired), or an application/json readiness document whose status is not_ready.'
                response['content'] = {'application/json': {'schema': ref('resources.schema.json#/$defs/ready')}}
                response['x-empty-body-allowed'] = True
            elif shape == 'empty_or_error':
                response['description'] = ('Empty response body (the handler time budget expired, or an authentication-store outage), or an application/json service_unavailable envelope (a query-originated outage). '
                                           'Nonempty bodies must match the JSON error schema.')
                if kind == 'discovery':
                    response['description'] = 'Empty response body (the handler time budget expired), or an application/json service_unavailable envelope. Nonempty bodies must match the JSON error schema.'
                response['content'] = {'application/json': {'schema': ref('errors.schema.json')}}
                response['x-empty-body-allowed'] = True
            elif shape == 'text':
                response['description'] = 'Request extraction or validation failed; body is bounded text/plain.'
                if status == 415:
                    response['description'] = 'The Content-Type is missing or is not a JSON media type (application/json or application/*+json), whatever the size of the body; body is bounded text/plain.'
                if status == 413:
                    response['description'] = 'The request body, of a JSON media type (application/json or application/*+json), is over the 4096 byte limit and the claim is not attempted; body is bounded text/plain.'
                if kind == 'claim' and status == 422:
                    response['description'] = ('The body is valid JSON but not a claim request, or its device_name is one the Hub refuses (empty, over 128 UTF-8 bytes or with a control character); '
                                               'the claim is not attempted and the invitation is not consumed; body is bounded text/plain.')
                response['content'] = {'text/plain': {'schema': {'type': 'string', 'maxLength': 65536}}}
            elif shape == 'any':
                response['description'] = STATUS_TEXT[status] + '; the body is not constrained.'
            if kind == 'revoke' and status == 404:
                response['description'] = 'Not found; empty body. Paired-device authentication is off and auth.revoke is not advertised.'
            if kind == 'rotate' and status == 404:
                response['description'] = 'Not found; empty body. Paired-device authentication is off.'
            if kind == 'claim' and status == 401:
                response['description'] = 'The invitation secret is invalid, expired or already claimed; empty body.'
            if kind == 'claim' and status == 503:
                response['description'] = 'Service unavailable; empty body (also the answer when the handler time budget expires). Never retry a claim automatically: the invitation is single use and the claim may already have consumed it.'
            if status in (200, 304) and route.get('etag_required'):
                response['headers'] = CACHE_HEADERS
            if status == 401 and route['bearer']:
                response['headers'] = {'WWW-Authenticate': {'schema': {'type': 'string', 'const': 'Bearer'}}}
            operation['responses'][str(status)] = response
        paths.setdefault(path, {})[route['method'].lower()] = operation
    return {'openapi': '3.1.0',
            'info': {'title': 'Teslatlas Hub public HTTP profile hub-http-v1', 'version': VERSION, 'description': TIMEOUT_RULE + ' ' + ORIGIN_RULE,
                     'license': {'name': 'Apache-2.0', 'identifier': 'Apache-2.0'}},
            'paths': paths,
            'components': {'securitySchemes': {'pairedBearer': {'type': 'http', 'scheme': 'bearer', 'description': 'Bearer issued by claim or rotate.'}}}}


# ---------------------------------------------------------------------------
# Examples (synthetic values only)
# ---------------------------------------------------------------------------
VEHICLE = '11111111-1111-4111-8111-111111111111'
DEVICE = '11111111-1111-4111-8111-111111111111'
T0 = 1788565900000
SYNTHETIC_CAR = {  # a non-null car, so the nested objects are checked by every example validation
    'id': 1, 'name': 'Synthetic car', 'model': 'Synthetic model', 'vin': None, 'trim_badging': None, 'marketing_name': None,
    'exterior_color': None, 'wheel_type': None, 'spoiler_type': None, 'firmware_version': None, 'source_eid': None, 'source_vid': None,
    'efficiency_wh_per_km': None,
    'settings': {'enabled': True, 'use_streaming_api': True, 'suspend_min_resolved': False, 'req_not_unlocked': False,
                 'free_supercharging': False, 'lfp_battery': False, 'suspend_after_idle_min': 15, 'suspend_min': 21}}


def build_examples(current, drive, position, charge, charge_sample):
    def blank(definition, **values):
        value = {key: None for key in definition['properties']}
        value.update(values)
        for key in [key for key in definition['properties'] if key not in definition['required'] and value[key] is None and key not in values]:
            del value[key]
        return value

    window = (0, I64_MAX)
    address = {'source': 'geocoder', 'provider': 'synthetic-geocoder', 'endpoint_host': 'geocoder.example.invalid',
               'language': 'en', 'licence': 'ODbL-1.0', 'looked_up_at_ms': T0 - 60000}
    imported = {'source': 'import', 'provider': None, 'endpoint_host': None, 'language': None, 'licence': None, 'looked_up_at_ms': None}
    terrain = {'source': 'terrain', 'dataset': 'synthetic-terrain', 'dataset_version': '1', 'tile': 'synthetic-tile'}

    drive_new = blank(drive, id=102, vehicle_id=VEHICLE, start_date_ms=T0 + 120000, end_date_ms=T0 + 300000, distance_km=12.5, duration_min=3,
                      speed_max=90, start_soc=80, end_soc=76, start_address='Synthetic start', end_address='Synthetic end',
                      start_latitude=0.0, start_longitude=0.0, end_latitude=0.0, end_longitude=0.0,
                      start_address_provenance=imported, end_address_provenance=address)
    drive_old = blank(drive, id=101, vehicle_id=VEHICLE, start_date_ms=T0, end_date_ms=T0 + 60000)
    drives = {'items': [drive_new, drive_old],
              'next_cursor': tqp1_cursor(CURSOR_KIND['drives'], VEHICLE, 0, *window, T0, 101)}

    positions = {'items': [
        blank(position, id=1001, drive_id=101, date_ms=T0, latitude=0.0, longitude=0.0, speed=0, power=0.0, battery_level=80,
              usable_battery_level=80, elevation=0, odometer=1000.0, inside_temp=20.0, outside_temp=10.0, is_climate_on=False,
              elevation_provenance=terrain),
        blank(position, id=1002, drive_id=101, date_ms=T0 + 30000, latitude=0.0, longitude=0.0, speed=30, power=12.0, battery_level=80,
              usable_battery_level=80, odometer=1000.5, elevation_provenance=None)],
        'next_cursor': tqp1_cursor(CURSOR_KIND['positions'], VEHICLE, 101, *window, T0 + 30000, 1002)}

    charge_new = blank(charge, id=202, vehicle_id=VEHICLE, start_date_ms=1788566400000, end_date_ms=1788569000000, duration_min=43,
                       charge_energy_added=21.5, start_battery_level=30, end_battery_level=60, start_latitude=0.0, start_longitude=0.0,
                       address='Synthetic charging address', is_dc=False, cost=4.5, billing_type='per_kwh', cost_per_unit=0.21, session_fee=0.0,
                       address_provenance=address)
    charge_old = blank(charge, id=201, vehicle_id=VEHICLE, start_date_ms=1788560000000, end_date_ms=1788563600000, duration_min=60)
    charges = {'items': [charge_new, charge_old], 'next_cursor': tqp1_cursor(CURSOR_KIND['charges'], VEHICLE, 0, *window, 1788560000000, 201)}

    samples = {'items': [
        blank(charge_sample, id=3001, charge_id=201, timestamp_ms=1788560000000, battery_level=30, usable_battery_level=30,
              charge_energy_added_kwh=0.0, charger_power_kw=7.0, charger_voltage=230.0, charger_actual_current=30.0, charger_pilot_current=32.0,
              charger_phases=1, battery_heater_on=False, fast_charger_present=False, charge_cable='IEC'),
        blank(charge_sample, id=3002, charge_id=201, timestamp_ms=1788560060000, battery_level=31, usable_battery_level=31,
              charge_energy_added_kwh=0.1, charger_power_kw=7.0, charger_voltage=230.0, charger_actual_current=30.0, charger_pilot_current=32.0,
              charger_phases=1, battery_heater_on=False, fast_charger_present=False, charge_cable='IEC')],
        'next_cursor': None}

    discovery = {'hub_id': VEHICLE, 'protocol': 'teslatlas-sync', 'protocol_major': 1, 'api_versions': ['1.0'], 'capabilities': CAPABILITIES,
                 'version': '2026.36.2', 'sourceUrl': 'https://example.invalid/source', 'pack_format': 'sqlite-zstd', 'profile': ID,
                 'manifestPublicKey': '0' * 64,
                 'attributions': [{'id': 'osm', 'text': '(c) OpenStreetMap contributors', 'licence': 'ODbL-1.0', 'url': 'https://www.openstreetmap.org/copyright'}]}
    discovery_unsigned = {key: discovery[key] for key in ('hub_id', 'protocol', 'protocol_major', 'api_versions', 'version', 'sourceUrl', 'pack_format', 'profile')}
    discovery_unsigned['capabilities'] = ['query.vehicles', 'query.current']
    claim = {'device_id': DEVICE, 'access_token': '0' * 64, 'expires_at_ms': 1788567300000}
    return {
        'discovery': discovery, 'discovery-unsigned': discovery_unsigned,
        'vehicles': {'vehicles': [{'vehicle_id': VEHICLE, 'display_name': None}]},
        'current': {**{key: None for key in current['properties']}, 'vehicle_id': VEHICLE, 'car': SYNTHETIC_CAR},
        'drives': drives, 'drive': drive_new, 'positions': positions, 'charges': charges, 'charge': charge_new, 'charge-samples': samples,
        'claim': claim, 'rotate-max-lifetime': {**claim, 'reason': 'max_lifetime'},
        'invitation': {'pairingId': VEHICLE, 'secret': '0' * 64, 'expiresAtMs': 1788567300000, 'endpoint': 'https://example.invalid', 'tlsPin': '0' * 64,
                       'pairingUri': 'teslatlas-hub://pair?' + urllib.parse.urlencode({'endpoint': 'https://example.invalid', 'pairing_id': VEHICLE, 'secret': '0' * 64, 'tls_pin': '0' * 64})},
        'error-drive-not-found': {'error': {'code': 'drive_not_found', 'message': 'drive was not found'}},
        'error-charge-not-found': {'error': {'code': 'charge_not_found', 'message': 'charge was not found'}},
        'health': {'status': 'ok', 'version': '2026.36.2'}, 'ready': {'status': 'ready'},
        'rotation-grace': {'fixture_id': 'paired-bearer-rotation-grace-24h', 'rotation_at_ms': 1788566400000, 'grace_duration_ms': 86400000,
                           'old_bearer_valid_from_ms': 1788566400000, 'old_bearer_valid_until_ms': 1788652800000,
                           'lost_response_retry': {'request': 'POST /v1/device/rotate', 'status': 200, 'returns': 'same_successor'},
                           'after_grace': {'status': 401}},
        'sync-schema-2-2-noop-unavailable': {'fixture_id': 'schema-2-2-noop-unavailable', 'route': 'GET /v1/vehicles/{vehicle_id}/sync/noop', 'requested_schema': '2.2',
                                             'response': {'status': 406, 'headers': {'cache_control': 'no-store'}, 'body_bytes': 0, 'manifest_signature': 'absent'}}}


# ---------------------------------------------------------------------------
# Field semantics
# ---------------------------------------------------------------------------
def build_semantics(current, drive, position, charge, charge_sample):
    units = {'observed_at_ms': 'epoch milliseconds', 'since': 'epoch milliseconds', 'scheduled_charging_start_time': 'epoch seconds',
             'speed': 'km/h, rounded integer from provider mph', 'odometer': 'km, provider miles converted and rounded to 0.01 km',
             'ideal_battery_range_km': 'km, provider miles converted and rounded to 0.01 km', 'est_battery_range_km': 'km, provider miles converted and rounded to 0.01 km',
             'rated_battery_range_km': 'km, provider miles converted and rounded to 0.01 km',
             'active_route_miles_to_arrival': 'miles, provider value unchanged', 'active_route_minutes_to_arrival': 'minutes', 'active_route_traffic_minutes_delay': 'minutes',
             'charge_energy_added': 'kWh', 'charger_power': 'kW', 'power': 'kW', 'time_to_full_charge': 'hours', 'elevation': 'metres',
             'charger_actual_current': 'amperes', 'charger_voltage': 'volts', 'charge_current_request': 'amperes', 'charge_current_request_max': 'amperes',
             'charger_phases': 'count, zero/nonpositive means unknown', 'active_route_energy_at_arrival': 'percent state of charge', 'heading': 'degrees',
             'center_display_state': 'provider integer state code'}
    kinds = {name: kind for kind, names in CURRENT_GROUPS for name in names.split()}

    def current_unit(name):
        if name in units:
            return units[name]
        if 'latitude' in name or 'longitude' in name:
            return 'degrees'
        if name in ('inside_temp', 'outside_temp'):
            return 'degrees Celsius'
        if name.startswith('tpms_pressure_'):
            return 'bar'
        if name in ('battery_level', 'usable_battery_level', 'charge_limit_soc', 'sun_roof_percent_open', 'download_perc', 'install_perc'):
            return 'percent'
        return 'boolean' if kinds[name] == 'boolean' else 'text or provider enum'

    def drive_unit(name):
        if name == 'vehicle_id':
            return 'plain UUID'
        if name == 'id':
            return 'signed64 identifier, preserve losslessly'
        if name.endswith('_date_ms'):
            return 'epoch milliseconds'
        if name.endswith('_km'):
            return 'km'
        if name == 'duration_min':
            return 'minutes, integer'
        if name == 'speed_max':
            return 'km/h, integer'
        if name in ('ascent', 'descent'):
            return 'metres'
        if name in ('start_soc', 'end_soc'):
            return 'percent'
        if 'temp' in name:
            return 'degrees Celsius'
        if 'power' in name:
            return 'kW'
        if 'latitude' in name or 'longitude' in name:
            return 'degrees'
        if name == 'efficiency':
            return 'legacy projection numeric field, no current producer assigns a value; clients must preserve null, no unit claim is made'
        if name.endswith('_provenance'):
            return 'provenance object; see address_provenance'
        return 'text'

    def table(definition, unit_of, non_null, source):
        return {name: {'unit': unit_of(name), 'nullable': name not in non_null, 'source': source} for name in definition['properties']}

    identity = {'id': 'signed64 identifier, preserve losslessly', 'vehicle_id': 'plain UUID', 'drive_id': 'signed64 identifier of the owning drive; equals the drive_id path parameter',
                'charge_id': 'signed64 identifier of the owning charge; equals the charge_id path parameter', 'date_ms': 'epoch milliseconds',
                'timestamp_ms': 'epoch milliseconds', 'start_date_ms': 'epoch milliseconds', 'end_date_ms': 'epoch milliseconds'}
    position_units = {'latitude': 'degrees', 'longitude': 'degrees', 'speed': 'km/h, integer', 'power': 'kW', 'battery_level': 'percent', 'usable_battery_level': 'percent',
                      'elevation': 'metres, integer', 'odometer': 'km', 'ideal_battery_range_km': 'km', 'est_battery_range_km': 'km', 'rated_battery_range_km': 'km',
                      'inside_temp': 'degrees Celsius', 'outside_temp': 'degrees Celsius', 'is_climate_on': 'boolean', 'fan_status': 'provider integer fan level',
                      'driver_temp_setting': 'degrees Celsius', 'passenger_temp_setting': 'degrees Celsius', 'battery_heater': 'boolean', 'battery_heater_on': 'boolean',
                      'tpms_pressure_fl': 'bar', 'tpms_pressure_fr': 'bar', 'tpms_pressure_rl': 'bar', 'tpms_pressure_rr': 'bar',
                      'elevation_provenance': 'provenance object; see elevation_provenance'}
    charge_units = {'duration_min': 'minutes, integer', 'charge_energy_added': 'kWh', 'charge_energy_used_kwh': 'kWh', 'start_battery_level': 'percent', 'end_battery_level': 'percent',
                    'start_ideal_range_km': 'km', 'end_ideal_range_km': 'km', 'start_rated_range_km': 'km', 'end_rated_range_km': 'km', 'start_latitude': 'degrees',
                    'start_longitude': 'degrees', 'address': 'text', 'location_name': 'text', 'geofence': 'text', 'fast_charger_type': 'text',
                    'is_dc': 'boolean', 'charge_rate_km_per_hour': 'km of range added per hour', 'max_charger_power_kw': 'kW', 'outside_temp_avg': 'degrees Celsius',
                    'cost': 'currency units; no currency code is carried; decimal serialised as a JSON number',
                    'billing_type': 'text; known values per_kwh and per_minute; preserve unknown values',
                    'cost_per_unit': 'currency units per kWh when billing_type is per_kwh, per minute when per_minute; decimal serialised as a JSON number',
                    'session_fee': 'currency units; decimal serialised as a JSON number', 'address_provenance': 'provenance object; see address_provenance'}
    sample_units = {'battery_level': 'percent', 'usable_battery_level': 'percent', 'charge_energy_added_kwh': 'kWh', 'charger_power_kw': 'kW', 'charger_voltage': 'volts',
                    'charger_actual_current': 'amperes', 'charger_pilot_current': 'amperes', 'charger_phases': 'count, zero/nonpositive means unknown',
                    'ideal_range_km': 'km', 'rated_range_km': 'km', 'outside_temp_c': 'degrees Celsius', 'battery_heater_on': 'boolean',
                    'fast_charger_present': 'boolean', 'fast_charger_brand': 'text', 'fast_charger_type': 'text', 'charge_cable': 'text'}

    def unit_from(*tables):
        merged = {}
        for source in tables:
            merged.update(source)
        return lambda name: merged[name]

    semantics = {
        'profile_id': ID,
        'number_rule': 'position.power and the charge_sample fields charger_power_kw, charger_voltage, charger_actual_current and charger_pilot_current are whole numbers in TeslaMate but are typed number, because the page routes serve the materialised projections of the Hub, which hold them as floating point: a whole value may carry a fraction digit (12 or 12.0). Clients accept both. Retyping these fields later is a new major profile.',
        'null_rule': 'Null means unavailable, unknown, or not derivable. Preserve null and numeric zero distinctly; never infer a zero timestamp. Current owner/stream merge ignores null overlays. No freshness guarantee beyond observed_at_ms. Optional provenance fields may be absent or null; absent and null mean no truthful record exists.',
        'source_trace': ['Hub current_state build_current_vehicle_summary: provider fields, rounded mph/miles conversions, latest admitted epoch milliseconds',
                         'Hub public_query PublicDrive projection: direct materialised values',
                         'Hub public page projections of the pack models ProjectionPosition, ProjectionCharge and ProjectionChargeSample: direct materialised values; car_id and source ids are dropped',
                         'Hub runtime/lifecycle drive_and_charge: end-start milliseconds divided by 60000; current speed and odometer conversions feed drive aggregates',
                         'Hub import/teslamate projection: stored km, minutes, Celsius and kW passed through; efficiency is absent in live/import projections'],
        'current': {'vehicle_id': {'unit': 'plain UUID', 'nullable': False},
                    'car': {'unit': 'optional materialised identity; source_eid/source_vid/id signed64, efficiency_wh_per_km Wh/km, settings durations minutes', 'nullable': True}},
        'drive': table(drive, drive_unit, {'id', 'vehicle_id', 'start_date_ms', 'end_date_ms'}, 'materialised projection; no public-query conversion'),
        'position': table(position, unit_from(identity, position_units), {'id', 'drive_id', 'date_ms'}, 'materialised position projection; no public-query conversion'),
        'charge': table(charge, unit_from(identity, charge_units), {'id', 'vehicle_id', 'start_date_ms', 'end_date_ms'}, 'materialised charge projection; closed sessions only'),
        'charge_sample': table(charge_sample, unit_from(identity, sample_units), {'id', 'charge_id', 'timestamp_ms'}, 'materialised charge-sample projection'),
        'address_provenance': {
            'source': {'values': {'import': 'value came from the migrated TeslaMate data and is never re-geocoded', 'geocoder': 'value came from a recorded geocoder lookup', 'manual': 'value was set by the owner'}},
            'provider': 'geocoder provider name; null when not recorded', 'endpoint_host': 'host name of the lookup endpoint; null when not recorded',
            'language': 'language requested from the geocoder; null when not recorded', 'licence': 'data licence of the lookup result; null when not recorded',
            'looked_up_at_ms': 'epoch milliseconds of the lookup; null when not recorded'},
        'elevation_provenance': {
            'source': {'values': {'vehicle': 'elevation was reported by the vehicle', 'import': 'elevation came from the migrated TeslaMate data', 'terrain': 'elevation was derived from a terrain dataset'}},
            'dataset': 'terrain dataset name; null unless source is terrain', 'dataset_version': 'terrain dataset version; null unless source is terrain',
            'tile': 'terrain tile identifier; null unless source is terrain'}}
    for name, kind in kinds.items():
        semantics['current'][name] = {'unit': current_unit(name), 'nullable': True, 'source': 'Hub admitted observation/lifecycle/materialised identity'}
    return semantics


# ---------------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------------
INPUTS = {
    'bearer': 'Bearer of a paired device granted vehicle_id and, for the grant cases, ungranted_vehicle_id.',
    'vehicle_id': 'Vehicle granted to bearer with at least five closed drives, three closed charges and the data named by drive_id and charge_id.',
    'other_vehicle_id': 'A second vehicle granted to bearer (it may have no data).',
    'empty_vehicle_id': 'A vehicle granted to bearer that has no observation.',
    'drive_id': 'A drive of vehicle_id with at least three positions.',
    'other_drive_id': 'A second drive of vehicle_id.',
    'charge_id': 'A charge of vehicle_id with at least three samples.',
    'other_charge_id': 'A second charge of vehicle_id (optional: cursor-scope-binding also checks charge samples when it is supplied).',
    'disposable_bearer': 'Bearer of a device paired for this run only and granted vehicle_id; the run revokes it.',
    'rotation_bearer': 'Bearer of a device paired for this run only; the run rotates it.',
    'expired_grace_bearer': 'A prior bearer whose rotation grace window has ended. The descriptor attests this: the run cannot tell it from a bearer the Hub never issued, so name a bearer that really was rotated out.',
    'second_bearer': 'Bearer of a different device whose grant excludes ungranted_vehicle_id.',
    'ungranted_vehicle_id': 'An existing vehicle that is granted to bearer and not granted to second_bearer; the run reads it with bearer first, so a vehicle that does not exist is not_run, never a pass.',
    'ungranted_pack_digest': 'SHA-256 digest (64 lowercase hex) of a pack that bearer can download and that belongs only to vehicles not granted to second_bearer; the run requests it as <digest>.sqlite.zst with bearer first, so a digest the Hub does not hold is not_run, never a pass.',
    'invitation_path': 'Private file holding a fresh single-use invitation.',
    'update_request_path': 'Private marker path the harness watches to append one synthetic observation.',
    'update_receipt_path': 'Private path the harness writes when the appended observation is committed.',
}
GLOBAL_INPUTS = {'endpoint': 'Owned loopback HTTPS origin of the Hub under test.',
                 'certificate_path': 'PEM file used for normal CA and hostname validation.',
                 'tls_pin': 'Optional lowercase SHA-256 of the DER leaf certificate; checked on the connected socket when present.'}


def build_cases():
    by_kind = {route['kind']: route for route in [*ROUTES, *COMPANION_ROUTES]}
    cases = []

    def add(case_id, kind, status=None, *, inputs=(), route=None, method=None, **extra):
        item = {'id': case_id, 'kind': kind, 'inputs': list(inputs)}
        item['method'] = method or by_kind[kind]['method']
        item['route'] = route or by_kind[kind]['path']
        if status is not None:
            item['status'] = status
        item.update(extra)
        cases.append(item)

    base = ['bearer', 'vehicle_id']
    add('discovery-success', 'discovery', 200)
    add('health-success', 'health', 200)
    add('ready-success', 'ready', 200)
    add('vehicles-success', 'vehicles', 200, inputs=['bearer', 'vehicle_id'])
    add('current-success', 'current', 200, inputs=base)
    add('drives-success', 'drives', 200, inputs=base)
    add('claim-success', 'claim', 200, inputs=['invitation_path'],
        description='With the invitation file, over the TLS identity the invitation pins: six device_name values a Hub must refuse (empty, 129 bytes, 65 two-byte characters, a newline, DEL and a C1 control character) are each 422 with a bounded text/plain body and consume nothing; then the claim itself is 200.')
    add('rotate-success', 'rotate', 200, inputs=['rotation_bearer'], destructive=True)
    error_scope = {'kinds': ['drives', 'drive_positions', 'charges', 'charge_samples'], 'optional_inputs': ['drive_id', 'charge_id']}
    scope_text = 'Checked on drives and, when the descriptor supplies drive_id or charge_id, on every other page route.'
    descriptions = {
        'invalid_limit': 'A limit of 0 and a limit above the route maximum are 400 invalid_limit. ' + scope_text,
        'invalid_time_range': 'from_ms above to_ms and a negative from_ms are 400 invalid_time_range. ' + scope_text,
        'invalid_query': 'An unknown query parameter and a value that is not an integer of the parameter\'s type (limit=abc, limit=-1, from_ms=1.5, to_ms=9223372036854775808) are 400 invalid_query. ' + scope_text}
    for code, query in [('invalid_limit', '?limit=0'), ('invalid_time_range', '?from_ms=20&to_ms=10'), ('invalid_query', '?from=2026-09-01')]:
        add(code, 'drives', 400, inputs=base, route=DRIVES + query, error_code=code, **error_scope, description=descriptions[code])
    add('invalid_cursor', 'drives', 400, inputs=base + ['other_vehicle_id'], route=DRIVES + '?cursor={other_vehicle_cursor}', error_code='invalid_cursor',
        **error_scope, description='A cursor of another vehicle, a malformed cursor and a tampered tag are 400 invalid_cursor on drives and, when the descriptor supplies drive_id or charge_id, on every other page route.')
    add('drive-conditional', 'drives', 304, inputs=base, body='empty')
    add('pairing-replay', 'claim', 401, inputs=['invitation_path'], body='empty')
    add('missing-bearer', 'vehicles', 401, body='empty')
    add('bad-bearer', 'vehicles', 401, body='empty')
    add('rotated-bearer-grace-valid', 'vehicles', 200, inputs=['rotation_bearer'])
    add('rotation-lost-response-retry', 'rotate', 200, inputs=['rotation_bearer'], destructive=True,
        returns='same_successor', description='A retry of rotate with the prior bearer returns the first successor: access_token equals the first rotation\'s access_token.')
    add('cursor-time-binding', 'drives', 400, inputs=base, error_code='invalid_cursor')
    add('later-current', 'current', 200, inputs=base + ['update_request_path', 'update_receipt_path'])
    add('scenario-page-2', 'drives', 200, inputs=base)
    add('scenario-page-3', 'drives', 200, inputs=base)
    add('current-empty', 'current', 200, inputs=['bearer', 'empty_vehicle_id'])
    add('current-unknown', 'current', 404, inputs=['bearer'], body='empty')
    add('rotated-bearer-valid', 'vehicles', 200, inputs=['rotation_bearer'])

    drive_inputs = base + ['drive_id']
    charge_inputs = base + ['charge_id']
    add('drive-success', 'drive', 200, inputs=drive_inputs)
    add('drive-unknown', 'drive', 404, inputs=base, error_code='drive_not_found',
        description='An unknown id, and a non-numeric, zero, negative, out-of-range or undecodable (%FF) id, return one 404 drive_not_found shape; positions of an unknown drive too.')
    add('positions-success', 'drive_positions', 200, inputs=drive_inputs)
    add('positions-empty', 'drive_positions', 200, inputs=drive_inputs, description='A window beyond the data returns {"items": [], "next_cursor": null}.')
    add('positions-keyset-three-pages', 'drive_positions', 200, inputs=drive_inputs,
        description='Three consecutive pages at limit 1 have no duplicate and no missing id and match the first items of an unpaged read.')
    add('charges-success', 'charges', 200, inputs=base)
    add('charges-empty', 'charges', 200, inputs=base, description='A window beyond the data returns {"items": [], "next_cursor": null}.')
    add('charge-success', 'charge', 200, inputs=charge_inputs)
    add('charge-unknown', 'charge', 404, inputs=base, error_code='charge_not_found',
        description='An unknown id, and a non-numeric, zero, negative, out-of-range or undecodable (%FF) id, return one 404 charge_not_found shape; samples of an unknown charge too.')
    add('malformed-identifier-404', 'drives', 404, inputs=['bearer'], error_code='vehicle_not_found',
        kinds=['current', 'drives', 'drive', 'drive_positions', 'charges', 'charge', 'charge_samples', 'sync_manifest', 'pack'],
        description='With bearer, every route that takes a vehicle id answers each of abc (not a UUID), %FF (percent-decodes to invalid UTF-8) and the nil UUID 00000000-0000-0000-0000-000000000000 (no vehicle can have it) as not found, after the bearer check, with the answer of the route: an empty 404 on /current, JSON vehicle_not_found on the JSON routes, and on the sync manifest an empty 404 for abc and %FF and JSON vehicle_not_found for the nil UUID. The pack route answers a pack name of abc or %FF with an empty 404. The same %FF request without a valid bearer is 401 on every route, never 400. Every listed route is checked: a route the Hub does not advertise makes the whole case not_run.')
    add('charge-samples-success', 'charge_samples', 200, inputs=charge_inputs)
    add('page-conditional', 'drives', 304, inputs=base + ['drive_id', 'charge_id'], body='empty',
        kinds=['drives', 'drive_positions', 'charges', 'charge_samples'])
    add('cursor-kind-binding', 'charges', 400, inputs=base, error_code='invalid_cursor',
        kinds=['drives', 'charges'], description='A drives cursor presented to charges (and a charges cursor to drives) is 400 invalid_cursor.')
    add('cursor-vehicle-binding', 'drives', 400, inputs=base + ['other_vehicle_id'], error_code='invalid_cursor',
        kinds=['drives', 'charges'])
    add('cursor-scope-binding', 'drive_positions', 400, inputs=drive_inputs + ['other_drive_id'], optional_inputs=['other_charge_id', 'charge_id'],
        error_code='invalid_cursor', kinds=['drive_positions', 'charge_samples'],
        description='A positions cursor of drive A presented to drive B is 400 invalid_cursor; with charge_id and other_charge_id, a samples cursor of one charge presented to the other is too.')
    add('cursor-window-binding', 'drive_positions', 400, inputs=drive_inputs, optional_inputs=['charge_id'], error_code='invalid_cursor',
        kinds=['charges', 'drive_positions', 'charge_samples'],
        description='A cursor presented with a different from_ms or to_ms is 400 invalid_cursor on charges and positions and, when the descriptor supplies charge_id, on charge samples; drives is covered by cursor-time-binding.')
    add('page-byte-cap', 'drives', 200, inputs=base + ['drive_id', 'charge_id'], kinds=['drives', 'drive_positions', 'charges', 'charge_samples'],
        description='At the maximum limit no page exceeds 1048576 bytes.')
    add('ungranted-vehicle-404', 'drives', 404, inputs=['bearer', 'second_bearer', 'ungranted_vehicle_id'], error_code='vehicle_not_found',
        kinds=['current', 'drives', 'drive', 'drive_positions', 'charges', 'charge', 'charge_samples', 'sync_manifest'],
        description='First bearer reads /current of ungranted_vehicle_id (200; otherwise not_run). Then, for second_bearer, JSON routes answer vehicle_not_found, /current an empty 404 and the sync manifest the 404 an unknown vehicle gets (JSON vehicle_not_found), each identical to an unknown vehicle, and an invalid query on a page route does not turn the answer into 400. Every listed route is checked: a route the Hub does not advertise makes the whole case not_run.')
    add('ungranted-vehicles-list', 'vehicles', 200, inputs=['bearer', 'second_bearer', 'ungranted_vehicle_id'],
        description='The list of bearer includes ungranted_vehicle_id (otherwise not_run) and the list of second_bearer does not.')
    add('ungranted-pack-404', 'pack', 404, inputs=['bearer', 'second_bearer', 'ungranted_pack_digest'], body='empty',
        description='First bearer downloads ungranted_pack_digest, requested as <digest>.sqlite.zst (200; otherwise not_run). Then an ungranted pack digest is an empty 404 for second_bearer, identical to an unknown digest.')
    add('revoke-then-401', 'revoke', 204, inputs=['disposable_bearer', 'vehicle_id'], destructive=True, body='empty',
        description='First a non-empty body, including one over 4096 bytes, is 400 and revokes nothing. Then the revoke is 204 and every listed step is 401. All steps run or the whole case is not_run before anything is sent: a step whose capability is not advertised makes the case not_run.',
        kinds=['revoke', 'vehicles', 'current', 'charges', 'sync_manifest', 'pack'],
        steps=[{'kind': 'revoke', 'status': 204}, {'kind': 'vehicles', 'status': 401}, {'kind': 'current', 'status': 401}, {'kind': 'charges', 'status': 401},
               {'kind': 'sync_manifest', 'status': 401}, {'kind': 'pack', 'status': 401}])
    add('revoke-again-401', 'revoke', 401, inputs=['disposable_bearer', 'vehicle_id'], destructive=True, body='empty')
    add('rotation-grace-same-successor', 'rotate', 200, inputs=['rotation_bearer'], destructive=True, returns='same_successor',
        description='Repeated retries with the prior bearer inside the grace window return the same successor; no other token is issued.')
    add('rotation-after-grace-401', 'rotate', 401, inputs=['expired_grace_bearer'], body='empty', kinds=['rotate', 'vehicles'],
        description='A bearer whose grace window ended is 401 on rotate and on vehicles. expired_grace_bearer is descriptor-attested: a bearer the Hub never issued passes too.')
    add('discovery-profile', 'discovery', 200,
        description='Discovery names this profile and keeps the Home Assistant invariants: api_versions ["1.0"], protocol teslatlas-sync, protocol_major 1, query.vehicles and query.current.')
    return cases, {'profile_id': ID, 'global_inputs': GLOBAL_INPUTS, 'inputs': INPUTS, 'cases': cases}


def build_sync_regression():
    return {'profile_id': ID,
            'scope': 'Existing sync transport remains separate from query profile schema validation. These regression gates remain mandatory for sync clients.',
            'routes': ['GET /v1/vehicles/{vehicle_id}/sync/manifest', 'GET /v1/vehicles/{vehicle_id}/sync/noop', 'GET /v1/packs/sha256/{object_name}'],
            'access': 'Every vehicle-scoped sync route and the pack route enforce the paired device vehicle grant. A sync route answers an ungranted vehicle exactly as it answers an unknown vehicle, and each keeps the 404 the Hub gives an unknown vehicle today (profile.json access.sync_routes: the manifest and no-op 404s are asserted by hub-http-v1@1.2.0, because hub-sync-v1@1.3.0 lists none for them), and an ungranted pack digest is an empty 404 identical to an unknown digest.',
            'schema_2_2': {'delivery': 'Full snapshots only; explicit x-teslatlas-supported-schemas: 2.2 required. Do not request delta-v2 for schema 2.2.',
                           'noop_unavailable': {'status': 406, 'cache_control': 'no-store', 'body_bytes': 0, 'manifest_signature': 'absent',
                                                'meaning': 'No schema 2.2 no-op is available. This response is unsigned and is not a signed 503 or a snapshot.'}},
            'manifest': 'Verify x-teslatlas-manifest-signature as Ed25519 over exact downloaded bytes using discovery manifestPublicKey before parsing/using object references. Preserve existing binding and sequence checks.',
            'pack': 'Verify content-addressed SHA256, declared compressed length and decompression limits. Resume single byte Range using matching strong ETag/If-Range and validate Content-Range; never join incompatible objects.',
            'noop': 'A successful schema 2.2 no-op remains signed and has exact manifest/object bindings. When no no-op is available, use the unsigned 406 no-store response in sync.schema.json; do not substitute a signed 503 or an invented empty snapshot.',
            'regression_sources': ['hub/tests/tls_import_e2e.rs', 'hub/src/api/server/tests.rs', 'hub/src/sync/updates_delivery.rs'],
            'acceptance_boundary': 'Passing query conformance does not prove sync ingestion, schema-2.2 client correctness, migration preservation, or platform matrix completion.'}


def bundle():
    schemas, current, drive, position, charge, charge_sample = build_schemas()
    cases, cases_document = build_cases()
    output = {'profile.json': build_profile(), 'openapi.json': build_openapi(), 'field-semantics.json': build_semantics(current, drive, position, charge, charge_sample),
              'cases.json': cases_document, 'sync-regression.json': build_sync_regression(), **schemas}
    output.update({'examples/' + name + '.json': example for name, example in build_examples(current, drive, position, charge, charge_sample).items()})
    return {name: (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + '\n').encode() for name, value in output.items()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    files = bundle()
    files['SHA256SUMS'] = ''.join(hashlib.sha256(data).hexdigest() + '  ' + name + '\n' for name, data in sorted(files.items())).encode()
    if args.check:
        wrong = [name for name, data in files.items() if not (ROOT / name).is_file() or (ROOT / name).read_bytes() != data]
        extra = sorted(str(path.relative_to(ROOT)) for path in ROOT.rglob('*') if path.is_file() and str(path.relative_to(ROOT)) not in files) if ROOT.is_dir() else []
        if wrong or extra:
            raise SystemExit('stale hub-http-v1@1.2.0 artifacts: ' + ', '.join(wrong + ['unexpected ' + name for name in extra]))
    else:
        for name, data in files.items():
            target = ROOT / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            target.chmod(0o644)  # the tree is the same whatever the caller's umask
        for directory in {ROOT, *((ROOT / name).parent for name in files)}:
            directory.chmod(0o755)
    print(ID + ' sha256=' + hashlib.sha256(files['SHA256SUMS']).hexdigest())


if __name__ == '__main__':
    main()
