#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Run the audited Teslatlas companion source bootstrap."""

import sys
from pathlib import Path

script_path = Path(__file__).resolve()
packaged_module_root = script_path.parent
source_module_root = script_path.parents[1] / "tools"
module_root = (
    packaged_module_root
    if (packaged_module_root / "companions").is_dir()
    else source_module_root
)
sys.path.insert(0, str(module_root))

from companions.cli import install_signal_handlers, main  # noqa: E402

if __name__ == "__main__":
    install_signal_handlers()
    raise SystemExit(main())
