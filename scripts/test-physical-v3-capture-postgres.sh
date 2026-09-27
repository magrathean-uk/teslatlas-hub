#!/bin/zsh
# SPDX-License-Identifier: AGPL-3.0-only
set -euo pipefail

postgres_bin=/opt/homebrew/opt/postgresql@18/bin
schema_dump=${TESLATLAS_TEST_TESLAMATE_SCHEMA_DUMP:?set TESLATLAS_TEST_TESLAMATE_SCHEMA_DUMP to the private TeslaMate custom dump}
lab_tmp_root=${TESLATLAS_TEST_PRIVATE_TMP_ROOT:-${HOME}/dev/lab/teslatlas-v7/tmp}
hub_root=${0:A:h:h}
workspace_root=${hub_root:h}

[[ -f ${schema_dump} ]] || {
  print -u2 'physical V3 test schema dump is not a regular file'
  exit 2
}
[[ -x ${postgres_bin}/initdb && -x ${postgres_bin}/pg_restore ]] || {
  print -u2 'PostgreSQL 18 test tools are unavailable'
  exit 2
}
[[ -d ${lab_tmp_root} ]] || {
  print -u2 'private lab temporary root is unavailable'
  exit 2
}

umask 077
fixture_root=$(mktemp -d "${lab_tmp_root%/}/physical-v3-pg.XXXXXX")
chmod 0700 "${fixture_root}"
data_root=${fixture_root}/data
socket_root=${fixture_root}/socket
mkdir -m 0700 "${socket_root}"
fixture_port=$(python3 -c 'import socket; value = socket.socket(); value.bind(("127.0.0.1", 0)); print(value.getsockname()[1]); value.close()')

cleanup() {
  if [[ -s ${data_root}/postmaster.pid ]]; then
    "${postgres_bin}/pg_ctl" -D "${data_root}" -m immediate -w stop >/dev/null 2>&1 || true
  fi
  case ${fixture_root} in
    ${lab_tmp_root%/}/physical-v3-pg.*) rm -rf -- "${fixture_root}" ;;
    *) print -u2 'refusing to remove unexpected physical V3 fixture path' ;;
  esac
}
trap cleanup EXIT INT TERM

"${postgres_bin}/initdb" \
  -D "${data_root}" \
  --username=fixture_owner \
  --auth-local=trust \
  --auth-host=trust \
  --no-instructions >/dev/null
"${postgres_bin}/pg_ctl" \
  -D "${data_root}" \
  -l "${fixture_root}/postgres.log" \
  -o "-h 127.0.0.1 -p ${fixture_port} -k ${socket_root}" \
  -w start >/dev/null
"${postgres_bin}/createdb" \
  -h "${socket_root}" -p "${fixture_port}" -U fixture_owner teslamate_fixture
"${postgres_bin}/pg_restore" \
  -h "${socket_root}" -p "${fixture_port}" -U fixture_owner \
  -d teslamate_fixture \
  --schema-only --schema=public --no-owner --no-privileges \
  "${schema_dump}" >/dev/null
"${postgres_bin}/psql" \
  -h "${socket_root}" -p "${fixture_port}" -U fixture_owner \
  -d teslamate_fixture -v ON_ERROR_STOP=1 -q <<'SQL'
CREATE ROLE fixture_reader LOGIN;
GRANT CONNECT ON DATABASE teslamate_fixture TO fixture_reader;
GRANT USAGE ON SCHEMA public TO fixture_reader;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO fixture_reader;
SQL

export TESLATLAS_HUB_PHYSICAL_CAPTURE_TEST_POSTGRES_URL="postgresql://fixture_reader@127.0.0.1:${fixture_port}/teslamate_fixture"
export TESLATLAS_HUB_PHYSICAL_CAPTURE_TEST_POSTGRES_ADMIN_URL="postgresql://fixture_owner@127.0.0.1:${fixture_port}/teslamate_fixture"
"${workspace_root}/scripts/dev/with-heavy-build-lock.sh" \
  "${workspace_root}/scripts/dev/run.sh" hub cargo test --locked --lib \
    physical_v3_capture_uses_one_exported_snapshot_and_discards_hook_failures_when_configured \
    -- --nocapture
