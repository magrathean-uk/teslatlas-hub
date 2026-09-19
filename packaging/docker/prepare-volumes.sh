#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

# This command is intentionally explicit and one-shot. It only changes the
# newly created named data volume; normal service startup never chowns state.
exec docker compose run --rm --user 0 --entrypoint /bin/sh hub -c \
  'install -d -o 10001 -g 10001 -m 700 /var/lib/teslatlas-hub'
