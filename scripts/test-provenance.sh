#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
VERIFY="$ROOT/scripts/verify-provenance.py"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-provenance-test.XXXXXX")
cleanup() {
    find "$TMP" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

make_repo() {
    destination=$1
    mkdir -p "$destination/scripts"
    git -C "$destination" init -q
    git -C "$destination" config user.name Test
    git -C "$destination" config user.email test@example.invalid
    cp "$VERIFY" "$destination/scripts/verify-provenance.py"
    printf '%s\n' 'fn main() {}' >"$destination/main.rs"
    printf '%s\n' data >"$destination/fixture.json"
    printf '%s\n' generated >"$destination/Cargo.lock"
    printf '%s\n' third-party >"$destination/LICENSE"
    cat >"$destination/provenance-manifest.json" <<'EOF'
{
  "schema": "teslatlas.provenance-classification/v1",
  "scope_note": "Classification fixture only; no chain-of-title conclusion.",
  "declared_classes": [
    "MAGRATHEAN-ORIGINAL",
    "COMPANY-ASSIGNED-CONTRIBUTION",
    "TESLAMATE-COMPATIBILITY",
    "THIRD-PARTY",
    "GENERATED",
    "DATA-OR-FACTS",
    "UNKNOWN"
  ],
  "rules": [
    {
      "id": "fixture-original",
      "class": "MAGRATHEAN-ORIGINAL",
      "rationale": "Test-only original inputs.",
      "paths": ["main.rs", "provenance-manifest.json"],
      "globs": ["scripts/*.py"]
    },
    {
      "id": "fixture-data",
      "class": "DATA-OR-FACTS",
      "rationale": "Test-only data.",
      "paths": ["fixture.json"],
      "exception": {
        "origin": "Created by the hermetic test.",
        "licensing": "No external rights asserted by the fixture.",
        "release_treatment": "Fixture only."
      }
    },
    {
      "id": "fixture-generated",
      "class": "GENERATED",
      "rationale": "Test-only generated lock.",
      "paths": ["Cargo.lock"],
      "exception": {
        "origin": "Created by the hermetic test.",
        "licensing": "Dependency rights would be handled separately.",
        "release_treatment": "Fixture only."
      }
    },
    {
      "id": "fixture-third-party",
      "class": "THIRD-PARTY",
      "rationale": "Test-only third-party placeholder.",
      "paths": ["LICENSE"],
      "exception": {
        "origin": "Created by the hermetic test.",
        "licensing": "Placeholder licence text.",
        "release_treatment": "Fixture only."
      }
    }
  ]
}
EOF
    git -C "$destination" add .
}

make_repo "$TMP/good"
python3 "$VERIFY" --repo "$TMP/good" >"$TMP/good.out"
grep -Fq '6 tracked files, exactly one class each' "$TMP/good.out"

cp -R "$TMP/good" "$TMP/missing"
printf '%s\n' uncovered >"$TMP/missing/new.txt"
git -C "$TMP/missing" add new.txt
if python3 "$VERIFY" --repo "$TMP/missing" >"$TMP/missing.out" 2>&1; then
    echo 'provenance test: unclassified tracked file was accepted' >&2
    exit 1
fi
grep -Fq 'unclassified tracked files:' "$TMP/missing.out"
grep -Fq 'new.txt' "$TMP/missing.out"

cp -R "$TMP/good" "$TMP/duplicate"
python3 - "$TMP/duplicate/provenance-manifest.json" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
manifest = json.loads(path.read_text())
manifest["rules"][0]["paths"].append("fixture.json")
path.write_text(json.dumps(manifest, indent=2) + "\n")
PY
if python3 "$VERIFY" --repo "$TMP/duplicate" >"$TMP/duplicate.out" 2>&1; then
    echo 'provenance test: duplicate classification was accepted' >&2
    exit 1
fi
grep -Fq 'tracked files with multiple classifications:' "$TMP/duplicate.out"

cp -R "$TMP/good" "$TMP/stale"
python3 - "$TMP/stale/provenance-manifest.json" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
manifest = json.loads(path.read_text())
manifest["rules"][0]["paths"].append("removed.rs")
path.write_text(json.dumps(manifest, indent=2) + "\n")
PY
if python3 "$VERIFY" --repo "$TMP/stale" >"$TMP/stale.out" 2>&1; then
    echo 'provenance test: stale explicit path was accepted' >&2
    exit 1
fi
grep -Fq 'explicit path is not tracked: removed.rs' "$TMP/stale.out"

cp -R "$TMP/good" "$TMP/unknown"
python3 - "$TMP/unknown/provenance-manifest.json" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
manifest = json.loads(path.read_text())
manifest["rules"][0]["class"] = "UNKNOWN"
path.write_text(json.dumps(manifest, indent=2) + "\n")
PY
if python3 "$VERIFY" --repo "$TMP/unknown" >"$TMP/unknown.out" 2>&1; then
    echo 'provenance test: UNKNOWN classification was accepted' >&2
    exit 1
fi
grep -Fq 'uses UNKNOWN, which blocks release' "$TMP/unknown.out"

cp -R "$TMP/good" "$TMP/undocumented"
python3 - "$TMP/undocumented/provenance-manifest.json" <<'PY'
import json
from pathlib import Path
import sys

path = Path(sys.argv[1])
manifest = json.loads(path.read_text())
del manifest["rules"][1]["exception"]["licensing"]
path.write_text(json.dumps(manifest, indent=2) + "\n")
PY
if python3 "$VERIFY" --repo "$TMP/undocumented" >"$TMP/undocumented.out" 2>&1; then
    echo 'provenance test: undocumented exception was accepted' >&2
    exit 1
fi
grep -Fq 'must document exactly:' "$TMP/undocumented.out"

for source_root in \
    "$ROOT/src" "$ROOT/tests" \
    "$ROOT/macos/TeslatlasHubApp/TeslatlasHubApp" \
    "$ROOT/macos/TeslatlasHubApp/TeslatlasHubAppTests"; do
    find "$source_root" -type f \( -name '*.rs' -o -name '*.swift' \) -print |
        while IFS= read -r source_file; do
            grep -Eq '^// SPDX-License-Identifier: (AGPL-3.0-only|MIT)$' "$source_file" || {
                echo "provenance test: missing or invalid source SPDX identifier: $source_file" >&2
                exit 1
            }
        done
done
find "$ROOT/scripts" "$ROOT/packaging/linux" "$ROOT/packaging/macos-service" \
    -type f \( -name '*.sh' -o -name '*.py' -o -name preinst -o -name postinst \
        -o -name prerm -o -name postrm \) -print |
    while IFS= read -r script_file; do
        grep -Fqx '# SPDX-License-Identifier: AGPL-3.0-only' "$script_file" || {
            echo "provenance test: missing tooling SPDX identifier: $script_file" >&2
            exit 1
        }
    done

printf '%s\n' 'provenance verifier tests passed'
