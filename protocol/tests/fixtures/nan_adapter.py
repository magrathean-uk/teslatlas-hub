from __future__ import annotations

import json
import sys


for line in sys.stdin:
    request = json.loads(line)
    sys.stdout.write(
        '{"case_id":'
        + json.dumps(request["case_id"])
        + ',"step_id":'
        + json.dumps(request["step_id"])
        + ',"response":{"status":599,"headers":{},"body":NaN}}\n'
    )
    sys.stdout.flush()
