#!/usr/bin/env bash
# Contract tests for bootstrap-from-git.sh flag validation and --dry-run.
set -euo pipefail
IFS=$'\n\t'

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
BOOTSTRAP="$ROOT/scripts/bootstrap-from-git.sh"
fail=0

assert_contains() {
  local haystack="$1"
  local needle="$2"
  if [[ "$haystack" != *"$needle"* ]]; then
    printf 'missing expected text: %s\n' "$needle" >&2
    fail=1
  fi
}

assert_exit() {
  local expected="$1"
  shift
  set +e
  output="$("$@" 2>&1)"
  code=$?
  set -e
  if [[ "$code" -ne "$expected" ]]; then
    printf 'expected exit %s, got %s for: %s\noutput:\n%s\n' \
      "$expected" "$code" "$*" "$output" >&2
    fail=1
  fi
  printf '%s' "$output"
}

# Unknown option is rejected.
assert_exit 1 bash "$BOOTSTRAP" --not-a-flag >/dev/null

# Dry-run requires the reviewed identity trio and changes nothing.
out="$(assert_exit 0 bash "$BOOTSTRAP" \
  --repo https://github.com/example/teslatlas-hub.git \
  --ref v0.1.0 \
  --commit 0123456789abcdef0123456789abcdef01234567 \
  --dry-run)"
assert_contains "$out" "dry-run: would clone"
assert_contains "$out" "0123456789abcdef0123456789abcdef01234567"
assert_contains "$out" "no packages, network package installs, credentials, or services changed"

# Dry-run still rejects a secret-bearing repository URL.
assert_exit 1 bash "$BOOTSTRAP" \
  --repo 'https://token@github.com/example/teslatlas-hub.git' \
  --ref v0.1.0 \
  --commit 0123456789abcdef0123456789abcdef01234567 \
  --dry-run >/dev/null

# Dry-run still rejects a short commit.
assert_exit 1 bash "$BOOTSTRAP" \
  --repo https://github.com/example/teslatlas-hub.git \
  --ref v0.1.0 \
  --commit deadbeef \
  --dry-run >/dev/null

if ((fail)); then
  printf 'bootstrap contract tests failed\n' >&2
  exit 1
fi
printf 'bootstrap contract tests passed\n'
