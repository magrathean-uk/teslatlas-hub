"""Independent hub-http-v1@1.2.0 admission checks. No Hub implementation is imported.

Two layers share one loader:

* Offline profile checks (`check_offline`) run on the checked-in profile directory only.
  They are local contract evidence, never evidence about a running Hub.
* Native checks (`run_native`) exercise an owned loopback Hub described by a private
  descriptor. A case whose inputs, advertised capability or data are missing is reported
  as `not_run` with the missing item named; it never counts as passed.

Descriptor (a private owner-only JSON file, never printed or logged)::

    {"schema_version": 1, "kind": "hub-http-v1-actual-hub", "profile_id": "hub-http-v1@1.2.0",
     "endpoint": "https://127.0.0.1:PORT", "certificate_path": "/abs/ca-or-leaf.pem",
     "tls_pin": "<optional lowercase sha256 of the DER leaf>",
     "inputs": {"bearer": "...", "vehicle_id": "...", ...}}

The input names, and which case needs which, are `inputs` and `cases[].inputs` in the
profile's `cases.json`. Output carries case identifiers and fixed labels only.

Exit status: 0 when every requested check passed (the offline checks, and with `--config` every native
case), 1 when a check failed, 3 when nothing failed but a requested check did not run.
"""
import argparse
import copy
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import secrets
import ssl
import struct
import sys
import time
import urllib.parse
import uuid

from jsonschema import Draft202012Validator, FormatChecker

try:
    from . import hub_http
except ImportError:  # direct execution through conformance/runner.py or this file
    import hub_http

ROOT = Path(__file__).resolve().parents[1]
PROFILE_ID = 'hub-http-v1@1.2.0'
DEFAULT_PROFILE = ROOT / 'profiles/hub-http-v1/1.2.0'
SCHEMA_DRAFT = 'https://json-schema.org/draft/2020-12/schema'
MAX_BYTES = 1048576
I64_MAX = 2 ** 63 - 1
REQUIRED_FILES = frozenset({
    'profile.json', 'openapi.json', 'discovery.schema.json', 'errors.schema.json', 'auth.schema.json', 'resources.schema.json',
    'cursor.schema.json', 'rotation-grace.schema.json', 'sync.schema.json', 'field-semantics.json', 'cases.json', 'sync-regression.json'})
REQUIRED_CASE_IDS = frozenset({
    'discovery-success', 'health-success', 'ready-success', 'vehicles-success', 'current-success', 'drives-success', 'claim-success', 'rotate-success',
    'invalid_limit', 'invalid_time_range', 'invalid_query', 'invalid_cursor', 'drive-conditional', 'pairing-replay', 'missing-bearer', 'bad-bearer',
    'rotated-bearer-grace-valid', 'rotation-lost-response-retry', 'cursor-time-binding', 'later-current', 'scenario-page-2', 'scenario-page-3',
    'current-empty', 'current-unknown', 'rotated-bearer-valid',
    'drive-success', 'drive-unknown', 'positions-success', 'positions-empty', 'positions-keyset-three-pages', 'charges-success', 'charges-empty',
    'charge-success', 'charge-unknown', 'malformed-identifier-404', 'charge-samples-success', 'page-conditional', 'cursor-kind-binding', 'cursor-vehicle-binding',
    'cursor-scope-binding', 'cursor-window-binding', 'page-byte-cap', 'ungranted-vehicle-404', 'ungranted-vehicles-list', 'ungranted-pack-404',
    'revoke-then-401', 'revoke-again-401', 'rotation-grace-same-successor', 'rotation-after-grace-401', 'discovery-profile'})
CAPABILITY_TOKEN = re.compile(r'[a-z0-9.]{1,64}')
CURSOR_KEY_CAPABILITIES = ('query.drives', 'query.charges', 'query.positions', 'query.charge.samples', 'sync.packs')
CURSOR_KIND = {'drives': 1, 'charges': 2, 'drive_positions': 3, 'charge_samples': 4}
TQP1 = re.compile(r'tqp1\.([0-9a-f]{116})\.([0-9a-f]{64})')
TQD1 = re.compile(r'tqd1\.([0-9a-f]{98})\.([0-9a-f]{64})')
ETAG = re.compile(r'"[0-9a-f]{64}"')
BEARER_CHALLENGE = re.compile(r'(?i)bearer(?:[\s,]|$)')
SUMS_LINE = re.compile(r'([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._/-]*)')
INPUT_KINDS = {
    'bearer': 'bearer', 'second_bearer': 'bearer', 'disposable_bearer': 'bearer', 'rotation_bearer': 'bearer', 'expired_grace_bearer': 'bearer',
    'vehicle_id': 'uuid', 'other_vehicle_id': 'uuid', 'empty_vehicle_id': 'uuid', 'ungranted_vehicle_id': 'uuid',
    'drive_id': 'id', 'other_drive_id': 'id', 'charge_id': 'id', 'other_charge_id': 'id',
    'ungranted_pack_digest': 'digest', 'invitation_path': 'path', 'update_request_path': 'path', 'update_receipt_path': 'path'}
DESTRUCTIVE_BEARERS = ('disposable_bearer', 'rotation_bearer')


class ContractError(ValueError):
    """Profile or document problem. Messages are fixed labels, never server data."""


class CheckFailure(Exception):
    """One native or offline check failed; the message is a fixed label."""


class NotRun(Exception):
    """A check could not run; `missing` names what it needed."""

    def __init__(self, *missing):
        super().__init__(', '.join(missing))
        self.missing = list(missing)


# ---------------------------------------------------------------------------
# Strict loading and bundle verification
# ---------------------------------------------------------------------------
def strict_json(raw):
    """Strict JSON: UTF-8 only, no BOM, no duplicate members, no NaN or overflow (repository loader)."""
    try:
        text = bytes(raw).decode('utf-8') if isinstance(raw, (bytes, bytearray)) else raw
        return hub_http.strict_json(text)
    except (ValueError, UnicodeError, RecursionError, TypeError):
        raise ContractError('document is not strict JSON') from None


def read_json(path):
    return strict_json(Path(path).read_bytes())


def verify_bundle(root=DEFAULT_PROFILE):
    """SHA256SUMS must list exactly the on-disk files and every digest must match."""
    root = Path(root)
    try:
        sums = (root / 'SHA256SUMS').read_bytes()
        text = sums.decode('ascii')
    except (OSError, UnicodeError):
        raise ContractError('profile manifest is unreadable') from None
    if not text.endswith('\n'):
        raise ContractError('profile manifest is invalid')
    listed = {}
    for line in text[:-1].split('\n'):
        match = SUMS_LINE.fullmatch(line)
        if match is None or '..' in Path(match.group(2)).parts or match.group(2) in listed:
            raise ContractError('profile manifest is invalid')
        listed[match.group(2)] = match.group(1)
    actual = set()
    for path in root.rglob('*'):
        if path.is_symlink():
            raise ContractError('profile contains a symbolic link')
        if path.is_file() and path.name != 'SHA256SUMS':
            actual.add(path.relative_to(root).as_posix())
        elif path.is_file():
            if path.parent != root:
                raise ContractError('profile contains an unlisted manifest')
    if set(listed) != actual:
        raise ContractError('profile manifest does not list exactly the profile files')
    for name, digest in listed.items():
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != digest:
            raise ContractError('profile file digest mismatch')
    if not REQUIRED_FILES <= set(listed):
        raise ContractError('profile is incomplete')
    profile = read_json(root / 'profile.json')
    if profile.get('profile_id') != PROFILE_ID:
        raise ContractError('unsupported profile identity')
    for name in profile.get('examples', {}):
        if 'examples/' + name + '.json' not in listed:
            raise ContractError('profile example is missing')
    return dict(profile, profile_sha256=hashlib.sha256(sums).hexdigest())


# ---------------------------------------------------------------------------
# Schema helpers
# ---------------------------------------------------------------------------
_MAP_KEYWORDS = ('properties', 'patternProperties', '$defs', 'definitions', 'dependentSchemas')
_SCHEMA_KEYWORDS = ('items', 'additionalProperties', 'unevaluatedItems', 'unevaluatedProperties', 'contains', 'not', 'if', 'then', 'else',
                    'propertyNames', 'contentSchema')
_LIST_KEYWORDS = ('allOf', 'anyOf', 'oneOf', 'prefixItems')


def walk_schema(node, path=''):
    """Yield (pointer, subschema) for every schema node, keyword-aware (a property named `items` is not an array)."""
    if not isinstance(node, dict):
        return
    yield path, node
    for key in _MAP_KEYWORDS:
        if isinstance(node.get(key), dict):
            for name, child in node[key].items():
                yield from walk_schema(child, path + '/' + key + '/' + name)
    for key in _SCHEMA_KEYWORDS:
        if key in node:
            yield from walk_schema(node[key], path + '/' + key)
    for key in _LIST_KEYWORDS:
        if isinstance(node.get(key), list):
            for index, child in enumerate(node[key]):
                yield from walk_schema(child, path + '/' + key + '/' + str(index))


def iter_openapi_schemas(document):
    def media(content):
        for item in (content or {}).values():
            yield from walk_schema(item.get('schema'), '/media')

    for path, item in document.get('paths', {}).items():
        for method, operation in item.items():
            if not isinstance(operation, dict):
                continue
            where = '/paths/' + path + '/' + method
            for parameter in operation.get('parameters', []):
                yield from ((where + '/parameters/' + str(parameter.get('name')) + pointer, node) for pointer, node in walk_schema(parameter.get('schema')))
            yield from ((where + '/requestBody' + pointer, node) for pointer, node in media(operation.get('requestBody', {}).get('content')))
            for status, response in operation.get('responses', {}).items():
                yield from ((where + '/' + status + pointer, node) for pointer, node in media(response.get('content')))
                for name, header in response.get('headers', {}).items():
                    yield from ((where + '/' + status + '/headers/' + name + pointer, node) for pointer, node in walk_schema(header.get('schema')))
    for name, schema in document.get('components', {}).get('schemas', {}).items():
        yield from (('/components/schemas/' + name + pointer, node) for pointer, node in walk_schema(schema))


def is_array_schema(node):
    declared = node.get('type')
    return declared == 'array' or (isinstance(declared, list) and 'array' in declared) or 'items' in node or 'prefixItems' in node


def unbounded_arrays(nodes):
    """Pointers of array schemas that lack an integer maxItems."""
    return [pointer or '/' for pointer, node in nodes if is_array_schema(node) and type(node.get('maxItems')) is not int]


def strict_variant(document):
    """Server-strict copy: additionalProperties false on every object schema that declares properties."""
    result = copy.deepcopy(document)
    for _, node in walk_schema(result):
        if isinstance(node.get('properties'), dict):
            node['additionalProperties'] = False
    return result


def schema_problems(validator, value, limit=3):
    """Bounded labels: schema location and keyword only, never instance values."""
    errors = sorted(validator.iter_errors(value), key=lambda error: [str(part) for part in error.absolute_path])
    return [('/'.join(str(part) for part in error.absolute_path) or '$') + ': ' + str(error.validator) for error in errors[:limit]]


# ---------------------------------------------------------------------------
# Profile
# ---------------------------------------------------------------------------
class Profile:
    """The loaded 1.2.0 profile directory: documents, routes and validators."""

    def __init__(self, root=DEFAULT_PROFILE, *, verify=True):
        self.root = Path(root)
        self.info = verify_bundle(self.root) if verify else None
        self._documents = {}
        self._validators = {}
        self.profile = self.document('profile.json')
        self.routes = {route['kind']: route for route in [*self.profile['routes'], *self.profile.get('companion_routes', [])]}

    def document(self, name):
        if name not in self._documents:
            self._documents[name] = strict_json((self.root / name).read_bytes())
        return self._documents[name]

    def validator(self, name, definition=None, strict=False):
        key = (name, definition, strict)
        if key not in self._validators:
            document = self.document(name)
            document = strict_variant(document) if strict else copy.deepcopy(document)
            if definition:
                document = {**document, '$ref': '#/$defs/' + definition}
            self._validators[key] = Draft202012Validator(document, format_checker=FormatChecker())
        return self._validators[key]

    def reference_validator(self, reference, strict=False):
        name, _, fragment = reference.partition('#')
        return self.validator(name, fragment.rsplit('/', 1)[-1] if fragment else None, strict)

    def body_reference(self, kind):
        route = self.routes[kind]
        operation = self.document('openapi.json')['paths'][route['path']][route['method'].lower()]
        return operation['responses']['200']['content']['application/json']['schema']['$ref']


def as_profile(value):
    return value if isinstance(value, Profile) else Profile(value, verify=False)


# ---------------------------------------------------------------------------
# Cursors, capabilities and page semantics
# ---------------------------------------------------------------------------
def decode_cursor(text):
    """Structure of a tqp1 cursor, for conformance only; clients never parse cursors and the tag needs the Hub key."""
    match = TQP1.fullmatch(text) if isinstance(text, str) else None
    if match is None:
        raise ContractError('cursor is not tqp1')
    payload = bytes.fromhex(match.group(1))
    if payload[0] != 1 or payload[1] not in CURSOR_KIND.values():
        raise ContractError('cursor header is invalid')
    scope, from_ms, to_ms, key_a, key_b = struct.unpack('>qqqqq', payload[18:58])
    return {'kind': payload[1], 'vehicle_id': str(uuid.UUID(bytes=payload[2:18])), 'scope_id': scope, 'from_ms': from_ms,
            'to_ms': to_ms, 'key_a': key_a, 'key_b': key_b}


def capability_problems(tokens):
    if not isinstance(tokens, list) or len(tokens) > 16:
        return ['capabilities must be an array of at most 16 tokens']
    if not all(isinstance(token, str) and CAPABILITY_TOKEN.fullmatch(token) for token in tokens):
        return ['capability token violates the token grammar']
    if len(set(tokens)) != len(tokens):
        return ['capability tokens are not unique']
    return []


def check_discovery(value, *, profile_id=None, tokens=None):
    """Semantic discovery rules on top of the schema: grammar, Home Assistant invariants, advertisement rules.

    `tokens` is the set a Hub of this profile may advertise (profile.json `capabilities`). A Hub that names the
    profile is held to it: clients ignore unknown tokens, servers do not add them within 1.2.0.
    """
    if not isinstance(value, dict):
        return ['discovery is not an object']
    capabilities = value.get('capabilities')
    problems = capability_problems(capabilities)
    if problems:
        return problems
    for token in ('query.vehicles', 'query.current'):
        if token not in capabilities:
            problems.append('discovery lacks ' + token)
    if value.get('api_versions') != ['1.0'] or value.get('protocol') != 'teslatlas-sync' or value.get('protocol_major') != 1:
        problems.append('discovery violates the Home Assistant invariants')
    present = {token for token in CURSOR_KEY_CAPABILITIES if token in capabilities}
    if present and present != set(CURSOR_KEY_CAPABILITIES):
        problems.append('cursor-key capabilities are advertised partially')
    if 'query.enrichment' in capabilities and not value.get('attributions'):
        problems.append('query.enrichment requires attributions')
    if tokens is not None and any(token not in tokens for token in capabilities):
        problems.append('discovery advertises a token outside the profile')
    if profile_id is not None and value.get('profile') != profile_id:
        problems.append('discovery names a different profile')
    return problems


def check_page(profile, kind, page, *, vehicle_id=None, scope_id=None, from_ms=0, to_ms=I64_MAX, limit=None):
    """Order, window, ownership and cursor-structure rules for one page (labels only)."""
    route = profile.routes[kind]
    keys, descending = route['order']['keys'], route['order']['direction'] == 'descending'
    window_key = route['window_key']
    items = page['items']
    problems = []
    if limit is not None and len(items) > limit:
        problems.append('page exceeds the requested limit')
    previous = None
    for item in items:
        if not from_ms <= item[window_key] < to_ms:
            problems.append('item lies outside the requested window')
        if vehicle_id is not None and item.get('vehicle_id', vehicle_id) != vehicle_id:
            problems.append('item belongs to another vehicle')
        if scope_id is not None and item.get('drive_id', item.get('charge_id', scope_id)) != scope_id:
            problems.append('item belongs to another parent')
        key = tuple(item[name] for name in keys)
        if previous is not None and not (key < previous if descending else key > previous):
            problems.append('items are not strictly ordered')
            break
        previous = key
    cursor = page['next_cursor']
    if cursor is not None:
        if not items:
            problems.append('cursor on an empty page')
        else:
            try:
                fields = decode_cursor(cursor)
            except ContractError:
                return problems + ['next_cursor is not a tqp1 cursor']
            last = items[-1]
            expected = {'kind': CURSOR_KIND[kind], 'from_ms': from_ms, 'to_ms': to_ms, 'key_a': last[keys[0]], 'key_b': last[keys[1]]}
            if vehicle_id is not None:
                expected['vehicle_id'] = vehicle_id
            expected['scope_id'] = scope_id if (scope_id is not None and route.get('parent')) else 0
            if any(fields[name] != value for name, value in expected.items()):
                problems.append('next_cursor does not describe this page')
    return problems


def check_sequence(profile, kind, pages):
    """Continuity across consecutive pages: no duplicate and no ordering reversal at a boundary."""
    route = profile.routes[kind]
    keys, descending = route['order']['keys'], route['order']['direction'] == 'descending'
    seen, previous = set(), None
    for page in pages:
        for item in page['items']:
            key = tuple(item[name] for name in keys)
            if item['id'] in seen:
                return ['an item appears on two pages']
            seen.add(item['id'])
            if previous is not None and not (key < previous if descending else key > previous):
                return ['pages are not continuous in order']
            previous = key
    return []


# ---------------------------------------------------------------------------
# Raw response validation
# ---------------------------------------------------------------------------
def _json_content_type(headers):
    return headers.get('content-type', '').split(';')[0].strip().lower() == 'application/json'


def validate_raw(profile, kind, status, headers, raw, *, strict=True):
    """Return bounded diagnostic labels for one HTTP response; raw values never appear."""
    profile = as_profile(profile)
    headers = {key.lower(): value for key, value in headers.items()}
    if len(raw) > MAX_BYTES:
        return ['response exceeds byte limit']
    if kind == 'invitation':
        return _validate_invitation(profile, raw, strict)
    route = profile.routes.get(kind)
    if route is None:
        return ['unknown route kind']
    shape = route['responses'].get(str(status))
    if shape is None and status == profile.profile.get('limits', {}).get('handler_timeout_response', {}).get('status'):
        shape = 'empty'  # the profile-wide answer when the handler time budget expires, on any route
    if shape is None:
        return ['status not specified by profile']
    if route.get('etag_required') and status in (200, 304):
        etag = headers.get('etag', '')
        if ETAG.fullmatch(etag) is None:
            return ['page ETag missing or not a quoted SHA-256']
        if headers.get('cache-control') != 'no-store':
            return ['page Cache-Control must be no-store']
        if status == 200 and etag != '"' + hashlib.sha256(raw).hexdigest() + '"':
            return ['page ETag does not match the body']
    if status == 401 and route['bearer'] and BEARER_CHALLENGE.match(headers.get('www-authenticate', '')) is None:
        return ['401 lacks the Bearer challenge']
    if shape == 'any':
        return []
    if shape == 'text':
        if raw and headers.get('content-type', '').startswith('text/plain'):
            return []
        return ['expected extractor text error']
    if shape == 'empty' or (shape in ('empty_or_error', 'empty_or_ready') and raw == b''):
        return [] if raw == b'' else ['expected empty response body']
    if not _json_content_type(headers):
        return ['expected JSON content type']
    try:
        value = strict_json(raw)
        if shape == 'resource':
            validator = profile.reference_validator(profile.body_reference(kind), strict)
        elif shape in ('ready', 'empty_or_ready'):
            validator = profile.validator('resources.schema.json', 'ready', strict)
        else:
            validator = profile.validator('errors.schema.json', None, strict)
        found = schema_problems(validator, value)
        if found:
            return ['body violates ' + kind + ' schema (' + found[0] + ')']
        if shape in ('error', 'empty_or_error'):
            allowed = route.get('error_codes', {}).get(str(status), profile.profile['error_codes'])
            if value['error']['code'] not in allowed:
                return ['error code does not match status']
            if headers.get('cache-control') != 'no-store':
                return ['JSON error Cache-Control must be no-store']
        if shape in ('ready', 'empty_or_ready') and value['status'] != 'not_ready':
            return ['readiness status mismatch']
        if shape == 'resource' and kind == 'ready' and value['status'] != 'ready':
            return ['readiness status mismatch']
        if shape == 'resource' and kind == 'discovery':
            return check_discovery(value)
    except (ContractError, KeyError, TypeError, RecursionError):
        return ['invalid JSON response']
    return []


def _validate_invitation(profile, raw, strict):
    try:
        value = strict_json(raw)
        if schema_problems(profile.validator('auth.schema.json', 'invitation', strict), value):
            return ['body violates invitation schema']
        uri = urllib.parse.urlsplit(value['pairingUri'])
        expected = {'pairing_id': [value['pairingId']], 'secret': [value['secret']], 'endpoint': [value['endpoint']], 'tls_pin': [value['tlsPin']]}
        if uri.scheme != 'teslatlas-hub' or uri.netloc != 'pair' or uri.path or uri.fragment or urllib.parse.parse_qs(uri.query, strict_parsing=True) != expected:
            return ['invitation URI does not match its fields']
    except (ContractError, KeyError, TypeError, ValueError):
        return ['invalid JSON response']
    return []


# ---------------------------------------------------------------------------
# Offline checks
# ---------------------------------------------------------------------------
def _json_pointer(document, pointer):
    for token in [part.replace('~1', '/').replace('~0', '~') for part in pointer.split('/')[1:]]:
        document = document[token]
    return document


def _check_bundle(profile):
    verify_bundle(profile.root)


def _check_schema_documents(profile):
    for name in sorted(path.name for path in profile.root.glob('*.schema.json')):
        document = profile.document(name)
        try:
            Draft202012Validator.check_schema(document)
        except Exception:
            raise CheckFailure(name + ' is not a valid Draft 2020-12 schema') from None
        if document.get('$schema') != SCHEMA_DRAFT or document.get('$id') != 'urn:teslatlas:hub-http-v1:1.2.0:' + name.removesuffix('.schema.json'):
            raise CheckFailure(name + ' has the wrong $schema or $id')


def _check_openapi(profile):
    try:
        from openapi_spec_validator import validate
    except ImportError:
        raise NotRun('openapi-spec-validator') from None
    document = profile.document('openapi.json')
    try:
        validate(document, base_uri=profile.root.as_uri() + '/')
    except Exception:
        raise CheckFailure('openapi.json is not a valid OpenAPI 3.1 document') from None
    if document.get('openapi') != '3.1.0' or document['info'].get('version') != '1.2.0':
        raise CheckFailure('openapi.json has the wrong version')
    routes = [route for route in profile.profile['routes']]
    declared = {(route['path'], route['method'].lower()) for route in routes}
    served = {(path, method) for path, item in document['paths'].items() for method in item}
    if declared != served:
        raise CheckFailure('OpenAPI operations differ from the profile routes')
    for route in routes:
        operation = document['paths'][route['path']][route['method'].lower()]
        if operation['operationId'] != route['kind'] or sorted(operation['responses']) != sorted(str(status) for status in route['statuses']):
            raise CheckFailure('OpenAPI operation differs from route ' + route['kind'])
        if bool(operation['security']) != route['bearer']:
            raise CheckFailure('OpenAPI security differs from route ' + route['kind'])
        limits = {parameter['name']: parameter['schema'] for parameter in operation['parameters'] if parameter['in'] == 'query'}
        if 'limit' in route and (limits['limit']['maximum'], limits['limit']['default'], limits['limit']['minimum']) != (route['limit']['max'], route['limit']['default'], route['limit']['min']):
            raise CheckFailure('OpenAPI limit differs from route ' + route['kind'])
    names, listed = set(), set(path.relative_to(profile.root).as_posix() for path in profile.root.rglob('*') if path.is_file())

    def references(node):
        if isinstance(node, dict):
            for key, child in node.items():
                if key == '$ref' and isinstance(child, str):
                    yield child
                else:
                    yield from references(child)
        elif isinstance(node, list):
            for child in node:
                yield from references(child)

    for reference in references(document):
        name, _, fragment = reference.partition('#')
        if name not in listed:
            raise CheckFailure('OpenAPI references a missing file')
        names.add(name)
        if fragment:
            try:
                _json_pointer(profile.document(name), fragment)
            except (KeyError, TypeError):
                raise CheckFailure('OpenAPI reference does not resolve') from None


def _check_examples(profile, strict):
    mapping = profile.profile['examples']
    on_disk = sorted(path.stem for path in (profile.root / 'examples').glob('*.json'))
    if on_disk != sorted(mapping):
        raise CheckFailure('examples differ from the profile example index')
    for name, reference in sorted(mapping.items()):
        value = read_json(profile.root / 'examples' / (name + '.json'))
        problems = schema_problems(profile.reference_validator(reference, strict), value)
        if problems:
            raise CheckFailure('example ' + name + ' violates its ' + ('server-strict ' if strict else '') + 'schema (' + problems[0] + ')')


def _check_max_items(profile):
    nodes = []
    for path in sorted(profile.root.glob('*.schema.json')):
        nodes += [(path.name + pointer, node) for pointer, node in walk_schema(profile.document(path.name))]
    nodes += list(iter_openapi_schemas(profile.document('openapi.json')))
    missing = unbounded_arrays(nodes)
    if missing:
        raise CheckFailure('array schema without maxItems at ' + missing[0])
    bounds = profile.profile['limits']['array_max_items']
    resources = profile.document('resources.schema.json')['$defs']
    observed = {'vehicles': resources['vehicles']['properties']['vehicles']['maxItems'],
                'drives': resources['drives']['properties']['items']['maxItems'], 'positions': resources['positions']['properties']['items']['maxItems'],
                'charges': resources['charges']['properties']['items']['maxItems'], 'charge_samples': resources['charge_samples']['properties']['items']['maxItems'],
                'capabilities': profile.document('discovery.schema.json')['properties']['capabilities']['maxItems'],
                'attributions': profile.document('discovery.schema.json')['properties']['attributions']['maxItems']}
    if observed != bounds:
        raise CheckFailure('array bounds differ from the profile limits')
    for route in profile.profile['routes']:
        if 'limit' in route and bounds[{'drive_positions': 'positions'}.get(route['kind'], route['kind'])] != route['limit']['max']:
            raise CheckFailure('page bound differs from the route limit')


def _check_capabilities(profile):
    advertised = profile.profile['capabilities']
    if capability_problems(advertised):
        raise CheckFailure('profile capabilities violate the token grammar')
    rules = profile.profile['capability_advertisement']
    groups = rules['always'] + rules['with_cursor_key'] + rules['with_paired_device_authentication'] + rules['with_enrichment']
    if sorted(groups) != sorted(advertised):
        raise CheckFailure('capability advertisement rules differ from the capability list')
    for route in profile.routes.values():
        if route.get('capability') not in (None, *advertised):
            raise CheckFailure('route names an unknown capability')
    for name in ('discovery', 'discovery-unsigned'):
        example = read_json(profile.root / 'examples' / (name + '.json'))
        problems = check_discovery(example, profile_id=PROFILE_ID, tokens=advertised)
        if problems:
            raise CheckFailure('discovery example ' + name + ' violates the capability rules')
    if 'manifestPublicKey' in read_json(profile.root / 'examples/discovery-unsigned.json'):
        raise CheckFailure('the unsigned discovery example carries a signing key')


def _check_cases(profile):
    cases = profile.document('cases.json')
    if cases.get('profile_id') != PROFILE_ID:
        raise CheckFailure('cases name another profile')
    identifiers = [case['id'] for case in cases['cases']]
    if len(set(identifiers)) != len(identifiers):
        raise CheckFailure('case identifiers are not unique')
    if not REQUIRED_CASE_IDS <= set(identifiers):
        raise CheckFailure('required cases are missing')
    if set(INPUT_KINDS) != set(cases['inputs']):
        raise CheckFailure('descriptor input kinds differ from the case input catalogue')
    for case in cases['cases']:
        kinds = [case['kind'], *case.get('kinds', []), *[step['kind'] for step in case.get('steps', [])]]
        if any(kind not in profile.routes for kind in kinds):
            raise CheckFailure('case ' + case['id'] + ' references an undeclared route kind')
        if any(name not in cases['inputs'] for name in [*case['inputs'], *case.get('optional_inputs', [])]):
            raise CheckFailure('case ' + case['id'] + ' names an unknown input')
        route = profile.routes[case['kind']]
        if 'status' in case and str(case['status']) not in route['responses']:
            raise CheckFailure('case ' + case['id'] + ' expects a status outside its route')
        if case['method'] != route['method']:
            raise CheckFailure('case ' + case['id'] + ' uses the wrong method')


def _check_cursors(profile):
    tqp, tqd = profile.validator('cursor.schema.json', 'tqp1'), profile.validator('cursor.schema.json', 'tqd1')
    legacy = 'tqd1.' + '0' * 98 + '.' + '0' * 64
    if schema_problems(tqp, legacy) == [] or schema_problems(tqd, 'tqp1.' + '0' * 116 + '.' + '0' * 64) == []:
        raise CheckFailure('cursor grammars do not separate tqp1 from tqd1')
    if not tqp.is_valid('tqp1.' + '0' * 116 + '.' + '0' * 64) or not tqd.is_valid(legacy):
        raise CheckFailure('cursor grammars reject their own form')
    for name, kind, owner in (('drives', 'drives', 'vehicle_id'), ('positions', 'drive_positions', 'drive_id'),
                              ('charges', 'charges', 'vehicle_id'), ('charge-samples', 'charge_samples', 'charge_id')):
        page = read_json(profile.root / 'examples' / (name + '.json'))
        if page['next_cursor'] is not None:
            if not tqp.is_valid(page['next_cursor']):
                raise CheckFailure('example ' + name + ' cursor violates tqp1')
            vehicle = page['items'][0]['vehicle_id'] if owner == 'vehicle_id' else None
            scope = page['items'][0][owner] if owner != 'vehicle_id' else None
            if vehicle is None:
                vehicle = read_json(profile.root / 'examples/drive.json')['vehicle_id']
            problems = check_page(profile, kind, page, vehicle_id=vehicle, scope_id=scope)
            if problems:
                raise CheckFailure('example ' + name + ' cursor: ' + problems[0])


def _check_rotation_and_errors(profile):
    sentence = 'A retry of rotate with the prior bearer during the grace window returns the same successor; it never issues a different token.'
    rotation = profile.profile['pairing']['rotation']
    if rotation['lost_response_retry'] != sentence or sentence not in rotation['old_bearer_validity'] or rotation['after_grace'] != {'status': 401}:
        raise CheckFailure('rotation semantics differ from the contract')
    example = read_json(profile.root / 'examples/rotation-grace.json')
    if example['lost_response_retry'] != {'request': 'POST /v1/device/rotate', 'status': 200, 'returns': 'same_successor'}:
        raise CheckFailure('rotation-grace example does not return the same successor')
    if example['old_bearer_valid_until_ms'] != example['rotation_at_ms'] + example['grace_duration_ms']:
        raise CheckFailure('rotation-grace example window is inconsistent')
    codes = profile.profile['error_codes']
    used = {code for route in profile.routes.values() for listed in route.get('error_codes', {}).values() for code in listed}
    if not used <= set(codes) or any(re.fullmatch(r'[a-z_]{1,64}', code) is None for code in codes):
        raise CheckFailure('route error codes are not declared')
    for name in ('error-drive-not-found', 'error-charge-not-found'):
        if read_json(profile.root / 'examples' / (name + '.json'))['error']['code'] not in codes:
            raise CheckFailure('error example uses an undeclared code')
    if profile.profile['previous_profile'] != 'hub-http-v1@1.1.0' or profile.profile['status'] != 'served':
        raise CheckFailure('profile identity fields are wrong')


def _check_semantics(profile):
    semantics, resources = profile.document('field-semantics.json'), profile.document('resources.schema.json')['$defs']
    for name in ('current', 'drive', 'position', 'charge', 'charge_sample'):
        if set(resources[name]['properties']) != set(semantics[name]):
            raise CheckFailure('field semantics differ from the ' + name + ' schema')


OFFLINE_CHECKS = (
    ('bundle', _check_bundle),
    ('schemas', _check_schema_documents),
    ('openapi', _check_openapi),
    ('examples', lambda profile: _check_examples(profile, False)),
    ('examples-server-strict', lambda profile: _check_examples(profile, True)),
    ('max-items', _check_max_items),
    ('capabilities', _check_capabilities),
    ('cases', _check_cases),
    ('cursors', _check_cursors),
    ('rotation-and-errors', _check_rotation_and_errors),
    ('field-semantics', _check_semantics),
)


def check_offline(root=DEFAULT_PROFILE):
    """Run every offline profile check; one result per check, never raising for a bad profile."""
    results = []
    try:
        profile = Profile(root)
    except (ContractError, OSError, KeyError, TypeError):
        return [{'scope': 'offline', 'case_id': 'offline/bundle', 'status': 'failed', 'detail': 'profile is missing, incomplete or altered'}]
    for name, check in OFFLINE_CHECKS:
        result = {'scope': 'offline', 'case_id': 'offline/' + name}
        try:
            check(profile)
        except NotRun as skip:
            result.update(status='not_run', missing=skip.missing)
        except CheckFailure as failure:
            result.update(status='failed', detail=str(failure))
        except (ContractError, OSError, KeyError, TypeError, ValueError, IndexError):
            result.update(status='failed', detail='check could not read the profile')
        except Exception:  # a broken schema can raise library errors; report a fixed label, never the error text
            result.update(status='failed', detail='check raised an unexpected error')
        else:
            result['status'] = 'passed'
        results.append(result)
    return results


# ---------------------------------------------------------------------------
# Native checks against an owned loopback Hub
# ---------------------------------------------------------------------------
def validate_descriptor(value, profile):
    """Normalise a descriptor; labels name keys, never values."""
    if not isinstance(value, dict) or set(value) - {'schema_version', 'kind', 'profile_id', 'endpoint', 'certificate_path', 'tls_pin', 'inputs'}:
        raise hub_http.AcceptanceError('descriptor shape is invalid')
    if value.get('schema_version') != 1 or value.get('kind') != 'hub-http-v1-actual-hub' or value.get('profile_id') != PROFILE_ID:
        raise hub_http.AcceptanceError('descriptor profile binding is invalid')
    endpoint = hub_http.canonical_origin(value.get('endpoint'))
    certificate = value.get('certificate_path')
    if not isinstance(certificate, str) or not Path(certificate).is_absolute() or '\x00' in certificate or not Path(certificate).is_file():
        raise hub_http.AcceptanceError('descriptor certificate_path is invalid')
    pin = value.get('tls_pin')
    if pin is not None and (not isinstance(pin, str) or re.fullmatch(r'[0-9a-f]{64}', pin) is None):
        raise hub_http.AcceptanceError('descriptor tls_pin is invalid')
    inputs = value.get('inputs')
    catalogue = profile.document('cases.json')['inputs']
    if not isinstance(inputs, dict) or set(inputs) - set(catalogue):
        raise hub_http.AcceptanceError('descriptor inputs are invalid')
    for name, item in inputs.items():
        kind = INPUT_KINDS[name]
        if kind == 'bearer':
            valid = isinstance(item, str) and re.fullmatch(r'[0-9a-f]{64}', item) is not None
        elif kind == 'uuid':
            valid = isinstance(item, str) and re.fullmatch(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', item) is not None
        elif kind == 'id':
            valid = type(item) is int and 1 <= item <= I64_MAX
        elif kind == 'digest':
            valid = isinstance(item, str) and re.fullmatch(r'[0-9a-f]{64}', item) is not None
        else:
            valid = isinstance(item, str) and Path(item).is_absolute() and '\x00' not in item
        if not valid:
            raise hub_http.AcceptanceError('descriptor input ' + name + ' is invalid')
    bearers = [inputs[name] for name in ('bearer', 'second_bearer', *DESTRUCTIVE_BEARERS, 'expired_grace_bearer') if name in inputs]
    if len(set(bearers)) != len(bearers):
        raise hub_http.AcceptanceError('descriptor bearers must be distinct')
    return {'endpoint': endpoint, 'certificate_path': certificate, 'tls_pin': pin, 'inputs': dict(inputs)}


def load_descriptor(path, profile):
    try:
        return validate_descriptor(hub_http.read_private(path), profile)
    except OSError:
        raise hub_http.AcceptanceError('descriptor is unreadable') from None
    except (ValueError, UnicodeError):
        raise hub_http.AcceptanceError('descriptor is not strict JSON') from None


class Response:
    def __init__(self, status, headers, body):
        self.status, self.headers, self.body = status, headers, body


class NativeClient:
    """One bounded HTTPS exchange per call over the repository's deadline transport.

    An exchange ends at `total_timeout` seconds after it starts or at `run_deadline` (on the same clock), whichever
    comes first, so one slow exchange cannot outlast the budget of the whole run. An exchange that fails once the run
    deadline has passed was ended by the budget of the run, not by the Hub: it raises NotRun('time budget'), as every other
    place the budget ends a case does. An exchange that reaches its own `total_timeout` with budget to spare is a failure.
    """

    def __init__(self, endpoint, certificate_path, tls_pin=None, *, total_timeout=5.0, clock=time.monotonic, run_deadline=None):
        parsed = urllib.parse.urlsplit(hub_http.canonical_origin(endpoint))
        self.host, self.port = parsed.hostname, parsed.port or 443
        self.context = ssl.create_default_context(cafile=certificate_path)
        self.tls_pin, self.total_timeout, self.clock, self.run_deadline = tls_pin, total_timeout, clock, run_deadline
        self.requests = 0

    def exchange(self, method, path, *, bearer=None, body=None, headers=None):
        request_headers = dict(headers or {})
        if bearer is not None:
            request_headers['Authorization'] = 'Bearer ' + bearer
        deadline = self.clock() + self.total_timeout
        if self.run_deadline is not None:
            deadline = min(deadline, self.run_deadline)
        connection = response = None
        try:
            connection = hub_http._DeadlineHTTPSConnection(self.host, self.port, context=self.context, deadline=deadline, clock=self.clock)
            connection.connect()
            leaf = connection.sock.getpeercert(binary_form=True)
            if self.tls_pin is not None and (not leaf or hashlib.sha256(leaf).hexdigest() != self.tls_pin):
                raise hub_http.AcceptanceError('connected TLS leaf identity mismatch')
            connection.sock.settimeout(hub_http._remaining(deadline, self.clock))
            self.requests += 1
            connection.request(method, path, body=body, headers=request_headers)
            response = http.client.HTTPResponse(hub_http._DeadlineResponseSocket(connection.sock, deadline, self.clock), method=method)
            response.begin()
            raw = response.read(MAX_BYTES + 1)
            result = Response(response.status, {key.lower(): value for key, value in response.getheaders()}, raw)
            hub_http._remaining(deadline, self.clock)
            return result
        except hub_http.AcceptanceError:
            raise
        except ssl.SSLError:
            raise hub_http.AcceptanceError('TLS verification or exchange failed') from None
        except (OSError, TimeoutError):
            if self.run_deadline is not None and self.clock() >= self.run_deadline:
                raise NotRun('time budget') from None
            raise hub_http.AcceptanceError('bounded HTTPS exchange failed') from None
        except (http.client.HTTPException, ValueError):
            raise hub_http.AcceptanceError('HTTP response framing is invalid') from None
        finally:
            if response is not None:
                response.close()
            if connection is not None:
                connection.close()


# Execution order keeps flows contiguous; results are reported in cases.json order.
RECEIPT_WAIT_SECONDS = 30  # how long later-current waits for the harness receipt
MIN_EXCHANGE_SECONDS = 0.25  # an exchange does not start with less of the run budget than this
EXECUTION_ORDER = (
    'discovery-success', 'discovery-profile', 'health-success', 'ready-success', 'missing-bearer', 'bad-bearer',
    'vehicles-success', 'current-success', 'current-empty', 'current-unknown',
    'drives-success', 'scenario-page-2', 'scenario-page-3', 'invalid_limit', 'invalid_time_range', 'invalid_query', 'invalid_cursor',
    'drive-conditional', 'cursor-time-binding',
    'drive-success', 'drive-unknown', 'positions-success', 'positions-empty', 'positions-keyset-three-pages',
    'charges-success', 'charges-empty', 'charge-success', 'charge-unknown', 'malformed-identifier-404', 'charge-samples-success',
    'page-conditional', 'cursor-kind-binding', 'cursor-vehicle-binding', 'cursor-scope-binding', 'cursor-window-binding', 'page-byte-cap',
    'later-current',
    'ungranted-vehicle-404', 'ungranted-vehicles-list', 'ungranted-pack-404',
    'claim-success', 'pairing-replay',
    'rotate-success', 'rotated-bearer-grace-valid', 'rotation-lost-response-retry', 'rotation-grace-same-successor', 'rotated-bearer-valid',
    'rotation-after-grace-401',
    'revoke-then-401', 'revoke-again-401')
PREREQUISITES = {
    'rotated-bearer-grace-valid': 'rotate-success', 'rotation-lost-response-retry': 'rotate-success',
    'rotation-grace-same-successor': 'rotate-success', 'rotated-bearer-valid': 'rotate-success', 'pairing-replay': 'claim-success'}
PAGE_KINDS = ('drives', 'drive_positions', 'charges', 'charge_samples')
# device_name values the claim route must refuse with 422: not 1 to 128 UTF-8 bytes (empty; 129 bytes; 65 two-byte characters are 65 characters but 130 bytes)
# or holding a control character (a newline, DEL, and U+0085, a C1 control). A refusal consumes nothing, so the claim that follows must still succeed.
REFUSED_DEVICE_NAMES = (('empty', ''), ('129 bytes', 'x' * 129), ('65 two-byte characters', '\u00e9' * 65), ('newline', 'a\nb'), ('DEL', 'a\x7fb'), ('C1 control character', 'a\u0085b'))
# %FF percent-decodes to bytes that are not UTF-8: a malformed identifier like any other, never 400
UNKNOWN_IDENTIFIERS = ('abc', '0', '-1', '9223372036854775808', '%FF')
MALFORMED_SEGMENTS = ('abc', '%FF')  # a vehicle id or pack name that is not a UUID or a digest, and one that is not UTF-8
NIL_VEHICLE = '00000000-0000-0000-0000-000000000000'  # well-formed, and no vehicle can have it: an unknown vehicle
# not an integer of the parameter's type: limit is unsigned 32-bit, from_ms and to_ms signed 64-bit
MALFORMED_VALUES = (('limit', 'abc'), ('limit', '-1'), ('from_ms', '1.5'), ('to_ms', '9223372036854775808'))


class NativeRun:
    def __init__(self, profile, descriptor, *, timeout=None, clock=time.monotonic, client=None):
        self.profile, self.inputs, self.clock = profile, descriptor['inputs'], clock
        self.deadline = None if timeout is None else clock() + timeout
        self.client = client or NativeClient(descriptor['endpoint'], descriptor['certificate_path'], descriptor['tls_pin'], clock=clock, run_deadline=self.deadline)
        self.descriptor = descriptor
        self.cases = profile.document('cases.json')['cases']
        self.by_id = {case['id']: case for case in self.cases}
        self.results, self.advertised, self.revoked, self.rotation = {}, None, False, {}
        self.cache = {}

    # -- plumbing ---------------------------------------------------------
    def supports(self, kind):
        capability = self.profile.routes[kind].get('capability')
        return capability is None or self.advertised is None or capability in self.advertised

    def unavailable(self, case):
        """Capabilities the routes a case declares (its kind, `kinds` and `steps`) need but discovery did not advertise.

        A case never runs on only some of the routes it declares: one that cannot run all of them is `not_run`, and it
        is decided before anything is sent, so a destructive case revokes or rotates nothing.
        """
        declared = [case['kind'], *case.get('kinds', []), *[step['kind'] for step in case.get('steps', [])]]
        missing = []
        for kind in declared:
            label = 'capability ' + str(self.profile.routes[kind].get('capability'))
            if not self.supports(kind) and label not in missing:
                missing.append(label)
        return missing

    def need(self, *names):
        missing = [name for name in names if name not in self.inputs]
        if missing:
            raise NotRun(*missing)

    def ensure_budget(self):
        """An exchange starts only with at least MIN_EXCHANGE_SECONDS of the run budget left."""
        if self.deadline is not None and self.deadline - self.clock() < MIN_EXCHANGE_SECONDS:
            raise NotRun('time budget')

    def call(self, step, kind, method, path, *, status=None, bearer=None, body=None, headers=None, client=None):
        self.ensure_budget()
        response = (client or self.client).exchange(method, path, bearer=bearer, body=body, headers=headers)
        if status is not None and response.status != status:
            raise CheckFailure(step + ': expected HTTP ' + str(status) + ', got ' + str(response.status))
        problems = validate_raw(self.profile, kind, response.status, response.headers, response.body)
        if problems:
            raise CheckFailure(step + ': ' + problems[0])
        value = None
        if response.body and _json_content_type(response.headers):
            value = strict_json(response.body)
        return response, value

    def error_code(self, step, kind, path, code, *, bearer, status=400, headers=None):
        _, value = self.call(step, kind, 'GET', path, status=status, bearer=bearer, headers=headers)
        if value is None or value['error']['code'] != code:
            raise CheckFailure(step + ': wrong error code')
        return value

    def path(self, kind, *, vehicle=None, drive=None, charge=None, query=None):
        vehicle = vehicle or self.inputs['vehicle_id']
        base = {'drives': '/v1/vehicles/%s/drives', 'charges': '/v1/vehicles/%s/charges'}
        if kind in base:
            text = base[kind] % vehicle
        elif kind == 'drive':
            text = '/v1/vehicles/%s/drives/%s' % (vehicle, drive)
        elif kind == 'drive_positions':
            text = '/v1/vehicles/%s/drives/%s/positions' % (vehicle, drive if drive is not None else self.inputs['drive_id'])
        elif kind == 'charge':
            text = '/v1/vehicles/%s/charges/%s' % (vehicle, charge)
        elif kind == 'charge_samples':
            text = '/v1/vehicles/%s/charges/%s/samples' % (vehicle, charge if charge is not None else self.inputs['charge_id'])
        else:
            raise KeyError(kind)
        return text + ('?' + urllib.parse.urlencode(query) if query else '')

    def scope(self, kind):
        return {'drive_positions': self.inputs.get('drive_id'), 'charge_samples': self.inputs.get('charge_id')}.get(kind)

    def page(self, step, kind, *, bearer=None, vehicle=None, drive=None, charge=None, status=200, headers=None, **query):
        bearer = bearer or self.inputs['bearer']
        drive = drive if drive is not None else self.inputs.get('drive_id')
        charge = charge if charge is not None else self.inputs.get('charge_id')
        response, value = self.call(step, kind, 'GET', self.path(kind, vehicle=vehicle, drive=drive, charge=charge, query=query), status=status, bearer=bearer, headers=headers)
        if status == 200:
            scope = {'drive_positions': drive, 'charge_samples': charge}.get(kind)
            problems = check_page(self.profile, kind, value, vehicle_id=vehicle or self.inputs['vehicle_id'], scope_id=scope,
                                  from_ms=query.get('from_ms', 0), to_ms=query.get('to_ms', I64_MAX),
                                  limit=query.get('limit', self.profile.routes[kind]['limit']['default']))
            if problems:
                raise CheckFailure(step + ': ' + problems[0])
        return response, value

    def walk(self, step, kind, count, *, limit, **options):
        """Follow next_cursor for up to `count` pages; checks every page and the continuity between them."""
        pages, query = [], {'limit': limit}
        for index in range(count):
            _, value = self.page(step + ' page ' + str(index + 1), kind, **query, **options)
            pages.append(value)
            if value['next_cursor'] is None:
                break
            query = {'limit': limit, 'cursor': value['next_cursor']}
        problems = check_sequence(self.profile, kind, pages)
        if problems:
            raise CheckFailure(step + ': ' + problems[0])
        return pages

    def first_cursor(self, kind, *, vehicle=None, drive=None, charge=None, **query):
        _, value = self.page(kind + ' cursor source', kind, vehicle=vehicle, drive=drive, charge=charge, limit=1, **query)
        return value['next_cursor']

    def require_data(self, label, condition):
        if not condition:
            raise NotRun('data: ' + label)

    def probe(self, path):
        """Status of one read with `bearer`, for a control that must precede a comparison; it never validates the body."""
        self.ensure_budget()
        return self.client.exchange('GET', path, bearer=self.inputs['bearer']).status

    def window_reads(self, step, kind, items):
        """from_ms is inclusive and to_ms exclusive on the window key: re-read the page with each bound at a key it holds."""
        key = self.profile.routes[kind]['window_key']
        keys = sorted({item[key] for item in items})
        _, upper = self.page(step + ' from_ms', kind, from_ms=keys[-1])
        if keys[-1] not in {item[key] for item in upper['items']}:
            raise CheckFailure(step + ': from_ms is not inclusive')
        if keys[0] > 0:  # to_ms 0 would be an empty window; check_page rejects any item at or beyond to_ms
            self.page(step + ' to_ms', kind, to_ms=keys[0])

    # -- public cases -----------------------------------------------------
    def case_discovery_success(self):
        _, value = self.call('discovery', 'discovery', 'GET', '/.well-known/teslatlas-hub', status=200)
        self.advertised = set(value['capabilities'])
        self.cache['discovery'] = value

    def case_discovery_profile(self):
        value = self.cache.get('discovery')
        if value is None:
            _, value = self.call('discovery', 'discovery', 'GET', '/.well-known/teslatlas-hub', status=200)
        problems = check_discovery(value, profile_id=PROFILE_ID, tokens=self.profile.profile['capabilities'])
        if problems:
            raise CheckFailure('discovery: ' + problems[0])

    def case_health_success(self):
        self.call('health', 'health', 'GET', '/healthz', status=200)

    def case_ready_success(self):
        self.call('ready', 'ready', 'GET', '/readyz', status=200)

    def case_missing_bearer(self):
        self.call('vehicles without bearer', 'vehicles', 'GET', '/v1/vehicles', status=401)

    def case_bad_bearer(self):
        self.call('vehicles with a bad bearer', 'vehicles', 'GET', '/v1/vehicles', status=401, bearer='synthetic-invalid-token')

    # -- reads with one paired bearer --------------------------------------
    def case_vehicles_success(self):
        _, value = self.call('vehicles', 'vehicles', 'GET', '/v1/vehicles', status=200, bearer=self.inputs['bearer'])
        if self.inputs['vehicle_id'] not in [item['vehicle_id'] for item in value['vehicles']]:
            raise CheckFailure('vehicles: the granted vehicle is not listed')

    def case_current_success(self):
        _, value = self.call('current', 'current', 'GET', '/v1/vehicles/' + self.inputs['vehicle_id'] + '/current', status=200, bearer=self.inputs['bearer'])
        if value['vehicle_id'] != self.inputs['vehicle_id']:
            raise CheckFailure('current: vehicle identity mismatch')

    def case_current_empty(self):
        _, value = self.call('empty current', 'current', 'GET', '/v1/vehicles/' + self.inputs['empty_vehicle_id'] + '/current', status=200, bearer=self.inputs['bearer'])
        if value['observed_at_ms'] is not None:
            raise CheckFailure('empty current: observed_at_ms is not null')

    def case_current_unknown(self):
        self.call('unknown current', 'current', 'GET', '/v1/vehicles/' + str(uuid.uuid4()) + '/current', status=404, bearer=self.inputs['bearer'])

    def drives_walk(self):
        if 'drives_walk' not in self.cache:
            pages = self.walk('drives', 'drives', 3, limit=2)
            _, unpaged = self.page('drives unpaged', 'drives', limit=6)  # the same items read in one page: a walk must not skip or reorder
            walked = [item['id'] for page in pages for item in page['items']]
            if walked != [item['id'] for item in unpaged['items']][:len(walked)]:
                raise CheckFailure('drives: pages two at a time differ from the unpaged read')
            self.cache['drives_walk'] = pages
        return self.cache['drives_walk']

    def case_drives_success(self):
        _, value = self.page('drives', 'drives')
        self.require_data('drives for vehicle_id', value['items'])
        self.window_reads('drives', 'drives', value['items'])
        self.empty_window('drives in an empty window', 'drives')

    def case_scenario_page_2(self):
        self.require_data('at least three drives for vehicle_id', len(self.drives_walk()) >= 2)

    def case_scenario_page_3(self):
        self.require_data('at least five drives for vehicle_id', len(self.drives_walk()) >= 3)

    def page_targets(self):
        """(kind, path options) for every page route whose identifiers the descriptor supplies; drives first."""
        targets = [('drives', {}), ('charges', {})]
        if 'drive_id' in self.inputs:
            targets.append(('drive_positions', {'drive': self.inputs['drive_id']}))
        if 'charge_id' in self.inputs:
            targets.append(('charge_samples', {'charge': self.inputs['charge_id']}))
        return targets

    def case_invalid_limit(self):
        for kind, options in self.page_targets():
            for value in (0, self.profile.routes[kind]['limit']['max'] + 1):
                self.error_code(kind + ' limit ' + str(value), kind, self.path(kind, query={'limit': value}, **options), 'invalid_limit', bearer=self.inputs['bearer'])

    def case_invalid_time_range(self):
        for kind, options in self.page_targets():
            for query in ({'from_ms': 20, 'to_ms': 10}, {'from_ms': -1}):
                self.error_code(kind + ' time range', kind, self.path(kind, query=query, **options), 'invalid_time_range', bearer=self.inputs['bearer'])

    def case_invalid_query(self):
        for kind, options in self.page_targets():
            self.error_code(kind + ' unknown parameter', kind, self.path(kind, query={'from': '2026-09-01'}, **options), 'invalid_query', bearer=self.inputs['bearer'])
            # a value that is not an integer of the parameter's type is invalid_query, never invalid_limit or invalid_time_range
            for name, value in MALFORMED_VALUES:
                self.error_code(kind + ' ' + name + '=' + value, kind, self.path(kind, query={name: value}, **options), 'invalid_query', bearer=self.inputs['bearer'])

    def case_invalid_cursor(self):
        bearer = self.inputs['bearer']
        cursor = self.first_cursor('drives')
        self.require_data('at least two drives for vehicle_id', cursor)
        self.error_code('cursor of another vehicle', 'drives', self.path('drives', vehicle=self.inputs['other_vehicle_id'], query={'limit': 1, 'cursor': cursor}), 'invalid_cursor', bearer=bearer)
        for kind, options in self.page_targets():
            self.error_code(kind + ' malformed cursor', kind, self.path(kind, query={'cursor': 'not-a-cursor'}, **options), 'invalid_cursor', bearer=bearer)
            own = cursor if kind == 'drives' else self.first_cursor(kind, **options)
            if own:  # a tampered tag needs a route with at least two items
                tampered = own[:-1] + ('0' if own[-1] != '0' else '1')
                self.error_code(kind + ' tampered cursor tag', kind, self.path(kind, query={'limit': 1, 'cursor': tampered}, **options), 'invalid_cursor', bearer=bearer)

    def case_drive_conditional(self):
        response, _ = self.page('drives', 'drives', limit=2)
        etag = response.headers['etag']
        not_modified, _ = self.call('conditional drives', 'drives', 'GET', self.path('drives', query={'limit': 2}), status=304, bearer=self.inputs['bearer'], headers={'If-None-Match': etag})
        if not_modified.headers.get('etag') != etag:
            raise CheckFailure('conditional drives: the 304 does not carry the validator ETag')
        self.call('non-matching validator', 'drives', 'GET', self.path('drives', query={'limit': 2}), status=200, bearer=self.inputs['bearer'], headers={'If-None-Match': '"' + '0' * 64 + '"'})

    def case_cursor_time_binding(self):
        cursor = self.first_cursor('drives')
        self.require_data('at least two drives for vehicle_id', cursor)
        for query in ({'from_ms': 1}, {'to_ms': I64_MAX - 1}):
            self.error_code('cursor window', 'drives', self.path('drives', query={'limit': 1, 'cursor': cursor, **query}), 'invalid_cursor', bearer=self.inputs['bearer'])

    # -- items and nested pages ---------------------------------------------
    def item_case(self, step, kind, list_kind, identifier, pick):
        _, value = self.call(step, kind, 'GET', self.path(kind, **{pick: identifier}), status=200, bearer=self.inputs['bearer'])
        if value['id'] != identifier or value.get('vehicle_id', self.inputs['vehicle_id']) != self.inputs['vehicle_id']:
            raise CheckFailure(step + ': identity mismatch')
        _, listing = self.page(step + ' listing', list_kind, limit=self.profile.routes[list_kind]['limit']['max'])
        match = [item for item in listing['items'] if item['id'] == identifier]
        if match and match[0] != value:
            raise CheckFailure(step + ': item differs from its listing entry')

    def unknown_case(self, step, kind, nested_kind, code, pick):
        shapes = []
        for identifier in ('9223372036854775807', *UNKNOWN_IDENTIFIERS):
            response, value = self.call(step + ' ' + identifier, kind, 'GET', self.path(kind, **{pick: identifier}), status=404, bearer=self.inputs['bearer'])
            if value['error']['code'] != code:
                raise CheckFailure(step + ': wrong not-found code')
            shapes.append((response.status, value['error']['code'], response.headers.get('content-type'), response.headers.get('cache-control')))
        if len(set(shapes)) != 1:
            raise CheckFailure(step + ': invalid identifiers return different shapes')
        for identifier in ('9223372036854775807', 'abc', '%FF'):
            self.error_code(step + ' nested', nested_kind, self.path(nested_kind, **{pick: identifier}), code, bearer=self.inputs['bearer'], status=404)

    def case_drive_success(self):
        self.item_case('drive', 'drive', 'drives', self.inputs['drive_id'], 'drive')

    def case_drive_unknown(self):
        self.unknown_case('drive', 'drive', 'drive_positions', 'drive_not_found', 'drive')

    def case_charge_success(self):
        self.item_case('charge', 'charge', 'charges', self.inputs['charge_id'], 'charge')

    def case_charge_unknown(self):
        self.unknown_case('charge', 'charge', 'charge_samples', 'charge_not_found', 'charge')

    def case_malformed_identifier_404(self):
        bearer = self.inputs['bearer']
        routes = (('current', lambda vehicle: '/v1/vehicles/' + vehicle + '/current'),
                  ('drives', lambda vehicle: self.path('drives', vehicle=vehicle)),
                  ('drive', lambda vehicle: self.path('drive', vehicle=vehicle, drive=1)),
                  ('drive_positions', lambda vehicle: self.path('drive_positions', vehicle=vehicle, drive=1)),
                  ('charges', lambda vehicle: self.path('charges', vehicle=vehicle)),
                  ('charge', lambda vehicle: self.path('charge', vehicle=vehicle, charge=1)),
                  ('charge_samples', lambda vehicle: self.path('charge_samples', vehicle=vehicle, charge=1)),
                  ('sync_manifest', lambda vehicle: '/v1/vehicles/' + vehicle + '/sync/manifest'))
        for identifier in (*MALFORMED_SEGMENTS, NIL_VEHICLE):
            for kind, path_for in routes:
                step = 'vehicle ' + identifier + ' on ' + kind
                # validate_raw holds /current to an empty 404 and the JSON routes to their JSON envelope; the manifest allows either, so it is held here
                response, value = self.call(step, kind, 'GET', path_for(identifier), status=404, bearer=bearer)
                if kind == 'sync_manifest' and identifier in MALFORMED_SEGMENTS:
                    if response.body:
                        raise CheckFailure(step + ': expected an empty 404')
                elif kind != 'current' and (value is None or value['error']['code'] != 'vehicle_not_found'):
                    raise CheckFailure(step + ': wrong not-found code')
        for identifier in MALFORMED_SEGMENTS:
            self.call('pack ' + identifier, 'pack', 'GET', '/v1/packs/sha256/' + identifier, status=404, bearer=bearer)
        # the bearer is checked before the path segments are interpreted
        for kind, path_for in routes:
            self.call('without a bearer on ' + kind, kind, 'GET', path_for('%FF'), status=401)
        self.call('without a bearer on pack', 'pack', 'GET', '/v1/packs/sha256/%FF', status=401)

    def success_and_continuity(self, kind, label, walk=True):
        _, value = self.page(label, kind)
        self.require_data(label + ' for the supplied identifiers', value['items'])
        self.window_reads(label, kind, value['items'])
        if walk and len(value['items']) >= 3:
            pages = self.walk(label + ' continuity', kind, 3, limit=1)
            if len(pages) != 3 or [item['id'] for page in pages for item in page['items'][:1]] != [item['id'] for item in value['items'][:3]]:
                raise CheckFailure(label + ': single-item pages differ from the unpaged read')
        return value

    def empty_window(self, step, kind):
        path = self.path(kind, drive=self.inputs.get('drive_id'), charge=self.inputs.get('charge_id'), query={'from_ms': I64_MAX - 1000})
        _, value = self.call(step, kind, 'GET', path, status=200, bearer=self.inputs['bearer'])
        if value != {'items': [], 'next_cursor': None}:
            raise CheckFailure(step + ': an empty window is not an empty page')

    def case_positions_success(self):
        self.success_and_continuity('drive_positions', 'positions', walk=False)

    def case_positions_empty(self):
        self.empty_window('positions in an empty window', 'drive_positions')

    def case_positions_keyset_three_pages(self):
        _, unpaged = self.page('positions', 'drive_positions')
        self.require_data('at least three positions for drive_id', len(unpaged['items']) >= 3)
        pages = self.walk('positions keyset', 'drive_positions', 3, limit=1)
        identifiers = [item['id'] for page in pages for item in page['items'][:1]]
        if len(pages) != 3 or len(set(identifiers)) != 3 or identifiers != [item['id'] for item in unpaged['items'][:3]]:
            raise CheckFailure('positions keyset: pages duplicate or miss an item')

    def case_charges_success(self):
        self.success_and_continuity('charges', 'charges')

    def case_charges_empty(self):
        self.empty_window('charges in an empty window', 'charges')

    def case_charge_samples_success(self):
        self.success_and_continuity('charge_samples', 'charge samples')
        self.empty_window('charge samples in an empty window', 'charge_samples')

    def case_page_conditional(self):
        for kind in PAGE_KINDS:
            first, value = self.page(kind + ' validator source', kind, limit=2)
            etag = first.headers['etag']
            path = self.path(kind, drive=self.inputs['drive_id'], charge=self.inputs['charge_id'], query={'limit': 2})
            not_modified, _ = self.call(kind + ' conditional', kind, 'GET', path, status=304, bearer=self.inputs['bearer'], headers={'If-None-Match': etag})
            if not_modified.headers.get('etag') != etag:
                raise CheckFailure(kind + ': the 304 does not carry the validator ETag')
            again, _ = self.call(kind + ' repeat', kind, 'GET', path, status=200, bearer=self.inputs['bearer'], headers={'If-None-Match': '"' + '0' * 64 + '"'})
            if again.headers['etag'] != etag or again.body != first.body:
                raise CheckFailure(kind + ': the same read changed its ETag')

    def cursors_for(self, kinds, **options):
        found = {}
        for kind in kinds:
            cursor = self.first_cursor(kind, **options.get(kind, {}))
            if cursor:
                found[kind] = cursor
        return found

    def case_cursor_kind_binding(self):
        found = self.cursors_for(('drives', 'charges'))
        self.require_data('at least two drives for vehicle_id', 'drives' in found)
        for source, target in (('drives', 'charges'), ('charges', 'drives')):
            if source in found:
                self.error_code(source + ' cursor on ' + target, target, self.path(target, query={'limit': 1, 'cursor': found[source]}), 'invalid_cursor', bearer=self.inputs['bearer'])

    def case_cursor_vehicle_binding(self):
        found = self.cursors_for(('drives', 'charges'))
        self.require_data('at least two drives for vehicle_id', 'drives' in found)
        for kind, cursor in found.items():
            self.error_code(kind + ' cursor on another vehicle', kind, self.path(kind, vehicle=self.inputs['other_vehicle_id'], query={'limit': 1, 'cursor': cursor}), 'invalid_cursor', bearer=self.inputs['bearer'])

    def case_cursor_scope_binding(self):
        cursor = self.first_cursor('drive_positions', drive=self.inputs['drive_id'])
        self.require_data('at least two positions for drive_id', cursor)
        self.error_code('positions cursor of another drive', 'drive_positions', self.path('drive_positions', drive=self.inputs['other_drive_id'], query={'limit': 1, 'cursor': cursor}), 'invalid_cursor', bearer=self.inputs['bearer'])
        if 'charge_id' in self.inputs and 'other_charge_id' in self.inputs:
            samples = self.first_cursor('charge_samples', charge=self.inputs['charge_id'])
            if samples:
                self.error_code('samples cursor of another charge', 'charge_samples', self.path('charge_samples', charge=self.inputs['other_charge_id'], query={'limit': 1, 'cursor': samples}), 'invalid_cursor', bearer=self.inputs['bearer'])

    def case_cursor_window_binding(self):
        kinds, options = ['drive_positions', 'charges'], {'drive_positions': {'drive': self.inputs['drive_id']}}
        if 'charge_id' in self.inputs:
            kinds.append('charge_samples')
            options['charge_samples'] = {'charge': self.inputs['charge_id']}
        found = self.cursors_for(kinds, **options)
        self.require_data('a page route with at least two items', found)
        for kind, cursor in found.items():
            for query in ({'from_ms': 1}, {'to_ms': I64_MAX - 1}):
                self.error_code(kind + ' cursor window', kind, self.path(kind, drive=self.inputs['drive_id'], query={'limit': 1, 'cursor': cursor, **query}), 'invalid_cursor', bearer=self.inputs['bearer'])

    def case_page_byte_cap(self):
        for kind in PAGE_KINDS:
            maximum = self.profile.routes[kind]['limit']['max']
            pages, query = [], {'limit': maximum}
            for index in range(3):
                # page() holds each page to the byte cap (validate_raw) and to the requested limit (check_page)
                _, value = self.page(kind + ' maximum limit page ' + str(index + 1), kind, **query)
                pages.append(value)
                if value['next_cursor'] is None:
                    break
                query = {'limit': maximum, 'cursor': value['next_cursor']}
            problems = check_sequence(self.profile, kind, pages)
            if problems:
                raise CheckFailure(kind + ': ' + problems[0])

    def case_later_current(self):
        self.need('update_request_path', 'update_receipt_path')
        path = '/v1/vehicles/' + self.inputs['vehicle_id'] + '/current'
        _, before = self.call('current before the update', 'current', 'GET', path, status=200, bearer=self.inputs['bearer'])
        try:
            descriptor = os.open(self.inputs['update_request_path'], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except OSError:
            raise CheckFailure('update request could not be created') from None
        os.close(descriptor)
        limit, receipt, partial = self.clock() + RECEIPT_WAIT_SECONDS, None, False
        while receipt is None:
            if Path(self.inputs['update_receipt_path']).exists():
                try:
                    receipt = hub_http.read_private(self.inputs['update_receipt_path'])
                    break
                except (OSError, hub_http.AcceptanceError):
                    raise CheckFailure('update receipt is unreadable') from None
                except ValueError:
                    partial = True  # a harness that writes in place makes the file exist before it has content: read again
            if self.clock() > limit:
                raise CheckFailure('update receipt is unreadable' if partial else 'the harness did not confirm the update')
            if self.deadline is not None and self.clock() >= self.deadline:
                raise NotRun('time budget')  # the run ran out of time; that says nothing about the harness
            time.sleep(0.1)
        if not isinstance(receipt, dict) or receipt.get('status') != 'passed':
            raise CheckFailure('the harness did not commit the update')
        _, after = self.call('current after the update', 'current', 'GET', path, status=200, bearer=self.inputs['bearer'])
        if type(after['observed_at_ms']) is not int or (before['observed_at_ms'] is not None and after['observed_at_ms'] <= before['observed_at_ms']):
            raise CheckFailure('current did not advance after the update')

    # -- grants -------------------------------------------------------------
    def same_shape(self, step, kind, path_for, ungranted, expect, *, headers=None):
        bearer = self.inputs['second_bearer']
        shown, hidden = [], str(uuid.uuid4())
        for vehicle in (ungranted, hidden):
            response, value = self.call(step, kind, 'GET', path_for(vehicle), status=404, bearer=bearer, headers=headers)
            if expect and (value is None or value['error']['code'] != expect):
                raise CheckFailure(step + ': wrong not-found code')
            shown.append((response.status, response.body, response.headers.get('content-type'), response.headers.get('cache-control')))
        if shown[0] != shown[1]:
            raise CheckFailure(step + ': an ungranted vehicle differs from an unknown vehicle')

    def case_ungranted_vehicle_404(self):
        ungranted = self.inputs['ungranted_vehicle_id']
        if self.probe('/v1/vehicles/' + ungranted + '/current') != 200:  # the comparison proves nothing for a vehicle that does not exist
            raise NotRun('data: ungranted_vehicle_id is not readable by bearer')
        self.same_shape('current of an ungranted vehicle', 'current', lambda vehicle: '/v1/vehicles/' + vehicle + '/current', ungranted, None)
        invalid = {'limit': 0}  # an invalid query on a page route must not turn the answer into 400
        routes = (('drives', self.path('drives', vehicle='%s', query=invalid)), ('drive', self.path('drive', vehicle='%s', drive=1)),
                  ('drive_positions', self.path('drive_positions', vehicle='%s', drive=1, query=invalid)),
                  ('charges', self.path('charges', vehicle='%s', query=invalid)), ('charge', self.path('charge', vehicle='%s', charge=1)),
                  ('charge_samples', self.path('charge_samples', vehicle='%s', charge=1, query=invalid)))
        for kind, template in routes:
            self.same_shape('ungranted ' + kind, kind, lambda vehicle, template=template: template % vehicle, ungranted, 'vehicle_not_found')
        # the manifest keeps the 404 an unknown vehicle gets (JSON vehicle_not_found, asserted by this profile); an ungranted vehicle gets the same
        self.same_shape('ungranted sync manifest', 'sync_manifest', lambda vehicle: '/v1/vehicles/' + vehicle + '/sync/manifest', ungranted, 'vehicle_not_found')

    def case_ungranted_vehicles_list(self):
        _, mine = self.call('vehicles of the first device', 'vehicles', 'GET', '/v1/vehicles', status=200, bearer=self.inputs['bearer'])
        if self.inputs['ungranted_vehicle_id'] not in [item['vehicle_id'] for item in mine['vehicles']]:
            raise NotRun('data: ungranted_vehicle_id is not readable by bearer')
        _, value = self.call('vehicles of the second device', 'vehicles', 'GET', '/v1/vehicles', status=200, bearer=self.inputs['second_bearer'])
        if self.inputs['ungranted_vehicle_id'] in [item['vehicle_id'] for item in value['vehicles']]:
            raise CheckFailure('vehicles: an ungranted vehicle is listed')

    @staticmethod
    def pack_path(digest):
        """The pack route takes an object name: the digest followed by .sqlite.zst; any other name is a 404 before any lookup."""
        return '/v1/packs/sha256/' + digest + '.sqlite.zst'

    def case_ungranted_pack_404(self):
        if self.probe(self.pack_path(self.inputs['ungranted_pack_digest'])) not in (200, 206):  # an unknown digest would pass the comparison below
            raise NotRun('data: ungranted_pack_digest is not downloadable by bearer')
        bearer, shown = self.inputs['second_bearer'], []
        for digest in (self.inputs['ungranted_pack_digest'], secrets.token_hex(32)):
            response, _ = self.call('pack', 'pack', 'GET', self.pack_path(digest), status=404, bearer=bearer)
            shown.append((response.status, response.body, response.headers.get('content-type'), response.headers.get('cache-control')))
        if shown[0] != shown[1]:
            raise CheckFailure('pack: an ungranted digest differs from an unknown digest')

    # -- pairing, rotation and revocation --------------------------------------
    def case_claim_success(self):
        try:
            invitation_raw = json.dumps(hub_http.read_private(self.inputs['invitation_path'])).encode()
        except (OSError, ValueError, hub_http.AcceptanceError):
            raise CheckFailure('invitation file is unreadable') from None
        if validate_raw(self.profile, 'invitation', 200, {}, invitation_raw):
            raise CheckFailure('invitation does not match its schema')
        invitation = strict_json(invitation_raw)
        if invitation['endpoint'].rstrip('/') != self.descriptor['endpoint'] or invitation['expiresAtMs'] <= int(time.time() * 1000):
            raise CheckFailure('invitation endpoint or expiry is invalid')
        client = NativeClient(self.descriptor['endpoint'], self.descriptor['certificate_path'], invitation['tlsPin'], clock=self.clock, run_deadline=self.deadline)
        body = json.dumps({'secret': invitation['secret'], 'device_name': 'Synthetic hub-http-v1@1.2.0 conformance'}, separators=(',', ':')).encode()
        if len(body) > self.profile.profile['max_claim_request_bytes']:
            raise CheckFailure('claim request exceeds the profile byte limit')
        self.cache['claim'] = (client, invitation['pairingId'], body)
        path, headers = '/v1/pairings/' + invitation['pairingId'] + '/claim', {'Content-Type': 'application/json'}
        for label, name in REFUSED_DEVICE_NAMES:
            refused = json.dumps({'secret': invitation['secret'], 'device_name': name}, separators=(',', ':')).encode()
            self.call('claim with a refused device_name (' + label + ')', 'claim', 'POST', path, status=422, body=refused, headers=headers, client=client)
        self.call('claim', 'claim', 'POST', path, status=200, body=body, headers=headers, client=client)

    def case_pairing_replay(self):
        client, pairing_id, body = self.cache['claim']
        self.call('claim replay', 'claim', 'POST', '/v1/pairings/' + pairing_id + '/claim', status=401, body=body,
                  headers={'Content-Type': 'application/json'}, client=client)

    def rotate(self, step, bearer, status=200):
        _, value = self.call(step, 'rotate', 'POST', '/v1/device/rotate', status=status, bearer=bearer)
        return value

    def case_rotate_success(self):
        first = self.inputs['rotation_bearer']
        value = self.rotate('rotate', first)
        if value.get('reason') == 'max_lifetime':
            if value['access_token'] != first:  # at the maximum a Hub issues nothing: the answer carries the bearer that was presented
                raise CheckFailure('rotate: a response at the maximum lifetime carries a new bearer')
            raise NotRun('rotation_bearer below its absolute maximum lifetime')
        if value['access_token'] == first:
            raise CheckFailure('rotate: the bearer was not replaced')
        self.rotation = {'device_id': value['device_id'], 'successor': value['access_token']}

    def case_rotated_bearer_grace_valid(self):
        self.call('prior bearer in the grace window', 'vehicles', 'GET', '/v1/vehicles', status=200, bearer=self.inputs['rotation_bearer'])

    def retry(self, step):
        value = self.rotate(step, self.inputs['rotation_bearer'])
        if value['access_token'] != self.rotation['successor'] or value['device_id'] != self.rotation['device_id'] or 'reason' in value:
            raise CheckFailure(step + ': the retry did not return the same successor')

    def case_rotation_lost_response_retry(self):
        self.retry('rotation retry')

    def case_rotation_grace_same_successor(self):
        for index in range(2):
            self.retry('rotation retry ' + str(index + 2))

    def case_rotated_bearer_valid(self):
        self.call('successor bearer', 'vehicles', 'GET', '/v1/vehicles', status=200, bearer=self.rotation['successor'])

    def case_rotation_after_grace_401(self):
        self.call('rotate after the grace window', 'rotate', 'POST', '/v1/device/rotate', status=401, bearer=self.inputs['expired_grace_bearer'])
        self.call('vehicles after the grace window', 'vehicles', 'GET', '/v1/vehicles', status=401, bearer=self.inputs['expired_grace_bearer'])

    def case_revoke_then_401(self):
        bearer, vehicle = self.inputs['disposable_bearer'], self.inputs['vehicle_id']
        self.call('disposable bearer before revoke', 'vehicles', 'GET', '/v1/vehicles', status=200, bearer=bearer)
        # any non-empty body is 400, including one over the 4096 byte request limit, where an extractor would answer 413
        for label, body in (('revoke with a body', b'{}'), ('revoke with a body over the request limit', b'x' * (self.profile.profile['limits']['ordinary_request_body_bytes'] + 1))):
            self.call(label, 'revoke', 'POST', '/v1/device/revoke', status=400, bearer=bearer, body=body, headers={'Content-Type': 'application/json'})
            self.call('bearer after a rejected revoke', 'vehicles', 'GET', '/v1/vehicles', status=200, bearer=bearer)
        self.call('revoke', 'revoke', 'POST', '/v1/device/revoke', status=204, bearer=bearer)
        self.revoked = True
        for step in self.by_id['revoke-then-401']['steps'][1:]:  # the declared steps after the revoke itself
            label = {'charges': 'a page', 'sync_manifest': 'sync manifest'}.get(step['kind'], step['kind'])
            self.call(label + ' after revoke', step['kind'], 'GET', self.step_path(step['kind'], vehicle), status=step['status'], bearer=bearer)

    def step_path(self, kind, vehicle):
        return {'vehicles': '/v1/vehicles', 'current': '/v1/vehicles/' + vehicle + '/current', 'charges': self.path('charges'),
                'sync_manifest': '/v1/vehicles/' + vehicle + '/sync/manifest', 'pack': self.pack_path('0' * 64)}[kind]

    def case_revoke_again_401(self):
        if not self.revoked:
            raise NotRun('prerequisite revoke-then-401')
        self.call('second revoke', 'revoke', 'POST', '/v1/device/revoke', status=401, bearer=self.inputs['disposable_bearer'])

    # -- driver ---------------------------------------------------------------
    def record(self, case_id, status, **fields):
        self.results[case_id] = {'scope': 'native', 'case_id': case_id, 'status': status, **fields}

    def execute(self, case_id):
        case = self.by_id[case_id]
        missing = [name for name in case['inputs'] if name not in self.inputs]
        if missing:
            return self.record(case_id, 'not_run', missing=missing)
        prerequisite = PREREQUISITES.get(case_id)
        if prerequisite is not None and self.results.get(prerequisite, {}).get('status') != 'passed':
            return self.record(case_id, 'not_run', missing=['prerequisite ' + prerequisite])
        unavailable = self.unavailable(case)
        if unavailable:
            return self.record(case_id, 'not_run', missing=unavailable)
        try:
            getattr(self, 'case_' + case_id.replace('-', '_'))()
        except NotRun as skip:
            self.record(case_id, 'not_run', missing=skip.missing)
        except CheckFailure as failure:
            self.record(case_id, 'failed', detail=str(failure))
        except hub_http.AcceptanceError as failure:
            self.record(case_id, 'failed', detail=str(failure))
        except (ContractError, KeyError, TypeError, ValueError, IndexError):
            self.record(case_id, 'failed', detail='response could not be interpreted')
        except Exception:
            self.record(case_id, 'failed', detail='check raised an unexpected error')
        else:
            self.record(case_id, 'passed')

    def run(self):
        for case_id in EXECUTION_ORDER:
            self.execute(case_id)
        return [self.results[case['id']] for case in self.cases]


def unrun_native(profile, missing):
    """Every native case as not_run, for a run without a usable descriptor."""
    return [{'scope': 'native', 'case_id': case['id'], 'status': 'not_run', 'missing': list(missing)} for case in profile.document('cases.json')['cases']]


def run_native(descriptor, profile=None, *, timeout=None, clock=time.monotonic, client=None):
    profile = profile or Profile()
    return NativeRun(profile, descriptor, timeout=timeout, clock=clock, client=client).run()


# ---------------------------------------------------------------------------
# Summary and command line
# ---------------------------------------------------------------------------
def summarize(results, profile_sha256, *, native_requested):
    counts = {name: sum(1 for item in results if item['status'] == name) for name in ('passed', 'failed', 'not_run')}
    native_ran = any(item['scope'] == 'native' and item['status'] != 'not_run' and item['case_id'] != 'native/descriptor' for item in results)  # a descriptor failure reached no Hub
    return {'profile_id': PROFILE_ID, 'profile_sha256': profile_sha256, 'provenance': 'actual-hub' if native_ran else 'offline-profile',
            'native_requested': native_requested, 'runs': len(results), **counts, 'complete': counts['failed'] == 0 and counts['not_run'] == 0,
            'results': results}


def run(config=None, *, profile_dir=DEFAULT_PROFILE, timeout=None):
    """Offline checks always; native cases when a descriptor path is given. Returns the summary dictionary."""
    started = time.monotonic()
    results = check_offline(profile_dir)
    try:
        profile = Profile(profile_dir)
    except (ContractError, OSError, KeyError, TypeError):
        return summarize(results, None, native_requested=config is not None)
    if config is None:
        return summarize(results + unrun_native(profile, ['descriptor']), profile.info['profile_sha256'], native_requested=False)
    try:
        descriptor = load_descriptor(config, profile)
        if any(item['status'] == 'failed' for item in results):
            raise hub_http.AcceptanceError('profile checks failed; native checks were not started')
        native = NativeRun(profile, descriptor, timeout=None if timeout is None else max(0.0, timeout - (time.monotonic() - started)))
    except hub_http.AcceptanceError as failure:
        reason = str(failure)
    except (OSError, ssl.SSLError):
        reason = 'descriptor certificate could not be loaded'
    else:
        return summarize(results + native.run(), profile.info['profile_sha256'], native_requested=True)
    results = results + [{'scope': 'native', 'case_id': 'native/descriptor', 'status': 'failed', 'detail': reason}]
    return summarize(results + unrun_native(profile, ['usable descriptor']), profile.info['profile_sha256'], native_requested=True)


EXIT_FAILED, EXIT_INCOMPLETE = 1, 3


def exit_code(summary):
    """0 only when everything that was asked for passed; 1 when a check failed; 3 when nothing failed but a requested check did not run."""
    if summary['failed']:
        return EXIT_FAILED
    pending = any(item['status'] == 'not_run' and (summary['native_requested'] or item['scope'] == 'offline') for item in summary['results'])
    return EXIT_INCOMPLETE if pending else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description='hub-http-v1@1.2.0 offline profile checks and native Hub checks',
                                     epilog='exit status: 0 every requested check passed, 1 a check failed, 3 no check failed but a requested check did not run (a run with --config that leaves any native case not_run is 3)')
    parser.add_argument('--config', default=None, help='private actual-Hub descriptor; native cases run only when it is given')
    parser.add_argument('--profile-dir', default=str(DEFAULT_PROFILE))
    parser.add_argument('--timeout-seconds', type=float, default=None, help='whole-run budget; native cases still pending when it ends are not_run')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(argv)
    try:
        summary = run(args.config, profile_dir=args.profile_dir, timeout=args.timeout_seconds)
    except Exception:
        # Paths, TLS and parser diagnostics may contain secrets.
        print(json.dumps({'status': 'failed', 'profile_id': PROFILE_ID, 'error': 'hub-http-v1@1.2.0 checks failed to start'}))
        return 1
    if args.json:
        print(json.dumps(summary, ensure_ascii=False, sort_keys=True, separators=(',', ':'), allow_nan=False))
    else:
        for item in summary['results']:
            tail = item.get('detail') or ('missing: ' + ', '.join(item['missing']) if item.get('missing') else '')
            print(item['status'].upper().replace('_', ' ') + ' ' + PROFILE_ID + ' ' + item['case_id'] + (': ' + tail if tail else ''))
        print('profiles=1 runs=%d passed=%d failed=%d not_run=%d' % (summary['runs'], summary['passed'], summary['failed'], summary['not_run']))
    return exit_code(summary)


if __name__ == '__main__':
    sys.exit(main())
