#!/bin/bash
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/env.sh"
df -h "$TESLATLAS_LAB"
du -sh "$TESLATLAS_LAB"/* "$HOME/dev/toolchains/rust" 2>/dev/null

