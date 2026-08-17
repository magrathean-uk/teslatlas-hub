# GitHub repository settings

Configure manually for the public Hub repository.

## Repository

- enable `Require contributors to sign off on web-based commits`;
- enable private vulnerability reporting;
- disable direct pushes to protected `main` except emergency administrators;
- disable force pushes and deletions;
- preserve issue templates for legal/security routing.

## Branch/ruleset

Require:

- pull request before merge;
- at least one approving review;
- conversation resolution;
- branch current with base;
- DCO status;
- licence/provenance status;
- tests and security checks;
- signed release tags.

Consider requiring verified signed commits. DCO sign-off and cryptographic commit signing are different controls.

## Contributor-assignment check

The public check should reveal only `verified/not verified` and an internal reference. Signed agreements remain private.

## Release

Only authorised Company maintainers create official releases. Attach exact source, checksums, signatures, SBOM and notices.
