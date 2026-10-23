#!/usr/bin/env python3
"""Export the independently authored hub-http-v1@1.3.0 wire contract (Apache-2.0).

The minor is assembled from its accepted base, the committed hub-http-v1@1.2.0 directory (C6 R51), which is only read:
every schema, case, example and text of the base is kept, and the changes are the ones named here (C6 R47 to R49, R58,
R61, R62, R66 and R75). This authoring source contains public field and route facts, not Hub implementation.

    python tools/build_hub_http_profile_13.py          # write profiles/hub-http-v1/1.3.0
    python tools/build_hub_http_profile_13.py --check  # fail when the tree differs
"""
import argparse
import copy
import hashlib
import json
import re
from pathlib import Path

PROTOCOL = Path(__file__).resolve().parents[1]
BASE = PROTOCOL / 'profiles/hub-http-v1/1.2.0'
ROOT = PROTOCOL / 'profiles/hub-http-v1/1.3.0'
BASE_VERSION = '1.2.0'
VERSION = '1.3.0'
BASE_ID = 'hub-http-v1@' + BASE_VERSION
ID = 'hub-http-v1@' + VERSION
ALIAS_TEXT = 'Deprecated alias of {}; carries the same value.'  # C6 R51 (a), R58 (e) and (f)

INT = {'type': 'integer', 'format': 'int64', 'minimum': -(2 ** 63), 'maximum': 2 ** 63 - 1}
STR = {'type': 'string', 'maxLength': 65536}

# C6 R47: the 28 top-level renames (1.2.0 key, 1.3.0 key), and the two additions.
CURRENT_RENAMES = [
    ('state', 'activity_state'), ('since', 'state_since_ms'), ('healthy', 'processing_healthy'), ('geofence', 'geofence_name'),
    ('model', 'model_code'), ('version', 'software_version'), ('update_available', 'software_update_available'),
    ('update_status', 'software_update_status'), ('update_version', 'software_update_version'),
    ('download_perc', 'software_update_download_perc'), ('install_perc', 'software_update_install_perc'),
    ('speed', 'speed_kmh'), ('odometer', 'odometer_km'), ('rated_battery_range_km', 'battery_range_km'),
    ('active_route_miles_to_arrival', 'active_route_km_to_arrival'), ('plugged_in', 'charge_cable_connected'),
    ('doors_open', 'any_door_open'), ('windows_open', 'any_window_open'),
    ('driver_front_door_open', 'door_driver_front_open'), ('driver_rear_door_open', 'door_driver_rear_open'),
    ('passenger_front_door_open', 'door_passenger_front_open'), ('passenger_rear_door_open', 'door_passenger_rear_open'),
    ('driver_front_window_open', 'window_driver_front_open'), ('driver_rear_window_open', 'window_driver_rear_open'),
    ('passenger_front_window_open', 'window_passenger_front_open'), ('passenger_rear_window_open', 'window_passenger_rear_open'),
    ('frunk_open', 'front_trunk_open'), ('trunk_open', 'rear_trunk_open'),
]
CURRENT_ADDITIONS = {'model_name': {'anyOf': [STR, {'type': 'null'}]}, 'model_year': {'anyOf': [INT, {'type': 'null'}]}}
# C6 R48: the `car` object. `model_code` is nullable here because the Hub code is absent when neither the VIN nor the stored text states a model;
# its alias `model` stays the non-null stored text.
CAR_RENAMES = [('model', 'model_code'), ('marketing_name', 'variant_name'), ('source_eid', 'api_vehicle_id'), ('source_vid', 'stream_vehicle_id')]
CAR_ADDITIONS = {'model_name': CURRENT_ADDITIONS['model_name'], 'model_year': CURRENT_ADDITIONS['model_year']}
# C6 R49: the `car.settings` keys.
SETTINGS_RENAMES = [
    ('enabled', 'collection_enabled'), ('use_streaming_api', 'streaming_enabled'), ('suspend_min', 'sleep_window_min'),
    ('suspend_after_idle_min', 'sleep_after_idle_min'), ('suspend_min_resolved', 'sleep_window_min_resolved'),
    ('req_not_unlocked', 'sleep_requires_locked'), ('free_supercharging', 'supercharging_free'), ('lfp_battery', 'battery_lfp'),
]
# C6 R58 (a): the 33 history renames, by item.
HISTORY_RENAMES = {
    'drive': [
        ('start_date_ms', 'started_at_ms'), ('end_date_ms', 'ended_at_ms'), ('start_ideal_range_km', 'start_ideal_battery_range_km'),
        ('end_ideal_range_km', 'end_ideal_battery_range_km'), ('start_rated_range_km', 'start_battery_range_km'),
        ('end_rated_range_km', 'end_battery_range_km'), ('start_geofence', 'start_geofence_name'), ('end_geofence', 'end_geofence_name'),
        ('speed_max', 'speed_max_kmh'), ('power_max', 'power_max_kw'), ('power_min', 'power_min_kw'),
        ('outside_temp_avg', 'outside_temp_avg_c'), ('inside_temp_avg', 'inside_temp_avg_c'), ('ascent', 'ascent_m'), ('descent', 'descent_m'),
    ],
    'charge': [
        ('start_date_ms', 'started_at_ms'), ('end_date_ms', 'ended_at_ms'), ('start_ideal_range_km', 'start_ideal_battery_range_km'),
        ('end_ideal_range_km', 'end_ideal_battery_range_km'), ('start_rated_range_km', 'start_battery_range_km'),
        ('end_rated_range_km', 'end_battery_range_km'), ('geofence', 'geofence_name'), ('billing_type', 'tariff_type'),
        ('cost_per_unit', 'tariff_rate'), ('session_fee', 'tariff_session_fee'), ('charge_energy_added', 'charge_energy_added_kwh'),
        ('outside_temp_avg', 'outside_temp_avg_c'),
    ],
    'position': [('date_ms', 'timestamp_ms'), ('speed', 'speed_kmh'), ('odometer', 'odometer_km'), ('rated_battery_range_km', 'battery_range_km')],
    'charge_sample': [('ideal_range_km', 'ideal_battery_range_km'), ('rated_range_km', 'battery_range_km')],
}
# The two aliases of `current` that do not carry their replacement's value (C6 R50).
CURRENT_EXCEPTIONS = {
    'active_route_miles_to_arrival': 'Deprecated alias of active_route_km_to_arrival; carries the provider miles value as given, because its name states miles.',
}
CAR_EXCEPTIONS = {
    'model': 'Deprecated alias of model_code; carries the stored model text as 1.2.0 served it.',
}
# 1.3.0 route order and window keys (C6 R58 (g)); the nested positions page keeps its parent scope.
ORDER_KEYS = {'drives': 'started_at_ms', 'charges': 'started_at_ms', 'drive_positions': 'timestamp_ms'}

# The semantic handoffs of C1 R95, C2 R82 and C8 R48, each under the new key it names (B3 addenda v2, sections 5.2 to 5.4).
HANDOVER = {  # (item, key): (nullable, unit or None to keep the base unit, source text); B3 addenda v2 sections 5.2 to 5.4
    ('drive', 'ascent_m'): (True, 'metres',
     "Total climb of the drive in whole metres. Saturated at 32767: a larger total is served as 32767 and is then a lower bound. The drive's positions keep their own elevations."),
    ('drive', 'descent_m'): (True, 'metres',
     "Total descent of the drive in whole metres. Saturated at 32767: a larger total is served as 32767 and is then a lower bound. The drive's positions keep their own elevations."),
    ('drive', 'duration_min'): (True, 'minutes, integer',
     'Whole minutes of the observed drive, from `started_at_ms` to `ended_at_ms`. Saturated at 32767. When the drive starts from a recent parked observation, `distance_km` and the start values include the short stretch before the first movement observation, which this duration does not cover.'),
    ('drive', 'distance_km'): (True, 'km',
     "Distance of the drive in km, from the odometer. When the Hub saw the car parked within its gap interval before the drive's first movement observation, the distance and the start values (start coordinates, battery level and ranges) are taken from that parked observation. They then include the stretch driven before the first movement observation, while `started_at_ms` and `duration_min` cover only the observed part. Distance divided by duration can therefore overstate the average speed by up to one parked polling interval of driving."),
    ('drive', 'ended_at_ms'): (False, 'epoch milliseconds',
     "End of the drive. When the car did not state the end (for example a source that reports no gear, or a drive closed because evidence stopped arriving), the end is the Hub's last evidence that the drive was in progress: the first observation without movement evidence that follows the drive's last observation with movement evidence, or, if there is none, the last observation folded into the drive. An observation without movement evidence does not show that the car had stopped. Observations after that point are not counted in the drive's duration, distance or averages. The end is never earlier than `started_at_ms`."),
    ('charge', 'started_at_ms'): (False, 'epoch milliseconds',
     "Start of the session. A session the Hub first saw already in progress starts at its first sample, which is then later than the true start (an upper bound); its added energy is still complete. A session the Hub records after a period without data, because the car's own energy counter shows energy was added while the Hub saw nothing, starts at the last time up to which the car's energy had been accounted for, which is earlier than the true start (a lower bound)."),
    ('charge', 'ended_at_ms'): (False, 'epoch milliseconds',
     'End of the session. A session that continued across a period without data and then ended has as its end the first observation after that period, an upper bound: the true end lies between the last sample before the period and this time. A session recorded after a period without data (see `started_at_ms`) also ends at the first observation after it, an upper bound. For both, `duration_min` is an upper bound, and the stored samples show the period without data. A per-minute cost is computed only when the stored samples show no unobserved stretch at the start or the end of the session (see `cost`).'),
    ('charge', 'charge_energy_used_kwh'): (True, 'kWh',
     "Energy that entered the car through its charge inlet during the session, as the car reports it, over the same span as `charge_energy_added_kwh`. AC session (`is_dc` false): the on-board charger's input energy, computed from the AC input power. It is what a meter before the car registers, apart from cable losses. Because it is computed from apparent power, it can overstate the real energy where the power factor is below 1 (about 1 % at 0.99, about 5 % at 0.95). DC session (`is_dc` true): the integral of the DC charging power the source reports. From sources with the Owner API's charge-state shape, on the one car measured so far, it was about 7 % above the battery-side counter (median of 81 sessions, never below), consistent with a reading taken before the battery. From Fleet Telemetry it is not yet measured. Unknown kind (`is_dc` null): the integral of the per-sample power the Hub selects. That is inlet energy for an AC session, and the reported DC power's integral for a DC session that showed no kind evidence. Null when not measured; no energy is invented for unobserved time. It prices no DC session (see `cost`)."),
    ('charge', 'cost'): (True, 'currency units; no currency code is carried; decimal serialised as a JSON number',
     "Cost of the session in currency units. Null means the cost is unknown: no complete tariff applied, its energy or time basis is missing, or nobody entered one. Zero is a known zero. Per-kWh tariffs: AC and unknown-kind sessions are priced on `charge_energy_used_kwh`, DC sessions on `charge_energy_added_kwh` only; a DC session without that figure has no per-kWh cost. Per-minute tariffs (`tariff_type` `per_minute`): the cost covers the whole connected time from `started_at_ms` to `ended_at_ms`, pauses included, in unrounded minutes. It is null when the stored samples show an unobserved stretch at either end of the session: the first sample is more than ten minutes after `started_at_ms`, the last two samples are more than ten minutes apart, or `ended_at_ms` is more than ten minutes after the last sample (the cases described under those keys). A session first seen in progress starts at its first sample and is priced over its observed time; a gap inside a session that later samples close is connected time and is priced; a row with no stored samples is priced over its own recorded start and end. Free Supercharging: when the owner's free-Supercharging setting is on, a session typed `Supercharger`, or a DC session of another DC type whose samples carry the charger brand Tesla and no other brand, costs zero. The session fee is added once per row (see `tariff_session_fee`). The Hub's automatic close and fill never overwrite a stored cost: a cost the import carried and a cost the owner entered stay. The owner can replace any stored cost, an imported one included. A total the owner enters replaces the cost and leaves the row's tariff unchanged. A unit price the owner enters, per kWh or per minute, replaces the row's tariff with that type and rate and no session fee, and the cost becomes the price on that tariff, with no free-charging rule and no fence fee; a per-minute price follows the per-minute rule above."),
    ('charge', 'tariff_type'): (True, 'text; known values per_kwh and per_minute; preserve unknown values',
     "The tariff recorded on the charge. For a charge the Hub collected: the tariff copied from the owner's geofence that contained its start point when it closed, or, when the owner asked the Hub to fill its missing cost after saving that geofence's tariff, the tariff of that geofence as the owner had just saved it. Later geofence edits never change it. It is null when no complete tariff was copied. For an imported charge: the tariff of the geofence the import source linked to the charge, as the import source held it when the history was imported; it is not a copy made when the session closed, and one of the three tariff keys can be null while another is not. For a charge whose price the owner entered as a unit price: the type the owner gave, which replaces any earlier tariff."),
    ('charge', 'tariff_rate'): (True, 'currency units per kWh when tariff_type is per_kwh, per minute when per_minute; decimal serialised as a JSON number',
     'Rate of the tariff recorded on the charge; `tariff_type` states which tariff it is and how and when it was recorded, and later geofence edits never change it. Currency units per kWh when `tariff_type` is `per_kwh`, per minute when `per_minute`. A unit price the owner enters replaces it.'),
    ('charge', 'tariff_session_fee'): (True, 'currency units; decimal serialised as a JSON number',
     "Session fee of the tariff recorded on the charge (see `tariff_type`). On a row the Hub collected that continues the previous row's plug-in (the car's own charging session continued with no unplug, drive or movement in between), the fee is waived so that a fee is charged once per connection: it is 0 when the tariff had a fee, and null, as in the tariff, when it had none. A unit price the owner enters replaces the tariff and clears the fee (null)."),
    ('charge', 'is_dc'): (True, 'boolean',
     'Session kind, by one rule for collected sessions and for closed imported sessions whose import applied that rule. True once any sample shows DC evidence (a DC charger type, the fast-charger flag with a type that is not Tesla AC equipment, or a power no AC inlet can carry), and then stays true. False when samples show AC evidence and none shows DC. Null when no sample says. A closed imported session whose import stored its kind by an earlier rule keeps that value until it is imported again.'),
    ('charge', 'fast_charger_type'): (True, 'text',
     'Charger type of the session, by one rule for collected sessions and for closed imported sessions whose import applied that rule: the trimmed type of the first sample whose type carries information. Later values do not change it. Values that say nothing (empty, `<invalid>`, `Unknown`, `SNA`, `Other`, `Tesla`) are skipped. A closed imported session whose import stored its type by an earlier rule keeps that value until it is imported again.'),
    ('drive', 'start_geofence_name'): (True, 'text',
     "For a row the Hub collected: the name of the owner's geofence that contained the row's stored point when the row closed (the drive's start or end point; the charge's start point). Where fences overlap, the one with the smallest radius, then the nearest centre. Null when none did or the point is missing. For an imported row: the label the import source recorded. When the owner later creates, edits or deletes a geofence, stored rows are relabelled by one rule. A row whose point the change moves into or out of a fence takes the Hub's new answer there. A label that named a renamed fence takes the new name. Any other label, an imported one included, is kept, unless every fence of that name was deleted; then a row with a point takes the Hub's current answer there. Tariff fields and cost are never relabelled."),
    ('drive', 'end_geofence_name'): (True, 'text',
     "For a row the Hub collected: the name of the owner's geofence that contained the row's stored point when the row closed (the drive's start or end point; the charge's start point). Where fences overlap, the one with the smallest radius, then the nearest centre. Null when none did or the point is missing. For an imported row: the label the import source recorded. When the owner later creates, edits or deletes a geofence, stored rows are relabelled by one rule. A row whose point the change moves into or out of a fence takes the Hub's new answer there. A label that named a renamed fence takes the new name. Any other label, an imported one included, is kept, unless every fence of that name was deleted; then a row with a point takes the Hub's current answer there. Tariff fields and cost are never relabelled."),
    ('charge', 'geofence_name'): (True, 'text',
     "For a row the Hub collected: the name of the owner's geofence that contained the row's stored point when the row closed (the drive's start or end point; the charge's start point). Where fences overlap, the one with the smallest radius, then the nearest centre. Null when none did or the point is missing. For an imported row: the label the import source recorded. When the owner later creates, edits or deletes a geofence, stored rows are relabelled by one rule. A row whose point the change moves into or out of a fence takes the Hub's new answer there. A label that named a renamed fence takes the new name. Any other label, an imported one included, is kept, unless every fence of that name was deleted; then a row with a point takes the Hub's current answer there. Tariff fields and cost are never relabelled."),
}
CAR_EFFICIENCY_TEXT = 'Battery energy that adds one km of rated range, in Wh per km: the median of the figures of recent Hub-measured AC sessions. Before three such sessions exist, it is the value imported from the source database, whose basis the Hub does not guarantee.'

# C6 R61: the two import provenance texts, exactly.
IMPORT_ADDRESS_TEXT = ("The address was copied unchanged from the history of an import source, the source database the Hub's owner migrated from. "
                       "The Hub did not look it up and does not replace it with a lookup. The Hub states nothing about how the import source obtained it.")
IMPORT_ELEVATION_TEXT = ("The elevation was copied unchanged from the history of an import source, the source database the Hub's owner migrated from. "
                         "The Hub did not sample it from terrain data and does not replace it. The Hub states nothing about how the import source obtained it.")

# C6 R51 (a), R28 to R41: the `current` entries the summary derives, written from the specification and not from the base prose.
NEWEST = 'The newest statement of the field decides; when it is not valid the field is null and no older value is used.'
PERSISTENT = 'Persistent: the last known value is kept through sleep however old it is; compare observed_at_ms with your own clock.'
TRANSIENT = ('Transient: null while activity_state is asleep or offline; null while the online interval of the car is open and the statement was received '
             "more than 60 seconds before that interval began; and null when the statement is older than the Hub's staleness bound (the configured offline "
             "drive timeout, 600 seconds by default), measured at the later of observed_at_ms and the Hub's clock.")


def persistent(text):
    return text + ' ' + NEWEST + ' ' + PERSISTENT


def transient(text):
    return text + ' ' + NEWEST + ' ' + TRANSIENT


CURRENT_TEXT = {
    'activity_state': ('text; online, asleep, offline, driving, charging, updating or null',
                       "The Hub's own state of the car from its stored lifecycle phase: online (also while suspended), asleep, offline, driving, charging or "
                       'updating; null when the phase is unknown. Without a readable lifecycle record the newest vehicle-data, stream or discovery row decides. '
                       'There is no age rule: the value stays the last recorded phase however old it is, so compare observed_at_ms with your own clock.'),
    'state_since_ms': (None,
                       'The start of the condition activity_state names. driving: the first movement observation, an upper bound of the true start. charging: '
                       'the first sample of a charge first seen in progress (an upper bound), or, for a charge that continued after a gap, the last time the '
                       "car's energy was accounted for (a lower bound). updating: the first installing status. online, asleep and offline: the time of the "
                       'observation that first gave that reachability, not the end of the last session. Null when the matching record is absent or the '
                       'lifecycle record cannot be read.'),
    'processing_healthy': (None,
                           "True when the Hub's processing record for this car exists and is not quarantined; false when it is quarantined or cannot be read; "
                           "null when the Hub holds no record. It reports the Hub's own processing of the car, the condition readiness reports, not the "
                           "car's condition."),
    'geofence_name': ('text',
                      "Name of the owner's geofence that contains the reported position, computed on each read from the rounded coordinates and never stored; "
                      'null when the position is unknown or no fence contains it. Where fences overlap, the smallest radius wins, then the nearest centre.'),
    'model_code': ('text; Hub model code S, X, 3, Y, R or C, or the model text passed through trimmed',
                   'The Hub model code: S, X, 3, Y, R or C when the VIN or the car\'s own model statement identifies the model, otherwise the model text the car '
                   'or the import source gave, trimmed and passed through; null when neither states a model. A VIN decoding published by the regulator '
                   'decides over any text.'),
    'model_name': ('text',
                   'Model name derived from model_code by the Hub (Model S, Model X, Model 3, Model Y, Roadster or Cybertruck); null for a pass-through code.'),
    'model_year': ('year, integer',
                   'Model year decoded from position 10 of a valid VIN with a regulator-published Tesla manufacturer code, in the window 2008 to 2037; null '
                   'for any other VIN, an unpublished manufacturer code or no VIN.'),
    'software_version': ('text',
                         "The car's software version without its build hash: the Hub's tracked version, else the newest reported version, else the stored "
                         'firmware version; the text before the first whitespace, which must be at least two dot-separated groups of 1 to 9 digits. ' + PERSISTENT),
    'software_update_available': ('boolean',
                                  'True while an update is offered to the car and not yet installed (status available, scheduled, downloading, '
                                  'downloading_wifi_wait, wifi_wait or installing); false for the idle status (an empty text); null for any other or no status. ' + PERSISTENT),
    'software_update_status': ('text or provider enum',
                               "The update status the car reports, trimmed and verbatim; null when idle (an empty text), absent or not a text. " + PERSISTENT),
    'software_update_version': ('text',
                                'Version of the offered update, normalised like software_version; null unless software_update_available is true. ' + PERSISTENT),
    'software_update_download_perc': ('percent', transient('Download progress of the offered update, an integer from 0 to 100.')),
    'software_update_install_perc': ('percent', transient('Installation progress of the offered update, an integer from 0 to 100.')),
    'speed_kmh': ('km/h, rounded integer from provider mph', transient('Speed converted from the provider mph value, an integer from 0 to 32767.')),
    'odometer_km': ('km, provider miles converted unrounded; imported values as given',
                    persistent('Odometer in km: the provider miles value converted, or the import source value as given; finite and not negative.')),
    'battery_range_km': ('km, provider miles converted and rounded to 0.01 km',
                         persistent('Rated battery range in km, from 0 to 9999.99: the provider miles value converted and rounded to 0.01 km, or the import source '
                                    'value rounded to 0.01 km.')),
    'active_route_km_to_arrival': ('km, provider miles converted and rounded to 0.01 km',
                                   transient('Remaining distance of the active route in km, finite and not negative.')),
    'charge_cable_connected': ('boolean',
                               'False when the newest charging state is Disconnected; true for Charging, Starting, Stopped, Complete or NoPower; otherwise from the '
                               'connected cable text (an invalid cable text is false, any other non-empty text is true); null when neither is stated. Plug state is '
                               'physical and persists through sleep.'),
    'any_door_open': ('boolean',
                      'True if any of the four door elements is open; false only if all four are known closed; otherwise null. The two trunks are not included. ' + PERSISTENT),
    'any_window_open': ('boolean',
                        'True if any of the four window elements is open or partly open; false only if all four are known closed; otherwise null. ' + PERSISTENT),
}
CURRENT_DOORS = ('door_driver_front_open', 'door_driver_rear_open', 'door_passenger_front_open', 'door_passenger_rear_open')
CURRENT_WINDOWS = ('window_driver_front_open', 'window_driver_rear_open', 'window_passenger_front_open', 'window_passenger_rear_open')
CURRENT_TRUNKS = ('front_trunk_open', 'rear_trunk_open')
for _name in CURRENT_DOORS:
    CURRENT_TEXT[_name] = ('boolean', persistent('The newest statement for this door: true when open (any non-zero code), false when closed; null when unknown.'))
for _name in CURRENT_WINDOWS:
    CURRENT_TEXT[_name] = ('boolean', persistent('The newest statement for this window: true when open or partly open, false when closed; null when unknown.'))
for _name in CURRENT_TRUNKS:
    CURRENT_TEXT[_name] = ('boolean', persistent('The newest statement for this trunk: true when open (any non-zero code), false when closed; null when unknown.'))
# C6 R27 to R28, R38 to R40: the rule of every other field the summary derives, each written from the specification (source key, conversion, valid range,
# class P or T). The unit of each entry stays the base unit.
POSITION_WHEEL = {'fl': 'front left', 'fr': 'front right', 'rl': 'rear left', 'rr': 'rear right'}
CURRENT_TEXT.update({
    'latitude': (None, persistent('Latitude in degrees, rounded to 6 decimals, from the newest statement that gives latitude and longitude together; null when the pair is '
                                  'not a valid coordinate. Both coordinates always come from the same statement.')),
    'longitude': (None, persistent('Longitude in degrees, rounded to 6 decimals, from the newest statement that gives latitude and longitude together; null when the pair '
                                   'is not a valid coordinate. Both coordinates always come from the same statement.')),
    'heading': (None, persistent('Heading in degrees as given, from 0 to 360 inclusive; 360 is kept and not turned into 0. Outside that range or not finite gives null.')),
    'elevation': (None, persistent('Elevation in whole metres, from -32768 to 32767. Each row gives its elevation when that is valid, otherwise its native location '
                                   'elevation when that is valid; the newest row decides.')),
    'power': (None, transient('Drive power in whole kW (rounded half away from zero), from -32768 to 32767.')),
    'shift_state': (None, transient('The gear after normalising the provider text: P, R, N or D; any other value gives null.')),
    'active_route_destination': (None, transient('The destination name of the active route, trimmed; null when empty or longer than 65536 characters.')),
    'active_route_latitude': (None, transient('Latitude of the active route destination in degrees, rounded to 6 decimals, from the newest statement that gives both '
                                              'route coordinates together; null when the pair is not a valid coordinate.')),
    'active_route_longitude': (None, transient('Longitude of the active route destination in degrees, rounded to 6 decimals, from the newest statement that gives '
                                               'both route coordinates together; null when the pair is not a valid coordinate.')),
    'active_route_minutes_to_arrival': (None, transient('Minutes to arrival on the active route as given; finite and not negative.')),
    'active_route_traffic_minutes_delay': (None, transient('Minutes of traffic delay on the active route as given; finite and not negative.')),
    'active_route_energy_at_arrival': (None, transient('Expected battery charge at arrival on the active route, as the provider gives it; finite.')),
    'battery_level': (None, persistent('Battery level in whole percent, from 0 to 100, from the charge state or from the import source battery level.')),
    'usable_battery_level': (None, persistent('Usable battery level in whole percent, from 0 to 100.')),
    'ideal_battery_range_km': (None, persistent('Ideal battery range in km, from 0 to 9999.99: the provider miles value converted and rounded to 0.01 km, or the import '
                                                'source value rounded to 0.01 km.')),
    'est_battery_range_km': (None, persistent('Estimated battery range in km, from 0 to 9999.99: the provider miles value converted and rounded to 0.01 km, or the import '
                                              'source value rounded to 0.01 km.')),
    'charge_limit_soc': (None, persistent('Charge limit in whole percent, from 0 to 100.')),
    'charge_energy_added': (None, persistent('Energy added in the current or last charge, in kWh rounded to 2 decimals, from 0 to 999999.99.')),
    'charge_current_request': (None, persistent('Requested charging current in whole amperes; not negative.')),
    'charge_current_request_max': (None, persistent('Highest requestable charging current in whole amperes; not negative.')),
    'scheduled_charging_start_time': (None, persistent("Start of the scheduled charge as the provider's epoch seconds, unchanged and not converted to milliseconds; "
                                                        'not negative.')),
    'charge_port_door_open': (None, persistent('Whether the charge port door is open; a JSON boolean as given, any other type gives null.')),
    'charger_power': (None, transient('Charger power in whole kW, from -32768 to 32767.')),
    'charger_voltage': (None, transient('Charger voltage in whole volts, from -32768 to 32767.')),
    'charger_actual_current': (None, transient('Charger current in whole amperes, from -32768 to 32767.')),
    'charger_phases': (None, transient('Number of charger phases as a whole number, from 0 to 32767.')),
    'time_to_full_charge': (None, transient('Hours until the charge is complete, as given; finite and not negative.')),
    'inside_temp': (None, transient('Cabin temperature in degrees Celsius, rounded to 1 decimal, from -999.9 to 999.9.')),
    'outside_temp': (None, transient('Outside temperature in degrees Celsius, rounded to 1 decimal, from -999.9 to 999.9.')),
    'is_climate_on': (None, transient('Whether the climate system is on; a JSON boolean as given, any other type gives null.')),
    'is_preconditioning': (None, transient('Whether the car is preconditioning; a JSON boolean as given, any other type gives null.')),
    'climate_keeper_mode': (None, transient('The climate keeper mode, trimmed; null when empty or longer than 65536 characters.')),
    'locked': (None, persistent('Whether the car is locked; a JSON boolean as given, any other type gives null.')),
    'sentry_mode': (None, transient('Whether Sentry Mode is on; a JSON boolean as given, any other type gives null.')),
    'is_user_present': (None, transient('Whether a user is present in the car; a JSON boolean as given, any other type gives null.')),
    'center_display_state': (None, transient('The provider state code of the centre display, an exact non-negative integer passed through without interpretation; a '
                                             'fraction or a negative number gives null.')),
    'sun_roof_percent_open': (None, persistent('Sunroof opening in whole percent, from 0 to 100.')),
    'sun_roof_state': (None, persistent('The sunroof state text, trimmed; null when empty or longer than 65536 characters.')),
    'sun_roof_installed': (None, persistent('Whether a sunroof is fitted, from the vehicle configuration: a JSON boolean as given, the exact integer 0 as false and any '
                                            'other exact integer as true.')),
    'service_mode': (None, 'Whether the car is in service mode: the Hub lifecycle record when it states one, otherwise the newest service mode boolean the car '
                           'reported, otherwise null.'),
    'display_name': (None, 'The name the owner gave the car: the newest display name the vehicle list states, else the newest vehicle name of the vehicle '
                           'state, else the stored car name when it is not empty; trimmed; null when none is usable. A source whose newest statement is not '
                           'usable text (not a string, empty after trimming or longer than 65536 characters) passes to the next source. ' + PERSISTENT),
    'trim_badging': (None, persistent('Trim badging from the newest vehicle configuration statement; when there is none, the stored car value; trimmed text, null when '
                                      'empty or longer than 65536 characters.')),
    'exterior_color': (None, persistent('Exterior colour from the newest vehicle configuration statement; when there is none, the stored car value; trimmed text, null '
                                        'when empty or longer than 65536 characters.')),
    'wheel_type': (None, persistent('Wheel type from the newest vehicle configuration statement; when there is none, the stored car value; trimmed text, null when '
                                    'empty or longer than 65536 characters.')),
    'spoiler_type': (None, persistent('Spoiler type from the newest vehicle configuration statement; when there is none, the stored car value; trimmed text, null when '
                                      'empty or longer than 65536 characters.')),
})
for _wheel, _label in POSITION_WHEEL.items():
    CURRENT_TEXT['tpms_pressure_' + _wheel] = (None, persistent('Tyre pressure of the ' + _label + ' wheel in bar, rounded to 1 decimal, from 0 to 999.9.'))
    CURRENT_TEXT['tpms_soft_warning_' + _wheel] = (None, persistent('Whether the tyre pressure soft warning of the ' + _label + ' wheel is set; a JSON boolean as given, '
                                                                   'any other type gives null.'))
OBSERVED_AT_TEXT = ('The greatest observed_at_ms over the rows the Hub accepts for the car from any source; null when there are none. It is the time the Hub last '
                    'received information about the car, not a measurement time. The Hub gives no freshness guarantee beyond it: compare it with your own clock, '
                    'because activity_state and the persistent fields keep their last known values however old they are.')
CHARGING_STATE_TEXT = ('The charging state, normalised: Charging, Starting, Stopped, Complete, Disconnected and NoPower in that spelling; Unknown and null give '
                       'null; any other text is passed through trimmed. While activity_state is asleep, Charging and Starting are null. '
                       'Otherwise persistent: the last known value is kept through sleep however old it is; compare observed_at_ms with your own clock.')
CAR_UNIT = ('optional materialised identity; api_vehicle_id/stream_vehicle_id/id signed64 (aliases source_eid/source_vid), efficiency_wh_per_km Wh/km, '
            'settings durations minutes')
CAR_SOURCE_PREFIX = 'car.efficiency_wh_per_km: '
BASE_GENERIC_SOURCE = 'Hub admitted observation/lifecycle/materialised identity'  # the 1.2.0 sentence every unwritten current entry carries

# C6 R66: the neutral substitutions in the two carried-forward texts, placed by their neighbours; no other character changes.
NUMBER_RULE_SPAN = re.compile(r'(?<=are whole numbers in )\S+(?= but are typed number)')
TRACE_LABEL_SPAN = re.compile(r'(?<=^Hub import)/\S+(?= projection)')
TRACE_INDEX = 4


def json_text(value):
    """The bytes of one generated file: sorted keys, two-space indent, U+007F to U+009F as escapes, one final newline (the 1.2.0 form)."""
    text = json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + '\n'
    return re.sub('[\u007f-\u009f]', lambda match: '\\u%04x' % ord(match.group()), text).encode()


def strict_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate member')
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique)


def load_base():
    """Every file of the base minor except its checksum list, as parsed documents; each must be in the canonical form json_text writes."""
    documents = {}
    for path in sorted(BASE.rglob('*.json')):
        raw = path.read_bytes()
        value = strict_json(raw.decode('utf-8'))
        name = str(path.relative_to(BASE))
        if json_text(value) != raw:
            raise SystemExit('the base file is not in canonical form: ' + name)
        documents[name] = value
    return documents


def moved(schema):
    return copy.deepcopy(schema)


def deprecate(schema, replacement, text=None):
    return {**schema, 'deprecated': True, 'description': text or ALIAS_TEXT.format(replacement)}


def rename_properties(definition, renames, additions=(), exceptions=None):
    """C6 R58 (e), R51 (a): each new key takes its old key's schema and requiredness; each old key stays, deprecated, naming its replacement."""
    exceptions = exceptions or {}
    properties, required = definition['properties'], definition['required']
    new_required = []
    for old, new in renames:
        if old not in properties or new in properties:
            raise SystemExit('rename %s -> %s does not fit the base' % (old, new))
        base_schema = properties[old]
        properties[new] = moved(base_schema)
        properties[old] = deprecate(base_schema, new, exceptions.get(old))
        if old in required:
            new_required.append(new)
    for name, schema in dict(additions).items():
        if name in properties:
            raise SystemExit('addition %s exists in the base' % name)
        properties[name] = copy.deepcopy(schema)
        new_required.append(name)
    definition['required'] = required + new_required


def build_resources(base):
    resources = copy.deepcopy(base['resources.schema.json'])
    defs = resources['$defs']
    current = defs['current']
    car_schema = current['properties']['car']['anyOf'][0]
    rename_properties(car_schema['properties']['settings'], SETTINGS_RENAMES)
    rename_properties(car_schema, CAR_RENAMES, CAR_ADDITIONS, CAR_EXCEPTIONS)
    car_schema['properties']['model_code'] = {'anyOf': [STR, {'type': 'null'}]}
    rename_properties(current, CURRENT_RENAMES, CURRENT_ADDITIONS, CURRENT_EXCEPTIONS)
    for item, renames in HISTORY_RENAMES.items():
        rename_properties(defs[item], renames)
    resources['$id'] = retarget(resources['$id'])
    return resources


def retarget(text):
    return text.replace('urn:teslatlas:hub-http-v1:' + BASE_VERSION + ':', 'urn:teslatlas:hub-http-v1:' + VERSION + ':')


def alias_entry(new_entry, replacement, source=None):
    return {'nullable': new_entry['nullable'], 'source': source or ALIAS_TEXT.format(replacement), 'unit': new_entry['unit']}


def build_semantics(base):
    document = copy.deepcopy(base['field-semantics.json'])
    document['profile_id'] = ID
    # C6 R66: only the two named substitutions in the carried-forward texts.
    rule = document['number_rule']
    if len(NUMBER_RULE_SPAN.findall(rule)) != 1:
        raise SystemExit('number_rule does not hold the one span of C6 R66')
    document['number_rule'] = NUMBER_RULE_SPAN.sub('the source database', rule)
    label = document['source_trace'][TRACE_INDEX]
    if len(TRACE_LABEL_SPAN.findall(label)) != 1:
        raise SystemExit('source_trace entry 4 does not hold the one span of C6 R66')
    document['source_trace'][TRACE_INDEX] = TRACE_LABEL_SPAN.sub('', label)
    # C6 R61: the two import texts, exactly.
    document['address_provenance']['source']['values']['import'] = IMPORT_ADDRESS_TEXT
    document['elevation_provenance']['source']['values']['import'] = IMPORT_ELEVATION_TEXT

    current = document['current']
    for old, new in CURRENT_RENAMES:
        entry = current[new] = copy.deepcopy(current[old])
        unit, text = CURRENT_TEXT.get(new, (None, None))
        if unit is not None:
            entry['unit'] = unit
        if text is not None:
            entry['source'] = text
        alias_unit = entry['unit']
        source = CURRENT_EXCEPTIONS.get(old)
        if old == 'active_route_miles_to_arrival':
            alias_unit = base['field-semantics.json']['current'][old]['unit']  # the alias keeps the miles it carried
        current[old] = {**alias_entry(entry, new, source), 'unit': alias_unit}
    for name in CURRENT_ADDITIONS:
        current[name] = {'nullable': True, 'source': CURRENT_TEXT[name][1], 'unit': CURRENT_TEXT[name][0]}
    renamed = {new for _, new in CURRENT_RENAMES} | set(CURRENT_ADDITIONS)
    for name, (unit, text) in CURRENT_TEXT.items():
        if name not in current:
            raise SystemExit('derived text for an unknown key: ' + name)
        if name not in renamed:  # a key that keeps its name: the rule is written from the specification, the unit stays the base unit
            if unit is not None:
                current[name]['unit'] = unit
            current[name]['source'] = text
    current['observed_at_ms']['source'] = OBSERVED_AT_TEXT
    current['charging_state']['source'] = CHARGING_STATE_TEXT
    current['car']['unit'] = CAR_UNIT
    current['car']['source'] = CAR_SOURCE_PREFIX + CAR_EFFICIENCY_TEXT
    for name, entry in current.items():
        if entry.get('source', '').startswith(BASE_GENERIC_SOURCE):
            raise SystemExit('a derived current entry keeps the generic base source: ' + name)

    for item, renames in HISTORY_RENAMES.items():
        entries = document[item]
        for old, new in renames:
            entries[new] = copy.deepcopy(entries[old])
        for (handover_item, key), (nullable, unit, text) in HANDOVER.items():
            if handover_item != item:
                continue
            entry = entries[key]
            if entry['nullable'] != nullable:
                raise SystemExit('hand-over nullability differs from the base: %s.%s' % (item, key))
            if key != 'tariff_rate' and entry['unit'] != unit:
                raise SystemExit('hand-over unit differs from the base: %s.%s' % (item, key))
            entry['source'] = text
            entry['unit'] = unit
        for old, new in renames:
            entries[old] = alias_entry(entries[new], new)
    return document


def build_cases(base):
    document = copy.deepcopy(base['cases.json'])
    document['profile_id'] = ID
    document['inputs'].update({
        'full_page_vehicle_id': 'A vehicle granted to bearer with more closed drives and more closed charges than fit in 1 MiB at the largest limit of each route.',
        'full_page_drive_id': 'A drive of full_page_vehicle_id with more positions than fit in 1 MiB at the largest positions limit.',
        'full_page_charge_id': 'A charge of full_page_vehicle_id with more samples than fit in 1 MiB at the largest samples limit.',
    })
    route = {'current': '/v1/vehicles/{vehicle_id}/current', 'drive': '/v1/vehicles/{vehicle_id}/drives/{drive_id}', 'charge': '/v1/vehicles/{vehicle_id}/charges/{charge_id}',
             'drive_positions': '/v1/vehicles/{vehicle_id}/drives/{drive_id}/positions',
             'charge_samples': '/v1/vehicles/{vehicle_id}/charges/{charge_id}/samples',
             'drives': '/v1/vehicles/{vehicle_id}/drives', 'charges': '/v1/vehicles/{vehicle_id}/charges'}
    pairs = {item: [{'alias': old, 'replacement': new} for old, new in renames] for item, renames in HISTORY_RENAMES.items()}
    alias_cases = [
        ('drive-aliases', 'drive', 'drive', ['bearer', 'vehicle_id', 'drive_id'], 'One served drive'),
        ('charge-aliases', 'charge', 'charge', ['bearer', 'vehicle_id', 'charge_id'], 'One served charge'),
        ('position-aliases', 'drive_positions', 'position', ['bearer', 'vehicle_id', 'drive_id'], 'The first position served for drive_id'),
        ('charge-sample-aliases', 'charge_samples', 'charge_sample', ['bearer', 'vehicle_id', 'charge_id'], 'The first sample served for charge_id'),
    ]
    cases = [{'id': 'current-aliases', 'kind': 'current', 'method': 'GET', 'route': route['current'], 'status': 200, 'inputs': ['bearer', 'vehicle_id'],
              'aliases': [{'scope': scope, 'alias': old, 'replacement': new}
                          for scope, renames in (('current', CURRENT_RENAMES), ('car', CAR_RENAMES), ('settings', SETTINGS_RENAMES)) for old, new in renames],
              'exceptions': ['current.active_route_miles_to_arrival', 'current.car.model'],
              'description': 'The current body of vehicle_id, which carries a car with its settings, holds for every deprecated alias of the current summary, of its car '
                             'and of its settings the same JSON value as its replacement (both keys present and equal), except the two named exceptions, '
                             'whose alias must be present and keeps its own value: active_route_miles_to_arrival the provider miles value as given, and '
                             'car.model the stored model text.'}]
    for case_id, kind, item, inputs, subject in alias_cases:
        cases.append({'id': case_id, 'kind': kind, 'method': 'GET', 'route': route[kind], 'status': 200, 'inputs': inputs, 'item': item,
                      'aliases': pairs[item],
                      'description': subject + ' carries, for every deprecated alias of this item, the same JSON value as its replacement (both keys present, equal, '
                                     'and the replacement listed in the item schema).'})
    full_cases = [
        ('drives-full-page', 'drives', ['bearer', 'full_page_vehicle_id'], 'Drives'),
        ('charges-full-page', 'charges', ['bearer', 'full_page_vehicle_id'], 'Charges'),
        ('positions-full-page', 'drive_positions', ['bearer', 'full_page_vehicle_id', 'full_page_drive_id'], 'Positions'),
        ('charge-samples-full-page', 'charge_samples', ['bearer', 'full_page_vehicle_id', 'full_page_charge_id'], 'Charge samples'),
    ]
    for case_id, kind, inputs, subject in full_cases:
        cases.append({'id': case_id, 'kind': kind, 'method': 'GET', 'route': route[kind], 'status': 200, 'inputs': inputs,
                      'description': subject + ' over a fixture with more items than fit in 1 MiB at the largest limit: the first page ends at the byte cap, its '
                                     'last item is complete, next_cursor is present, and the second page starts with the item after the first page\'s last, so '
                                     'that no item is served twice or skipped.'})
    document['cases'] = document['cases'] + cases
    return document


def build_profile(base):
    profile = copy.deepcopy(base['profile.json'])
    profile['profile_id'] = ID
    profile['contract_version'] = VERSION
    profile['previous_profile'] = BASE_ID
    for route in profile['routes']:
        key = ORDER_KEYS.get(route['kind'])
        if key is not None:
            route['order']['keys'][0] = key
            route['window_key'] = key
    profile['cursor']['legacy']['payload_layout'] = profile['cursor']['legacy']['payload_layout'].replace('last start_date_ms', 'last started_at_ms')
    profile['deprecated_aliases'] = {
        'current': dict(CURRENT_RENAMES), 'car': dict(CAR_RENAMES), 'settings': dict(SETTINGS_RENAMES),
        **{item: dict(renames) for item, renames in HISTORY_RENAMES.items()},
        'exceptions': {
            'current.active_route_miles_to_arrival': 'carries the provider miles value as given',
            'current.car.model': 'carries the stored model text',
        },
        'rule': 'Each deprecated alias stays a required property with deprecated true and carries the same JSON value as its replacement, except the two '
                'listed exceptions. A client reads the new keys; the aliases stay until a client release reads only the new names, and their removal is one '
                'contract change for the current and the history aliases together.',
    }
    return profile


def build_examples(base):
    examples = {}
    for name, value in base.items():
        if not name.startswith('examples/'):
            continue
        value = copy.deepcopy(value)
        stem = name[len('examples/'):-len('.json')]
        if stem == 'current':
            value = copy_aliases(value, CURRENT_RENAMES, CURRENT_ADDITIONS)
            value['car'] = copy_aliases(value['car'], CAR_RENAMES, CAR_ADDITIONS)
            value['car']['settings'] = copy_aliases(value['car']['settings'], SETTINGS_RENAMES)
        elif stem in ('drive', 'charge', 'position', 'charge-samples', 'drives', 'charges', 'positions'):
            item = {'drive': 'drive', 'drives': 'drive', 'charge': 'charge', 'charges': 'charge', 'positions': 'position', 'charge-samples': 'charge_sample'}.get(stem, stem)
            if 'items' in value:
                value['items'] = [copy_aliases(entry, HISTORY_RENAMES[item]) for entry in value['items']]
            else:
                value = copy_aliases(value, HISTORY_RENAMES[item])
        elif stem in ('discovery', 'discovery-unsigned'):
            value['profile'] = ID
        examples[name] = value
    return examples


def copy_aliases(value, renames, additions=()):
    value = dict(value)
    for old, new in renames:
        value[new] = copy.deepcopy(value[old])
    for name in dict(additions):
        value[name] = None
    return value


def build_openapi(base):
    document = copy.deepcopy(base['openapi.json'])
    document['info']['version'] = VERSION
    return document


def build_sync_regression(base):
    document = copy.deepcopy(base['sync-regression.json'])
    document['profile_id'] = ID
    return document


def bundle():
    base = load_base()
    output = {
        'profile.json': build_profile(base), 'openapi.json': build_openapi(base), 'field-semantics.json': build_semantics(base),
        'cases.json': build_cases(base), 'sync-regression.json': build_sync_regression(base), 'resources.schema.json': build_resources(base),
    }
    for name in ('auth.schema.json', 'cursor.schema.json', 'discovery.schema.json', 'errors.schema.json', 'rotation-grace.schema.json', 'sync.schema.json'):
        schema = copy.deepcopy(base[name])
        schema['$id'] = retarget(schema['$id'])
        output[name] = schema
    output.update(build_examples(base))
    unused = sorted(set(base) - set(output))
    if unused:
        raise SystemExit('base files left out: ' + ', '.join(unused))
    return {name: json_text(value) for name, value in output.items()}


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
            raise SystemExit('stale hub-http-v1@1.3.0 artifacts: ' + ', '.join(wrong + ['unexpected ' + name for name in extra]))
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
