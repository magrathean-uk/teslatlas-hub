# Security Policy

## System and scope

Teslatlas Protocol publishes source-neutral schemas, OpenAPI documents, event
contracts, profiles, fixtures, and conformance tooling. It does not implement
the Hub service, SDKs, the Teslatlas App, deployment infrastructure, or a
hosted endpoint.

Report protocol defects that could cause an implementation to expose vehicle,
location, account, credential, or command data; cross an authenticated
principal boundary; accept an invalid or replayed credential; weaken TLS or
receipt verification; or make unsafe public test data appear valid.

## Security properties

- Public fixtures and examples must remain deterministic and redacted. They
  must not contain real identifiers, credentials, secrets, raw provider data,
  or precise locations.
- Authenticated data, cursors, ETags, and event replay must remain bound to
  the authorized principal described by the applicable profile.
- Current-Hub conformance inputs containing credentials or private evidence
  must be owner-only, bounded, and excluded from logs and public artifacts.
- Protocol profiles must reject malformed, oversized, conflicting, or
  unbound data where their schemas and conformance cases require it.

## Reporting

Follow the [published Magrathean UK security policy](https://raw.githubusercontent.com/magrathean-uk/.github/main/SECURITY.md)
to report a suspected vulnerability privately. Email
[contact@magrathean.uk](mailto:contact@magrathean.uk) with the subject
`SECURITY: teslatlas-protocol`. This is the organisation's existing reporting
route.

Do not open a public issue, discussion, or pull request for a suspected
vulnerability. GitHub's private reporting option may also be used when it is
enabled for the repository; its availability is not asserted here.

For non-sensitive documentation or protocol questions, follow
[SUPPORT.md](SUPPORT.md). No supported-version commitment is documented by this
repository.

Include the affected protocol profile, contract version, minimal redacted
reproduction, and the observed and expected behaviour. Do not include bearer
tokens, pairing secrets, private paths, raw vehicle data, or precise locations.

## Scope limits

Implementation flaws in Hub, SDKs, applications, deployments, and external
services belong to their owners. A defect in this repository's contract,
fixtures, or conformance tooling remains in scope when it materially enables
or obscures such a flaw.

The local conformance suite records bounded contract evidence. It is not a
security audit or a guarantee of implementation or product security.
