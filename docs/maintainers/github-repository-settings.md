# GitHub repository settings

This public repository stores source, issues, review history, tags, and release
assets. It does not use GitHub-hosted build, test, or release automation.

## Repository

- default branch: `main`;
- web-based commits require a DCO sign-off;
- merged branches are deleted automatically;
- wiki and projects are disabled because maintained documentation lives in the
  repository;
- private vulnerability reporting, dependency alerts, secret scanning, and
  push protection are enabled;
- automated dependency fixes remain disabled.

## Protected references

The `main` ruleset blocks branch deletion and non-fast-forward updates. The
`v*` tag ruleset blocks deletion and updates of published release tags. Only a
repository administrator may change those rules in a documented emergency; no
ruleset bypass is configured.

Pull requests use `CODEOWNERS`, the contribution checklist, review, and
conversation resolution. This single-maintainer repository does not claim a
GitHub status check passed: format, tests, security, licence, provenance, and
release evidence are produced locally and recorded against the exact commit.

Verified signatures should be required only after the release signing key is
registered with the GitHub account and independently anchored. DCO sign-off
and cryptographic signing are separate controls.

## Contributor records

Any public assignment check reveals only `verified/not verified` and an
internal reference. Executed agreements and identity records remain private.

## Releases

Only an authorised MAGRATHEAN UK LTD maintainer creates an official release.
Publish the signed tag, exact source, platform artifacts, checksums,
signatures, SBOM, dependency notices, provenance, and verification material as
one complete prerelease. A partial draft is not an official release.
