# Public protocol binding

The HTTP adapter uses the embedded `hub-http-v1@1.3.0` profile. Its [SHA256SUMS file](../../custom_components/teslatlas_hub/profile/hub-http-v1/1.3.0/SHA256SUMS) has SHA-256:

```text
28957c2dfcca9efeefc7d7fee230ef769ee3c73833e60e0b0769d93bcf8b7221
```

This replaces the retired `hub-http-v1@1.0.0` binding. The 1.0.0 bundle remains for historical synthetic fixtures only. The 1.3.0 bundle is copied unchanged from the Hub's published profile.

The bundle defines discovery, invitation claim, credential rotation, vehicle listing, current state and bounded history queries, including the 4096-byte claim request/error contract. This integration uses discovery, claim, rotation, vehicles and current state. It does not query drives or invent event, command, charge, collector-health, cost, backup or data-quality endpoints.

The adapter keeps the existing request paths and claim body. It accepts an advertised discovery `profile` of `hub-http-v1@1.y.z` with minor revision `y` of 3 or more, following the profile's additive 1.x policy. Older revisions, other majors and malformed identifiers are rejected; historical fixtures without that member remain accepted. Current-state sensors prefer `activity_state`, `odometer_km`, `software_version` and `software_update_status`. The previous keys remain fallback aliases when a canonical key is absent; an explicit null or zero in a canonical key takes precedence.

The [compatibility record](../../compatibility/hub.json) has `locally_verified` status for this profile hash and the local synthetic verification receipt. Receipt paths are relative to the workspace root, as in `tools/gate.sh`. It makes no live-Hub, installed UI or current Debian lane acceptance claim. Historical container receipts apply to the retired binding only. The integration has not been published through HACS, and the former runtime was removed according to [the development record](../development/archive/STATUS.json).

Changes to the Hub or profile need explicit compatibility work and new evidence. A sibling SDK's old handoff does not establish current compatibility for this checkout.
