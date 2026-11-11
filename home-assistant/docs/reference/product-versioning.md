# Product versioning

The Python project, lockfile and Home Assistant manifest identify this integration as `2026.36.2`. The manifest supplies Home Assistant's displayed integration version.

Keep these version identities distinct:

| Identity | Recorded value | Meaning |
| --- | --- | --- |
| Integration | `2026.36.2` | Candidate component version |
| Home Assistant development | `2026.9.4` | Locked Python test environment |
| Home Assistant minimum | `2026.8.3` | Minimum version declared in `hacs.json` |
| Compose example | `2026.8.3` | Pinned isolated evaluation image |
| Hub HTTP profile | `hub-http-v1@1.3.0` | Embedded protocol contract |

[compatibility/hub.json](../../compatibility/hub.json) pins the exact bundled profile hash and points to the local verification receipt. Its `locally_verified` status covers synthetic HTTP fixtures and the locked Home Assistant suites. No Hub runtime version or source fingerprint is claimed as tested. A matching calendar version alone does not establish compatibility.

The [development status record](../development/archive/STATUS.json) describes historical validation and removal of its runtime. It is not evidence that the latest source has passed installed testing. No HACS, production, real-data, replacement-upgrade or general platform-support claim follows from the version number.
