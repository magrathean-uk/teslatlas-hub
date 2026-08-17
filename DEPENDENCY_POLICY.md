# Dependency and licence policy

## Default acceptable licences

Common permissive licences may be accepted after verification, including MIT, BSD-2-Clause, BSD-3-Clause, ISC, Apache-2.0 and Zlib.

## Manual review

Review is mandatory for:

- GPL, AGPL, LGPL and MPL components;
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
