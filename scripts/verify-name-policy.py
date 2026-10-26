#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Check repository naming, bounded exceptions, frozen references and digest pins."""
import argparse
import base64
import collections
import datetime
import difflib
import fnmatch
import hashlib
import html
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import unicodedata
import urllib.parse

DEFAULT_CLASSES = {'read-sql', 'frozen-history', 'g2-4-recorded'}
OPTIONAL_CLASSES = {'deprecated-alias', 'n1-pending'}
EXPIRIES = {'none', 'G2-4-M1', 'G2-4-M2', 'G2-4-M5', 'G2-4', 'G2-5',
            'G5-7', 'lineage-2.1', 'G2-9', 'C6-R52a', 'N1b', 'N1c'}
DATA_PATHS = ['scripts/name-policy/' + name + '.json' for name in ('allow', 'pins', 'milestones')]
SCRIPT_PATH = 'scripts/verify-name-policy.py'
REQUIRED_INPUTS = [SCRIPT_PATH, 'scripts/test-name-policy.sh'] + DATA_PATHS
FORBIDDEN = {'vendor', 'upstream', 'repository-roots', 'node_modules', 'TeslatlasCore.xcframework'}
ARCHIVES = ('docs/development/archive/', 'protocol/docs/development/archive/', 'sdk-typescript/docs/development/archive/')
IMAGE_SUFFIXES = {'.png', '.jpg', '.jpeg', '.gif', '.webp', '.ico', '.icns', '.pdf'}
BINARY_SUFFIXES = IMAGE_SUFFIXES | {'.zst', '.gz', '.sqlite', '.tar', '.zip'}
REQUIRED_REFERENCES = ('legacy_names::', 'import_source_names_step::')
BASE_PATTERN = re.compile('tesla' + r'[\W_]{0,6}' + 'mate')
LOOKALIKES = {}
for _name, _latin in (
    ('CYRILLIC SMALL LETTER A', 'a'), ('CYRILLIC SMALL LETTER IE', 'e'),
    ('CYRILLIC SMALL LETTER DZE', 's'), ('CYRILLIC SMALL LETTER TE', 't'),
    ('CYRILLIC SMALL LETTER EM', 'm'), ('CYRILLIC SMALL LETTER PALOCHKA', 'l'),
    ('GREEK SMALL LETTER ALPHA', 'a'), ('GREEK SMALL LETTER EPSILON', 'e'),
    ('GREEK SMALL LETTER TAU', 't'), ('GREEK SMALL LETTER MU', 'm'),
    ('GREEK SMALL LETTER LAMDA', 'l')):
    LOOKALIKES[unicodedata.lookup(_name)] = _latin


class PolicyError(ValueError):
    pass


def json_bytes(payload):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise PolicyError('duplicate JSON key: ' + key)
            result[key] = value
        return result
    return json.loads(payload, object_pairs_hook=pairs)


def decoded_fields(value):
    if isinstance(value, list):
        return [decoded_fields(item) for item in value]
    if not isinstance(value, dict):
        return value
    result = {}
    for key, item in value.items():
        if key.endswith('_b64'):
            name = key[:-4]
            if name in value or name in result:
                raise PolicyError('both plain and encoded policy field: ' + name)
            def decode(text):
                try:
                    return base64.b64decode(text, validate=True).decode('utf-8')
                except (ValueError, UnicodeError, TypeError) as error:
                    raise PolicyError('invalid encoded field: ' + key) from error
            result[name] = [decode(text) for text in item] if isinstance(item, list) else decode(item)
        else:
            if key in result:
                raise PolicyError('duplicate decoded policy field: ' + key)
            result[key] = decoded_fields(item)
    return result


def replace_mapped(text, offsets, pattern, replacement):
    output, mapping, previous = [], [], 0
    for match in re.finditer(pattern, text, flags=re.DOTALL):
        output.append(text[previous:match.start()])
        mapping.extend(offsets[previous:match.start()])
        result = replacement(match)
        output.append(result)
        mapping.extend([offsets[match.start()]] * len(result))
        previous = match.end()
    output.append(text[previous:])
    mapping.extend(offsets[previous:])
    return ''.join(output), mapping


def decode_copy(text):
    offsets = list(range(len(text)))
    text, offsets = replace_mapped(text, offsets, r'(?:%[0-9a-fA-F]{2})+',
                                   lambda match: urllib.parse.unquote(match.group()))
    def unicode_escape(match):
        return json.loads('"' + match.group() + '"')
    text, offsets = replace_mapped(text, offsets,
        r'\\u[dD][89aAbB][0-9a-fA-F]{2}\\u[dD][c-fC-F][0-9a-fA-F]{2}|\\u[0-9a-fA-F]{4}', unicode_escape)
    text, offsets = replace_mapped(text, offsets, r'&(?:#[0-9]+|#[xX][0-9a-fA-F]+|[A-Za-z][A-Za-z0-9]+);',
                                   lambda match: html.unescape(match.group()))
    escapes = {'n': '\n', 't': '\t', 'r': '\r', '"': '"', "'": "'", '\\': '\\'}
    text, offsets = replace_mapped(text, offsets, r'\\[ntr"\'\\]', lambda match: escapes[match.group()[1]])
    text, offsets = replace_mapped(text, offsets, r'<[^>]*>', lambda _match: '')
    text, offsets = replace_mapped(text, offsets, r'[*_]+', lambda _match: '')
    return text, offsets


def normalize(text, offsets):
    # First retain source positions through compatibility expansions, then align
    # canonical compositions to the exact whole-string NFKC result.
    characters, mapping = [], []
    for character, offset in zip(text, offsets):
        normalized = unicodedata.normalize('NFKC', character)
        characters.extend(normalized)
        mapping.extend([offset] * len(normalized))
    intermediate = ''.join(characters)
    whole = unicodedata.normalize('NFKC', text)
    if intermediate != whole:
        remapped = []
        for operation, a, b, c, d in difflib.SequenceMatcher(None, intermediate, whole, autojunk=False).get_opcodes():
            if operation == 'equal':
                remapped.extend(mapping[a:b])
            else:
                remapped.extend([mapping[min(a, len(mapping) - 1)]] * (d - c))
        mapping = remapped
    output, positions = [], []
    for character, offset in zip(whole, mapping):
        if unicodedata.category(character) == 'Cf' or character == '\u00ad':
            continue
        for folded in character.casefold():
            output.append(LOOKALIKES.get(folded, folded))
            positions.append(offset)
    return ''.join(output), positions


def find_matches(text, extra=()):
    patterns = [BASE_PATTERN] + [re.compile(pattern) for pattern in extra]
    found = {}
    copies = [(text, list(range(len(text)))), decode_copy(text)]
    for copy, offsets in copies:
        normalized, mapped = normalize(copy, offsets)
        for pattern in patterns:
            for match in pattern.finditer(normalized):
                if match.start() == match.end():
                    raise PolicyError('empty extra-pattern match')
                start = mapped[match.start()]
                end = mapped[match.end() - 1] + 1
                # One source occurrence may be found raw and after decoding.
                found[start] = dict(offset=start, end=max(end, found.get(start, {}).get('end', 0)),
                                    line=text.count('\n', 0, start) + 1)
    return [found[key] for key in sorted(found)]


def binary_file(path, payload):
    if Path(path).suffix.lower() in BINARY_SUFFIXES or b'\0' in payload:
        return True
    try:
        payload.decode('utf-8')
        return False
    except UnicodeError:
        return True


def git_bytes(root, *arguments):
    return subprocess.run(['git', *arguments], cwd=root, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def source_paths(args):
    if args.candidate_manifest:
        # Import lazily so commit-message checks have no repository dependency.
        from candidate_source_manifest import load_candidate_paths
        return sorted(load_candidate_paths(args.repo, args.candidate_manifest))
    if args.files_from:
        paths = json_bytes(args.files_from.read_text())
        if not isinstance(paths, list) or any(not isinstance(item, str) for item in paths):
            raise PolicyError('file list must be an array of relative paths')
        if len(paths) != len(set(paths)):
            raise PolicyError('duplicate input path')
        return sorted(paths)
    return [item.decode('utf-8') for item in git_bytes(args.repo, 'ls-files', '-z').split(b'\0') if item]


def load_data(root):
    values = []
    for path in DATA_PATHS:
        values.append(decoded_fields(json_bytes((root / path).read_text(encoding='utf-8'))))
    return dict(zip(('allow', 'pins', 'milestones'), values))


def load_base(args):
    if args.base_dir:
        if args.base_dir.resolve() == args.repo.resolve():
            raise PolicyError('working tree cannot be its own ratchet base')
        if any((args.base_dir / path).is_symlink() for path in [SCRIPT_PATH] + DATA_PATHS):
            raise PolicyError('ratchet base inputs must not be symbolic links')
        exists = [(args.base_dir / path).is_file() for path in [SCRIPT_PATH] + DATA_PATHS]
        if not all(exists):
            raise PolicyError('mirror base lacks complete gate data and script')
        return load_data(args.base_dir), None
    if (args.files_from or args.candidate_manifest) and not args.base:
        raise PolicyError('mirror runs require --base-dir or --base')
    base = args.base or 'HEAD'
    history = git_bytes(args.repo, 'log', '-1', '--format=%H', base, '--', SCRIPT_PATH).strip()
    if not history:
        return None, dict(kind='first introduction in reachable history', base=base)
    try:
        git_bytes(args.repo, 'show', base + ':' + SCRIPT_PATH)
        values = [decoded_fields(json_bytes(git_bytes(args.repo, 'show', base + ':' + path))) for path in DATA_PATHS]
    except subprocess.CalledProcessError as error:
        raise PolicyError('base once contained the gate but its script/data is missing') from error
    return dict(zip(('allow', 'pins', 'milestones'), values)), None


def pin_payload(root, pin):
    payload = (root / pin['path']).read_bytes()
    if 'anchor' not in pin:
        return payload
    lines = payload.splitlines(keepends=True)
    anchors = [index for index, line in enumerate(lines) if line.decode('utf-8').rstrip('\r\n') == pin['anchor']]
    if len(anchors) != 1 or not pin['anchor'].startswith('#'):
        raise PolicyError('pin heading must occur exactly once: ' + pin['path'])
    index = anchors[0]
    if 'heading_occurrence' in pin and pin['heading_occurrence'] != sum(line.startswith(b'#') for line in lines[:index + 1]):
        # A fixed ordinal would reject ordinary new entries; this optional field
        # is deliberately unsupported rather than pretending it proves a move.
        raise PolicyError('heading-ordinal pins are unsupported')
    return b''.join(lines[index:])


def ratchet(current, base):
    growth, hard = [], []
    if base is None:
        return growth, hard
    before = {entry['id']: entry for entry in base['allow'].get('entries', [])}
    after = {entry['id']: entry for entry in current['allow'].get('entries', [])}
    for ident, entry in after.items():
        old = before.get(ident)
        if old is None:
            growth.append('new entry: ' + ident)
            continue
        for key in set(old) | set(entry):
            if entry.get(key) == old.get(key):
                continue
            if key == 'max_count' and isinstance(entry.get(key), int) and entry[key] <= old[key]:
                continue
            if key == 'review_by' and entry.get(key) and old.get(key) and entry[key] <= old[key]:
                continue
            growth.append('changed entry ' + ident + ': ' + key)
    if current['allow'].get('extra_patterns', []) != base['allow'].get('extra_patterns', []):
        growth.append('changed extra patterns')
    old_pins = {pin['path']: pin for pin in base['pins'].get('files', [])}
    new_pins = {pin['path']: pin for pin in current['pins'].get('files', [])}
    for path in set(old_pins) | set(new_pins):
        if old_pins.get(path) != new_pins.get(path):
            growth.append('changed pin: ' + path)
    old_m = base['milestones']
    new_m = current['milestones']
    if not set(old_m.get('closed', [])) <= set(new_m.get('closed', [])):
        hard.append('closed milestones cannot be removed')
    if set(old_m.get('enabled_classes', [])) != set(new_m.get('enabled_classes', [])):
        growth.append('changed enabled classes')
    if old_m.get('class_authorities', {}) != new_m.get('class_authorities', {}):
        growth.append('changed class authorities')
    if current['allow'].get('references', {}) != base['allow'].get('references', {}):
        growth.append('changed frozen-reference coverage')
    return growth, hard


def verify(args):
    paths = source_paths(args)
    missing = sorted(set(REQUIRED_INPUTS) - set(paths))
    if missing:
        raise PolicyError('gate files missing from scan input: ' + ', '.join(missing))
    for path in paths:
        relative = Path(path)
        if relative.is_absolute() or '..' in relative.parts or str(relative) != path or (args.repo / path).is_symlink():
            raise PolicyError('unsafe candidate path: ' + path)
        (args.repo / path).resolve().relative_to(args.repo)
        if any(part in FORBIDDEN for part in relative.parts):
            raise PolicyError('forbidden tracked directory: ' + path)
    data = load_data(args.repo)
    base, introduction = load_base(args)
    if args.explain:
        print(json.dumps(data, ensure_ascii=False, indent=2))
    growth, errors = ratchet(data, base)
    if growth and not args.accept_growth:
        errors.extend('unapproved growth: ' + reason for reason in growth)
    entries = data['allow'].get('entries', [])
    if len({entry['id'] for entry in entries}) != len(entries):
        errors.append('duplicate allow-entry id')
    enabled = set(data['milestones'].get('enabled_classes', []))
    if not DEFAULT_CLASSES <= enabled or not enabled <= DEFAULT_CLASSES | OPTIONAL_CLASSES:
        errors.append('invalid enabled classes')
    authorities = data['milestones'].get('class_authorities', {})
    for category in enabled & OPTIONAL_CLASSES:
        if not authorities.get(category):
            errors.append('optional class lacks authority: ' + category)
    pin_rows = data['pins'].get('files', [])
    pins = {pin['path']: pin for pin in pin_rows}
    if len(pins) != len(pin_rows):
        errors.append('duplicate pin path')
    for path, pin in pins.items():
        try:
            if path not in paths or hashlib.sha256(pin_payload(args.repo, pin)).hexdigest() != pin['sha256']:
                errors.append('pin mismatch or missing input: ' + path)
        except (OSError, PolicyError, UnicodeError) as error:
            errors.append(str(error))
    for path in ('docs/releases/changelog.md', 'docs/legal/changelog.md'):
        if path in paths and not pins.get(path, {}).get('anchor'):
            errors.append('required heading-anchored changelog tail pin missing: ' + path)
    today = datetime.date.today()
    closed = set(data['milestones'].get('closed', []))
    for entry in entries:
        ident = entry['id']
        required = {'id', 'class', 'paths', 'match', 'max_count', 'expires', 'reason', 'authority'}
        if not required <= set(entry) or not entry.get('reason') or not entry.get('authority'):
            errors.append('incomplete allow entry: ' + ident)
        if entry.get('class') not in enabled:
            errors.append('disabled class: ' + ident)
        expiry = entry.get('expires')
        if expiry not in EXPIRIES or expiry in closed:
            errors.append('invalid or closed expiry: ' + ident)
        restricted_class = {'G2-9': 'deprecated-alias', 'C6-R52a': 'frozen-history',
                            'N1b': 'n1-pending', 'N1c': 'n1-pending'}.get(expiry)
        if restricted_class is not None and entry.get('class') != restricted_class:
            errors.append('expiry is restricted to its class: ' + ident)
        if expiry == 'none':
            matching = [path for path in paths if any(fnmatch.fnmatchcase(path, glob) for glob in entry.get('paths', []))]
            if not matching or any(path not in pins for path in matching):
                errors.append('permanent entry lacks complete digest pins: ' + ident)
        else:
            try:
                milestone_date = data['milestones'].get('review_dates', {}).get(expiry)
                if milestone_date is not None and entry['review_by'] != milestone_date:
                    errors.append('review date does not match milestone: ' + ident)
                if datetime.date.fromisoformat(entry['review_by']) < today:
                    errors.append('review date has passed: ' + ident)
            except (KeyError, TypeError, ValueError):
                errors.append('invalid review date: ' + ident)
        if type(entry.get('max_count')) is not int or entry['max_count'] < 0:
            errors.append('invalid match count: ' + ident)
        pattern = entry.get('match')
        if not isinstance(pattern, str) or not pattern:
            errors.append('empty or invalid match: ' + ident)
        if entry.get('match_kind', 'literal') == 'regex' and not (pattern.startswith('^') and pattern.endswith('$')):
            errors.append('allow patterns must be anchored: ' + ident)
        if entry.get('class') == 'g2-4-recorded' and not entry.get('recorded_row'):
            errors.append('recorded exception lacks a dated row: ' + ident)
        if entry.get('class') == 'deprecated-alias' and expiry != 'G2-9':
            errors.append('alias expiry must be its programme milestone: ' + ident)
    findings, warnings, opaque = [], [], []
    counts = collections.Counter()
    by_file, by_class = collections.Counter(), collections.Counter()
    references = data['allow'].get('references', {})
    extra = data['allow'].get('extra_patterns', [])
    for path in paths:
        payload = (args.repo / path).read_bytes() if path not in args.count_only else None
        is_binary = binary_file(path, payload) if payload is not None else False
        if payload is not None and (is_binary or path.startswith(ARCHIVES) or
             path.startswith('protocol/profiles/hub-http-v1/1.2.0/') or
             Path(path).name.startswith('release-notes-')) and path not in pins:
            errors.append('required digest pin missing: ' + path)
        sources = [('path', path)]
        if path in args.count_only:
            if args.strict or not args.report:
                errors.append('count-only evidence cannot satisfy a strict gate: ' + path)
            result = subprocess.run(['grep', '-c', '-i', 'tesla' + '[[:punct:][:space:]_]*' + 'mate', str(args.repo / path)],
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            if result.returncode not in (0, 1):
                errors.append('count-only grep failed: ' + path)
            opaque.append(dict(path=path, matching_lines=int(result.stdout.strip() or 0), owner='coordinator',
                               method='grep -c only; decoded and occurrence counts not established'))
        elif not is_binary:
            text = payload.decode('utf-8')
            sources.append(('text', text))
            for prefix in REQUIRED_REFERENCES:
                coverage = references.get(prefix, [])
                coverage = coverage.get('paths', []) if isinstance(coverage, dict) else coverage
                if re.search(r'\b' + re.escape(prefix.removesuffix('::')) + r'\b', text) and not any(fnmatch.fnmatchcase(path, glob) for glob in coverage):
                    errors.append('unlisted frozen reference: ' + path + ' ' + prefix)
        for kind, text in sources:
            for hit in find_matches(text, extra):
                start = hit['offset']
                line_start = text.rfind('\n', 0, start) + 1
                line_end = text.find('\n', hit['end'])
                context = text[line_start:line_end if line_end >= 0 else len(text)]
                covering = []
                for entry in entries:
                    if not any(fnmatch.fnmatchcase(path, glob) for glob in entry.get('paths', [])):
                        continue
                    pattern = entry.get('match', '')
                    if entry.get('match_kind', 'literal') == 'regex':
                        accepted = re.fullmatch(pattern, context) is not None
                    else:
                        accepted = pattern in context
                    if accepted:
                        covering.append(entry)
                issue = None
                if len(covering) != 1:
                    issue = 'uncovered occurrence' if not covering else 'multiply covered occurrence'
                else:
                    entry = covering[0]
                    counts[entry['id']] += 1
                    by_class[entry['class']] += 1
                by_file[path] += 1
                findings.append(dict(path=path, kind=kind, line=hit['line'], offset=start,
                    end=hit['end'], entries=[entry['id'] for entry in covering], issue=issue))
    for entry in entries:
        actual, expected = counts[entry['id']], entry.get('max_count', 0)
        if actual > expected or (args.strict and actual < expected):
            errors.append('entry count mismatch: %s expected %s actual %s' % (entry['id'], expected, actual))
        elif actual < expected:
            warnings.append('entry count decreased: ' + entry['id'])
    summary = dict(schema='teslatlas.name-policy-report/v1', paths=len(paths), matches=findings,
        counts_by_file=dict(by_file), counts_by_class=dict(by_class), entry_counts=dict(counts),
        errors=sorted(set(errors)), warnings=warnings, count_only=opaque,
        accepted_growth=args.accept_growth, growth=growth, first_introduction=introduction,
        strict=args.strict, report_only=args.report, base_dir=str(args.base_dir) if args.base_dir else None)
    if args.summary:
        args.summary.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(dict(paths=len(paths), matches=len(findings), errors=len(summary['errors']),
         uncovered=sum(hit['issue'] is not None for hit in findings), count_only=len(opaque),
         first_introduction=introduction, accepted_growth=args.accept_growth), ensure_ascii=False))
    for error in summary['errors']:
        print('name-policy: ' + error, file=sys.stderr)
    return 1 if errors or (not args.report and any(hit['issue'] for hit in findings)) else 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, default=Path.cwd())
    sources = parser.add_mutually_exclusive_group()
    sources.add_argument('--files-from', type=Path)
    sources.add_argument('--candidate-manifest', type=Path)
    bases = parser.add_mutually_exclusive_group()
    bases.add_argument('--base-dir', type=Path)
    bases.add_argument('--base')
    parser.add_argument('--count-only', action='append', default=[], metavar='PATH', help='explicit report-only count evidence; never a strict exemption')
    parser.add_argument('--strict', action='store_true')
    parser.add_argument('--report', action='store_true')
    parser.add_argument('--accept-growth')
    parser.add_argument('--summary', type=Path)
    parser.add_argument('--explain', action='store_true')
    parser.add_argument('--commit-msg', type=Path)
    args = parser.parse_args(argv)
    try:
        if args.commit_msg:
            extra = load_data(args.repo)['allow'].get('extra_patterns', [])
            hits = find_matches(args.commit_msg.read_text(encoding='utf-8'), extra)
            print(json.dumps(dict(commit_message_matches=len(hits))))
            return 1 if hits else 0
        args.repo = args.repo.resolve()
        if args.accept_growth is not None and not args.accept_growth.strip():
            raise PolicyError('growth acceptance needs a reason')
        return verify(args)
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print('name-policy: ' + str(error), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
