# Package lifecycle and final consumer gates

These gates consume immutable inputs. They do not discover a predecessor,
select a moving catalog, start Hub, or publish a package.

## Install, update, rollback and removal

Run from a clean source checkout at the exact catalog commit with Node `26.7.0`
and npm `11.19.0`:

```bash
npm run gate:package-lifecycle -- \
  --candidate /absolute/private/teslatlas-sdk-2026.36.2.tgz \
  --candidate-sha256 CANDIDATE_SHA256 \
  --candidate-receipt /absolute/private/candidate-admission.json \
  --candidate-receipt-sha256 CANDIDATE_RECEIPT_SHA256 \
  --candidate-source-export /absolute/private/candidate-source-export \
  --predecessor /absolute/private/teslatlas-sdk-PREVIOUS_VERSION.tgz \
  --predecessor-sha256 PREDECESSOR_SHA256 \
  --predecessor-receipt /absolute/private/predecessor-admission.json \
  --predecessor-receipt-sha256 PREDECESSOR_RECEIPT_SHA256 \
  --predecessor-source-export /absolute/private/predecessor-source-export \
  --catalog /absolute/private/catalog.json \
  --catalog-sha256 CATALOG_SHA256 \
  --receipt /absolute/private/package-lifecycle-receipt.json
```

Every path must be absolute, canonical and non-symlinked. The output receipt
must not exist. Before creating a temporary consumer or invoking npm, the gate
admits both archives with a strict tar reader, rejects duplicate keys in JSON,
and verifies each archive against its separate immutable, independently
accepted admission receipt and frozen source export. Each receipt binds the
exact commit, source-manifest SHA-256 and file count, archive SHA-256 and member
count, package version, toolchain and review verdict. The candidate must use
Node `26.7.0` and npm `11.19.0`; the historical 83-member archive
`42348d3688c5a723bd154e3c1e8172bc07b20d1bf28944818ccfdbf3d97891f7`
is explicitly ineligible for the changed candidate.

The strict Hub-owned schema-1 catalog must contain separate exact cohorts for
both the candidate and predecessor. Each cohort binds Protocol, TypeScript SDK,
Swift SDK, Home Assistant and Edge. Empty placeholders, missing components,
extra fields and mismatched source/package identities fail closed. The final
target separately binds the exact Hub source and artifact identity to the same
admitted cohort. The machine-readable contracts are
[`package-admission.schema.json`](../tools/package-admission.schema.json) and
[`f3-f6-catalog.schema.json`](../tools/f3-f6-catalog.schema.json).

The admitted sequence is candidate fresh install/import/removal, predecessor
install/import, candidate update/import, predecessor rollback/import, and final
removal with failed post-removal import. The caller-owned temporary consumer is
removed before a success receipt is written. If an exact predecessor is not
available, the gate stops without creating a consumer and no lifecycle claim is
made.

## Final F1 Hub Node and browser consumers

`gate:final-hub-consumers` is environment-driven because it reuses the existing
strict private handoff variables for `test:hub:node` and `test:hub:browser`. In
addition to those variables, it requires:

- `TESLATLAS_FINAL_HUB_TARGET`: owner-only JSON target described below;
- `TESLATLAS_FINAL_HUB_TARGET_SHA256`: exact target-file SHA-256;
- `TESLATLAS_HUB_CATALOG` and `TESLATLAS_HUB_CATALOG_SHA256`;
- `TESLATLAS_HUB_SDK_TARBALL` and `TESLATLAS_HUB_SDK_TARBALL_SHA256`;
- `TESLATLAS_HUB_SDK_CANDIDATE_RECEIPT` and
  `TESLATLAS_HUB_SDK_CANDIDATE_RECEIPT_SHA256`;
- `TESLATLAS_HUB_SDK_SOURCE_EXPORT`, the frozen reviewed source export;
- optional `TESLATLAS_FINAL_HUB_RECEIPT`, an absent absolute output path.

The Hub-owned HTTP descriptor must add an `acceptance_target` object structurally
identical to the `hub` object in this target:

```json
{
  "schema_version": 1,
  "state": "final-f1-artifact-ready",
  "hub_id": "00000000-0000-4000-8000-000000000001",
  "hub": {
    "repository": "https://github.com/magrathean-uk/teslatlas-hub.git",
    "product_version": "2026.36.2",
    "source_commit": "FULL_40_CHARACTER_HUB_COMMIT",
    "source_sha256": "HUB_SOURCE_MANIFEST_SHA256",
    "artifact_kind": "native",
    "artifact_sha256": "HUB_ARTIFACT_SHA256",
    "profile": {
      "id": "hub-http-v1",
      "revision": "1.0.0",
      "sha256": "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
    }
  },
  "descriptor_sha256": "HUB_HTTP_DESCRIPTOR_SHA256",
  "sdk": {
    "source_commit": "FULL_40_CHARACTER_SDK_COMMIT",
    "source_sha256": "SDK_SOURCE_EXPORT_SHA256",
    "package_sha256": "SDK_PACKAGE_SHA256",
    "package_version": "2026.36.2",
    "package_member_count": 86,
    "admission_receipt_sha256": "SDK_ADMISSION_RECEIPT_SHA256"
  },
  "catalog_sha256": "CATALOG_SHA256"
}
```

`artifact_kind` may be `native` or `container`; `86` is illustrative, not a
current candidate claim. The descriptor SHA-256 is computed from the exact
descriptor bytes before they are staged for the consumers. The target and
descriptor bind the separate Hub identity; the catalog must admit that Hub
product version and bind the same SDK source/package values, while the accepted
package receipt binds member count, toolchain and receipt identity. Only after
those checks does the gate install the archive in a fresh external consumer,
run Node invitation pinning and reads, then run the existing real-browser Web
PKI/CORS/read/rotation gate using the newly claimed caller-owned credential.
The gate removes its external consumer, credential and intermediate receipts
before writing success. It never starts or stops Hub.
