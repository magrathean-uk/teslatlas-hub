#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Fail unless a rendered Compose model orders the bounded initializer safely."""

import json
import sys


model = json.load(sys.stdin)
services = model.get("services")
assert isinstance(services, dict)
assert set(services) == {"hub", "volume-init"}

hub = services["hub"]
initializer = services["volume-init"]
dependency = hub.get("depends_on", {}).get("volume-init")
assert isinstance(dependency, dict)
assert dependency.get("condition") == "service_completed_successfully"
assert dependency.get("required", True) is True
assert initializer.get("profiles") in (None, [])
assert initializer["entrypoint"] == [
    "/usr/local/libexec/teslatlas-hub-initialize-volume"
]
assert initializer.get("command") in (None, [])
assert initializer["user"] == "0:0"
assert initializer["read_only"] is True
assert initializer["cap_drop"] == ["ALL"]
assert initializer["cap_add"] == ["CHOWN", "FOWNER", "DAC_OVERRIDE"]
assert initializer["restart"] == "no"
assert hub["read_only"] is True
assert hub["cap_drop"] == ["ALL"]
print("rendered Compose dependency topology passed")
