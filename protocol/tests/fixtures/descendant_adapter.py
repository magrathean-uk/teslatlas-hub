from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path


marker = Path(sys.argv[1])
subprocess.Popen(
    [
        sys.executable,
        "-c",
        (
            "import time; from pathlib import Path; "
            f"time.sleep(0.4); Path({str(marker)!r}).write_text('alive', encoding='utf-8')"
        ),
    ],
    stdin=subprocess.DEVNULL,
)
time.sleep(60)
