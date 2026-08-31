# GitHub repository settings

This public repository stores source, issues, review history, tags and release
assets. It does not use GitHub-hosted build, test or release automation.

## About panel

Use this repository description:

> Self-hosted multi-vehicle Tesla telemetry collector and local Teslatlas sync hub for macOS and Debian.

Set the website to `https://teslatlas.eu` and keep these topics:

```text
debian fleet-api macos privacy rust self-hosted sqlite telemetry tesla teslamate vehicle-telemetry
```

The public landing page intentionally lives at `.github/README.md`; Cargo also
points to it. The repository layout gate forbids root Markdown. Root
`CITATION.cff` supplies GitHub's citation metadata.

## Repository controls

- default branch: `main`;
- web-based commits require DCO sign-off;
- squash merge only, using the pull-request title and body;
- merged branches are deleted automatically;
- wiki and projects remain disabled because maintained documentation lives in
  the repository;
- private vulnerability reporting, dependency alerts, secret scanning and push
  protection are enabled; and
- automated dependency fixes remain disabled pending explicit review.

## Protected references

The `main` ruleset blocks branch deletion and non-fast-forward updates. The
`v*` tag ruleset blocks deletion and updates of published release tags. Only a
repository administrator may change those rules in a documented emergency; no
routine bypass should exist.

Pull requests use `CODEOWNERS`, the contribution checklist, review and
conversation resolution. This single-maintainer repository must not claim a
GitHub status check passed when tests and release evidence were produced only
locally. Record local results against the exact commit.

Require verified signatures only after the release signing identity is
registered and independently anchored. DCO sign-off and cryptographic signing
are separate controls.

## Contributor and release records

Public contributor status should disclose only what is needed to operate the
merge gate. Executed agreements, signatures, home addresses and identity
records remain in encrypted Company-controlled storage.

Only an authorised MAGRATHEAN UK LTD maintainer creates an official release.
Publish the signed tag, exact source, platform artefacts, checksums, signatures,
SBOM, dependency notices, provenance and verification material as one complete
prerelease. A partial draft is not an official release.
