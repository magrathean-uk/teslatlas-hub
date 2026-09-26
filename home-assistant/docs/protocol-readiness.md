# Public protocol binding

The HTTP adapter uses the embedded `hub-http-v1@1.0.0` profile. Its [SHA256SUMS file](../custom_components/teslatlas_hub/profile/hub-http-v1/1.0.0/SHA256SUMS) has SHA-256:

```text
b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926
```

The bundle defines discovery, invitation claim, credential rotation, vehicle listing, current state and bounded drive queries, including the 4096-byte claim request/error contract. This integration uses discovery, claim, rotation, vehicles and current state. It does not query drives or invent event, command, charge, collector-health, cost, backup or data-quality endpoints.

The [compatibility record](../compatibility/hub.json) has an accepted status for the exact product, profile, Hub source fingerprint and historical Debian 13 ARM64 Container lane named there. That status is narrower than general protocol or product readiness. The integration has not been published through HACS, and the former runtime was removed according to [the development record](development/STATUS.json).

Changes to the Hub or profile need explicit compatibility work and new evidence. A sibling SDK's old handoff does not establish current compatibility for this checkout.
