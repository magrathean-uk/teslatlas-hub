#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

# This command is intentionally explicit and one-shot. The Compose initializer
# has only CHOWN, FOWNER and DAC_OVERRIDE, verifies 10001:10001/0700 itself, and
# changes only the named data volume. Normal Hub startup still drops all caps.
exec docker compose run --rm --no-deps volume-init
