#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Own-code marker check (DEC-070 rule 1).

Searches tracked files under src, tests, protocol, macos and scripts for
attribution markers: "ported/adapted/translated/copied/derived from" phrases
and upstream copyright or licence lines naming another project. Mentions of
documented behaviour are not markers. An allowance in
scripts/name-policy/allow.json blanks only its exact text in the listed file; any
other marker on the same line still counts. Run with --self-test for the probes.
This is a mechanical aid; review judgement still applies.
"""
import base64
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIRS = ("src/", "tests/", "protocol/", "macos/", "scripts/")
PROJECT = r"tesla\s*-?\s*mate"
VERBS = (r"(?i:ported|porting|adapted|translated|transliterated|copied|lifted|"
         r"derived|borrowed|forked|rewritten|re-?implemented|taken|cribbed|"
         r"pasted|reproduced|mirrored)")
# Any named source: a URL, a path-like owner/repo, a capitalised or CamelCase
# name, or a generic reference to another project (case-insensitive).
SOURCE = (r"(?:[`'\"\u2018\u201c]\s*(?!(?:first|last|next|self|none|null|true|false)\b)[A-Za-z][\w.+/-]{3,}|https?://|www\.|git@|[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+|(?![A-Za-z]*\.(?:lock|toml|json|md|rs|sh|py)\b)[A-Z][A-Za-z][\w.+-]+|"
          r"(?i:(?:the\s+)?(?:upstream\b|original\s+(?:project|source|implementation|code)|"
          r"another\s+project|other\s+project|open[- ]source|third[- ]party|external\s+(?:project|code|source))))")
OWN = r"(?:magrathean|teslatlas|bolyki)"
# One own-project copyright notice, up to the next ';' or line end, is blanked
# on its own so it never masks another marker on the same line.
OWN_NOTICE = re.compile(
    r"(?:spdx-filecopyrighttext:?|copyright|\u00a9|\(c\))(?:[\s\u00a9\d,-]|\(c\)|copyright)*"
    + OWN + r"[^;,\n]*", re.I)
LOWER = r"(?i:(?!(?:the|a|an|this|that|these|those|its|our|my|your|their|each|every|any|some|one|it|them|which|what|where|when|how|scratch|first)\b)[a-z][a-z0-9_.+-]{2,})"
STRONG = r"(?i:ported|porting|adapted|translated|transliterated|copied|lifted|cribbed|pasted|reproduced)"
MARKERS = [
    re.compile(r"\b" + STRONG + r"\s+(?:directly\s+|verbatim\s+|largely\s+)?"
               r"(?i:from|after|off|out\s+of)\s+" + LOWER),
    re.compile(r"\b" + VERBS + r"\s+(?:directly\s+|verbatim\s+|largely\s+)?"
               r"(?i:from|after|off|out\s+of)\s+" + SOURCE),
    re.compile(r"(?i:\b(?:based|modell?ed|patterned)\s+on\s+)" + SOURCE),
    re.compile(r"(?i:\b(?:code|implementation|logic|source)\s+(?:is\s+)?"
               r"(?:taken|copied|ported|adapted|derived)\b)"),
    re.compile(r"(?i:\bupstream\s+(?:source|code|implementation)\s+(?:of|from)\b)"),
    re.compile(r"\b(?:copyright\s*(?:\u00a9|\(c\))?\s*\d{4}|copyright\s*(?:\u00a9|\(c\))|"
               r"spdx-filecopyrighttext)|\(c\)\s+\d{4}|\u00a9", re.I),
    re.compile(r"(?i:" + PROJECT + r")[^\n]*(?i:contributors|source code|\bMIT licen)"),
]


def tracked():
    out = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-z"],
                         check=True, capture_output=True).stdout
    for raw in out.split(b"\0"):
        name = raw.decode("utf-8", "replace")
        if name.startswith(DIRS) and name != "scripts/verify-own-code.py" \
                and (ROOT / name).is_file():
            yield name


# Exact authorised texts that are not in the name-policy table (which is growth-guarded).
LOCAL_ALLOWED = {
    "macos/TeslatlasHubApp/TeslatlasHubApp/TeslaAuthWindowController.swift":
        ["// SPDX-FileCopyrightText: 2021 Adrian Kumpf"],  # MIT notice, DEC-072 item 1
    "src/geo/geocoder_cache_tests.rs":
        ["Data \u00a9 OpenStreetMap contributors, ODbL 1.0"],  # ODbL attribution string
}


def allowances():
    data = json.loads((ROOT / "scripts/name-policy/allow.json").read_text())
    table = {}
    for entry in data.get("entries", []):
        if "match_b64" in entry:
            pattern = re.compile(re.escape(
                base64.b64decode(entry["match_b64"]).decode("utf-8", "replace")))
        elif "match" in entry:
            pattern = re.compile(entry["match"])
        else:
            continue
        for path in entry.get("paths", []):
            table.setdefault(path, []).append(pattern)
    for path, texts in LOCAL_ALLOWED.items():
        table.setdefault(path, []).extend(re.compile(re.escape(t)) for t in texts)
    return table


def find_markers(line, allowances=()):
    """Markers left after blanking only the exact authorised text."""
    for pattern in allowances:
        line = pattern.sub(lambda m: " " * len(m.group(0)), line)
    line = OWN_NOTICE.sub(lambda m: " " * len(m.group(0)), line)
    return [m.group(0) for marker in MARKERS for m in marker.finditer(line)]


def main():
    allowed = allowances()
    hits = []
    for name in tracked():
        data = (ROOT / name).read_bytes()
        if b"\0" in data:
            continue
        for number, line in enumerate(data.decode("utf-8", "replace").splitlines(), 1):
            if find_markers(line, allowed.get(name, ())):
                hits.append((name, number))
    for name, number in hits:
        print(f"own-code: attribution marker at {name}:{number}", file=sys.stderr)
    print(json.dumps({"hits": len(hits)}))
    return 1 if hits else 0


def self_test():
    auth = ("macos/TeslatlasHubApp/TeslatlasHubApp/TeslaAuthWindowController.swift")
    allow = allowances().get(auth, ())
    approved = next(e for e in json.loads(
        (ROOT / "scripts/name-policy/allow.json").read_text())["entries"]
        if e["id"] == "tesla-auth-mit-attribution")["match_b64"]
    approved = base64.b64decode(approved).decode()
    NAMED = "".join(["Te", "sla", "Ma", "te"])  # probe name, not a literal
    must_fail = [
        ("// copied from SomeOtherProject", ()),
        ("// copied from https://github.com/example/project", ()),
        ("// Copyright 2020 ExampleProject contributors", ()),
        ("// (c) 2020 Example Corp", ()),
        ("// \u00a9 Example Corp", ()),
        ("// ported from the upstream project", ()),
        ("# adapted from acme/widgets", ()),
        ("// translated from " + NAMED, ()),
        (approved + "; copied from " + NAMED, allow),
        (approved + " and adapted from Other", allow),
        ("// copied from otherproject", ()),
        ("// adapted from `OtherProject`", ()),
        ("// ported from 'otherproject'", ()),
        ("// COPIED FROM OTHERPROJECT", ()),
        ("// derived from `otherproject`", ()),
        ("// translated from otherproject", ()),
        (approved + "; copied from otherproject", allow),
        (approved + "; adapted from `OtherProject`", allow),
        ("// Copyright 2020 OtherProject; Copyright 2026 Magrathean UK", ()),
        ("// Copyright 2026 Magrathean UK; Copyright 2020 OtherProject", ()),
        ("// Copyright 2026 Magrathean UK, copied from OtherProject", ()),
    ]
    must_pass = [
        ("// the value is derived from the stored token", ()),
        ("// Copyright 2026 Magrathean UK", ()),
        ("// follows the documented behaviour of the vendor API", ()),
        (approved, allow),
    ]
    bad = [t for t, a in must_fail if not find_markers(t, a)]
    bad += [t for t, a in must_pass if find_markers(t, a)]
    for text in bad:
        print(f"own-code self-test failed: {text}", file=sys.stderr)
    print(json.dumps({"self_test_failures": len(bad)}))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(self_test() if "--self-test" in sys.argv else main())
