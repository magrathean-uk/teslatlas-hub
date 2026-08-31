# Contributing

Bug reports, documentation fixes and focused code changes are welcome. Discuss
large protocol, storage, authentication, migration, licensing or branding work
before building it; this avoids wasted effort and incompatible designs.

## Before opening a pull request

1. Work from the current `main` branch and keep the change narrow.
2. Add or update tests and documentation for observable behaviour.
3. Sign off every commit under DCO 1.1 with `git commit -s`.
4. Record every external source, copied or adapted fragment, generated asset and
   material use of an AI tool.
5. Use synthetic data only. Never submit credentials, VINs, precise journeys or
   production databases.

## Rights and provenance

A non-trivial external contribution requires a signed individual or corporate
copyright assignment before merge. You may discuss or open the work first; the
maintainer will arrange the private agreement when the change is likely to be
accepted. Executed agreements and identity records are never stored in GitHub.

The pull request must identify:

- whether the work was written from scratch;
- every implementation, specification and repository consulted, with revision;
- copied, translated, adapted or generated material and its licence;
- any employment, client or confidentiality restriction; and
- any movement from the separate proprietary Teslatlas codebase.

“Available online” is not permission to copy. See the
[DCO and assignment process](../docs/governance/contributor-agreement-process.md)
and [provenance policy](../docs/legal/provenance.md).

## Validation

Run the gates that apply to the change:

```sh
python3 scripts/verify-repository-layout.py
python3 scripts/verify-provenance.py
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

Platform, packaging, migration or release changes need their dedicated test
scripts and a documented rollback. Preserve least privilege, bounded resource
use, stable redacted errors, licences and upstream notices.

Security vulnerabilities belong in the private route described in
[SECURITY.md](SECURITY.md), not a public issue or pull request. Submission does
not guarantee acceptance.
