# Dependency and licence policy

## Default acceptable licences

Common permissive licences may be accepted after verification, including MIT, BSD-2-Clause, BSD-3-Clause, ISC, Apache-2.0 and Zlib.

## Manual review

Review is mandatory for:

- GPL, AGPL, LGPL, MPL, EPL and other copyleft or weak-copyleft components;
- custom or non-SPDX terms;
- source-available licences;
- fonts, icons, maps, models, corpora and databases;
- Git dependencies, forks and vendored code;
- generated code with uncertain output rights;
- copied forum or Stack Overflow material;
- dual licensing;
- advertising clauses;
- field-of-use, ethical-use or non-commercial restrictions;
- native libraries and build tools.

## Prohibited

Do not release material with no identified licence, confidential/leaked source, incompatible restrictions, missing required source/notices or rights the Company cannot grant.

## Release tools

Run and retain outputs from:

- `cargo metadata --locked --all-features`;
- licence allow/deny analysis;
- vulnerability scanning;
- SBOM generation;
- notice generation;
- secret scanning;
- provenance/header checks.

Every lockfile change requires a recorded dependency diff.

For a dependency whose licence requires source availability, release evidence
must include the exact locked source or an independently durable, version-bound
source location, plus an accompanying notice that tells recipients how to
obtain it. A licence text or repository homepage alone is not a source offer.

Generate `scripts/legal-bundle.py` after unsigned sidecar evidence and before
platform packaging. Every distributed package must embed that exact verified
bundle; release evidence must recompute the Rust material, verify the Go and
Fleet locks, and byte-compare every packaged component.

When a package declares a standard SPDX expression but its archive omits the
licence text, release evidence may use only the pinned canonical SPDX corpus in
`LICENSES/`, as documented by `LICENSE_CORPUS.md`. A missing identifier or text
blocks release. The fallback does not choose a licence alternative or replace
package-specific notices.
