# Release legal and compliance gate

A release is not complete until every mandatory item below is evidenced.

## Source and GNU AGPL

- release commit and tag are immutable and signed;
- `LICENSE` is the unmodified GNU AGPL version 3 text;
- package metadata and source notices say `AGPL-3.0-only`;
- section 7 notices are present only where Magrathean has authority;
- modified upstream material is marked with source, revision, date and changes;
- every interactive interface exposes appropriate legal notices;
- every remotely used modified deployment has a prominent source route;
- object-code downloads have equivalent no-charge Corresponding Source access;
- build, install, configuration and interface-definition material needed to modify and run the release is included;
- no secret, signing key or proprietary dependency is needed to build the covered work.

## Provenance

- every file has a resolved classification;
- similarity scanning against TeslaMate and the proprietary repository has been reviewed;
- unknown or copied material is removed or correctly licensed;
- contributor agreements and DCO sign-offs are on file;
- company ownership and contractor assignments are current.

## Dependencies

- lockfile is final;
- licence allow/deny checks pass;
- SBOM is generated;
- third-party notices and licence texts are generated from the release;
- native libraries, vendored code, data and build tools are included in the review;
- vulnerability scan is reviewed and exceptions are signed off.

## Privacy and security

- data-flow record matches actual code;
- diagnostics are opt-in and redacted;
- retention/deletion/export controls are documented;
- threat model and security tests are current;
- no development credential or fake endpoint ships enabled;
- backup, restore and rollback are tested;
- release artefacts are signed and checksummed.

## Trade marks, APIs and platforms

- product name is cleared for the territories and platforms used;
- third-party branding is nominative and non-confusing;
- current API and app-store terms are reviewed;
- any use of Tesla systems is through an authorised route;
- privacy and consent flows satisfy the applicable API agreement;
- marketing contains no endorsement or official-status implication.

## Publication set

Publish together:

- binaries/packages;
- exact source archive;
- checksums and signatures;
- SBOM;
- dependency notice bundle;
- build and install instructions;
- migration/rollback notes;
- security and privacy notices;
- legal changelog.

Retain the signed release sign-off privately.
