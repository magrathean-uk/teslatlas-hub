#!/bin/sh
# Local gate for the Home Assistant package (home-assistant/ in the hub).
# Usage: tools/gate.sh [--dry-run]
# Receipt paths in compatibility/hub.json are relative to the workspace root
# (the parent of the hub's main checkout, found through the git common directory,
# so it is the same from a git worktree); override with TESLATLAS_WORKSPACE.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

dry=0
case "${1-}" in
  "") ;;
  --dry-run) dry=1 ;;
  *) echo "usage: tools/gate.sh [--dry-run]" >&2; exit 2 ;;
esac

step() {
  echo "gate: $*"
  [ "$dry" -eq 1 ] || "$@"
}

compat_check() {
  common=$(git rev-parse --path-format=absolute --git-common-dir)
  ws=${TESLATLAS_WORKSPACE:-$(dirname "$(dirname "$common")")}
  python3 -I - "$repo" "$ws" <<'PY'
import hashlib, json, pathlib, sys
repo, ws = map(pathlib.Path, sys.argv[1:3])
data = json.loads((repo / "compatibility/hub.json").read_text(encoding="utf-8"))
want = data["profile"]["sha256"]
sums = repo / "custom_components/teslatlas_hub/profile/hub-http-v1" / data["profile"]["revision"] / "SHA256SUMS"
got = hashlib.sha256(sums.read_bytes()).hexdigest()
bad = []
if got != want:
    bad.append(f"profile hash mismatch: hub.json {want}, bundled {got}")
for rel in data["test_receipt_paths"]:
    if not (ws / rel).is_file():
        bad.append(f"missing receipt: {rel}")
if bad:
    print("\n".join(bad), file=sys.stderr)
    sys.exit(1)
PY
}

step uv run --locked pytest
step uv run --locked --no-default-groups --group floor pytest
step uv run --locked ruff check .
echo "gate: compatibility/hub.json profile hash and receipt paths"
[ "$dry" -eq 1 ] || compat_check
echo "gate: ok"
