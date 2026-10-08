from __future__ import annotations

import json
import sys


for line in sys.stdin:
    print(line.rstrip("\n"), flush=True)

print(json.dumps({"unexpected": True}), flush=True)
