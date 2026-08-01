#!/usr/bin/env bash
# Emit deterministic bulk rows for the Hub-owned TeslaMate corpus fixture.
set -euo pipefail
IFS=$'\n\t'
set +x

readonly PROGRAM_NAME="${0##*/}"
rows=10000000

usage() {
  cat <<'EOF'
Usage: scripts/generate-large-position-corpus.sh [--rows N]

Writes one PostgreSQL INSERT statement to stdout. Apply it only after
fixtures/teslamate-corpus/v1/current-schema.sql and
fixtures/teslamate-corpus/v1/complete-selected-car.sql in a disposable,
Hub-owned corpus database. The default emits the representative 10,000,000
attached position rows. No database connection is opened by this program.
EOF
}

die() {
  printf '%s: %s\n' "$PROGRAM_NAME" "$*" >&2
  exit 1
}

while (($#)); do
  case "$1" in
    --rows)
      (($# >= 2)) || die "--rows requires a value"
      rows="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ "$rows" =~ ^[0-9]+$ ]] || die "--rows must be a decimal integer"
((rows >= 1 && rows <= 10000000)) || die "--rows must be in 1..=10000000"

cat <<SQL
-- Deterministic generated rows: version 1, selected car 1, drive 300.
INSERT INTO positions (
  id, car_id, drive_id, date, latitude, longitude, speed, power, odometer,
  ideal_battery_range_km, rated_battery_range_km, battery_level
)
SELECT
  1000000 + generated.row_number,
  1,
  300,
  timestamp '2026-02-01 00:00:00' + generated.row_number * interval '1 second',
  51.510000 + generated.row_number::numeric / 1000000000,
  -0.110000 - generated.row_number::numeric / 1000000000,
  40,
  20,
  1010.0 + generated.row_number::double precision / 1000.0,
  390.0,
  370.0,
  78
FROM generate_series(1, ${rows}) AS generated(row_number);
SQL
