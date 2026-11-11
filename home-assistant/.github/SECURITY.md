# Security policy

How to report a vulnerability in this integration privately, and the trust boundaries a change must preserve.

## Report privately

Email [contact@magrathean.uk](mailto:contact@magrathean.uk) with subject `SECURITY: teslatlas-home-assistant`. This route is published in the [organisation security policy](https://github.com/magrathean-uk/.github/blob/main/SECURITY.md), which also describes handling and disclosure. Use GitHub's private vulnerability reporting option only if it is available for this repository.

Do not post vulnerability details in a public issue or pull request. Include the affected commit or component version, deployment context, reproduction steps and impact. Use synthetic or redacted evidence. Do not send live credentials, invitations, private keys, personal data or production database extracts.

No release-support matrix or fixed response-time commitment is established here. The documented candidate baseline is not a production-support promise.

## Scope and trust boundaries

The repository owns the custom integration's HTTP client, pairing and reauthentication flows, config-entry handling, diagnostics and embedded profile. Hub responses cross a network trust boundary. Saved device bearers, endpoint identities and vehicle data are sensitive; local access to the Home Assistant configuration must be protected.

Review these properties when changing the integration:

- Verify the expected Hub identity before sending a saved bearer, including during rotation.
- Preserve certificate and hostname validation when TLS is used. Check configured pins on the active connection before sending credentials or invitation content.
- Reject redirects and bound response parsing, polling and concurrent requests.
- Keep invitation material transient and sensitive data out of diagnostics, fixtures and public reports.
- Preserve the public HTTP and read-only sensor boundaries. There are no vehicle commands, Tesla credentials or private collector calls.

The configuration flow exposes a TLS choice; do not assume the implementation rejects every plain HTTP endpoint. Use HTTPS for credential-bearing traffic. Diagnostic redaction does not make configuration backups safe to share.

Credential disclosure, identity confusion, unsafe response handling and failures of these boundaries are reportable with a concrete reproduction and impact. No finding class is dismissed solely because this is a local integration.

The Hub service, Tesla services and third-party deployments are separately operated. A defect in this integration's handling of them remains reportable, but testing systems you do not own requires their operator's authorisation. This policy does not grant additional testing permissions.
