# Security Policy

## Scope

This repository contains the public Swift client products for the strict
protocol, deployed Hub v1 compatibility, and current-Hub profiles. It supports
iOS, macOS, and Linux through Swift Package Manager. The Hub service, client
applications, deployment infrastructure, and vehicle data stores are outside
this repository's implementation scope.

An SDK defect remains in scope when it can compromise a consuming application's
handling of a Hub, including defects at a profile boundary.

## Security Boundaries

The SDK receives remote discovery documents and HTTP responses. Credentials,
bearer tokens, pairing invitations, claim secrets, certificate pins, expected
Hub identities, origins, route templates, pagination cursors, and response
sizes are sensitive boundaries.

The current-Hub claim flow must validate the invitation's TLS identity and
certificate pin before it sends a claim secret. Redirects must remain rejected.
Credentials and invitation data must not appear in logs or diagnostic output.

## Reportable Findings

Report a finding when a realistic consumer can be affected by any of the
following:

- a credential, claim secret, invitation, or other sensitive value is exposed;
- origin, Hub identity, certificate, redirect, or transport validation can be
  bypassed;
- a profile binding accepts an unsupported route, capability, version, or data
  shape;
- request or response bounds can be bypassed in a way that affects security or
  availability; or
- data from one Hub, vehicle, or time window can be used for another.

## Reporting

Report suspected vulnerabilities privately using the [published Magrathean UK organisation policy](https://raw.githubusercontent.com/magrathean-uk/.github/main/SECURITY.md): email [contact@magrathean.uk](mailto:contact@magrathean.uk) with the subject `SECURITY: teslatlas-sdk-swift`.

Do not open a public issue, discussion or pull request for a suspected vulnerability. Include the affected revision and product, impact, a synthetic reproduction and relevant redacted output. Never send live credentials, bearer values, invitation links, claim secrets, private certificates or vehicle data.

GitHub private vulnerability reporting was disabled when checked on 2026-09-26; the published email route remains available. This document does not claim that mailbox monitoring or delivery has been tested.

## Limitations

This policy describes the SDK boundary. It is not a security audit and does not
claim support for a particular release line or response time.
