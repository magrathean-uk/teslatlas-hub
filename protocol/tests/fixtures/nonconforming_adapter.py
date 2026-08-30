from __future__ import annotations

import json
import sys


for line in sys.stdin:
    request = json.loads(line)
    print(
        json.dumps(
            {
                "case_id": request["case_id"],
                "step_id": request["step_id"],
                "response": {"status": 599, "headers": {}},
            }
        ),
        flush=True,
    )

