# Product versioning

The Python project, lockfile and Home Assistant manifest identify this integration as `2026.36.2`. The manifest supplies Home Assistant's displayed integration version.

Keep three version identities distinct:

| Identity | Recorded value | Meaning |
| --- | --- | --- |
| Integration | `2026.36.2` | Candidate component version |
| Home Assistant | `2026.8.3` | Pinned development and Compose baseline |
| Hub HTTP profile | `hub-http-v1@1.0.0` | Embedded protocol contract |

[compatibility/hub.json](../../compatibility/hub.json) accepts only the exact profile hash, tested Hub source fingerprint and named synthetic Debian 13 ARM64 Container receipts. A matching calendar version alone does not establish compatibility.

The [development status record](../development/archive/STATUS.json) describes historical validation and removal of its runtime. It is not evidence that the latest source has passed installed testing. No HACS, production, real-data, replacement-upgrade or general platform-support claim follows from the version number.
