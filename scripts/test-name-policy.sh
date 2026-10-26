#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
python3 -B "$root/scripts/name-policy/test_name_policy.py"
python3 -B "$root/scripts/name-policy/test_repository_layout.py"
python3 -B "$root/scripts/name-policy/test_name_policy_git.py"
