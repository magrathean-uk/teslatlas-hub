# Security Policy

Report a security issue in the Teslatlas TypeScript SDK privately, using the route below.

## System and scope

This repository contains a TypeScript SDK for browser and Node.js clients. The
security-sensitive surface includes endpoint discovery, authenticated HTTP
requests, protocol decoding, current-Hub pairing, credential lifecycle, and
server-sent event replay.

The SDK does not operate a Hub service or persist credentials. Callers provide
the endpoint, authorization, credential store, event checkpoint store, and any
injected transport.

## Trust boundaries and invariants

Network responses and discovery data are untrusted until the SDK validates
them against the locked protocol artifacts. Public client operations use named
protocol methods rather than arbitrary paths. Callers must keep credential and
checkpoint stores scoped to the appropriate authorization principal and event
filters.

Current-Hub pairing must preserve normal TLS certificate and hostname checks.
The Node transport must also verify the invitation's leaf-certificate pin on
the same connection before sending claim bytes. A browser client must fail
closed when no pin-capable claim transport is available. The SDK must not
disable TLS verification, expose credentials or raw response bodies through
errors, retry a command after uncertain dispatch, or report a stale operation
as successful after logout or disposal.

## Reportable findings

Report a finding when the SDK can expose credentials, bypass endpoint,
identity, TLS, or pin validation, accept unsafe protocol data, cross an
authorization or replay-checkpoint boundary, retry a non-idempotent command,
or misrepresent the outcome of a cancelled or disposed operation. Include the
affected version, entry point, and a minimal reproduction without real
credentials or vehicle data.

## Reporting

Use the private email route in the [Magrathean UK organization security policy](https://github.com/magrathean-uk/.github/blob/main/SECURITY.md): [contact@magrathean.uk](mailto:contact@magrathean.uk), with subject `SECURITY: teslatlas-sdk-typescript`.

Do not report a suspected vulnerability in a public issue, discussion, or pull request. Provide a minimal, redacted reproduction with the affected version or commit, required permissions, and impact. Do not send live credentials, private keys, vehicle data, or production extracts. Follow the linked organization policy for research scope and disclosure handling.

GitHub private vulnerability reporting was disabled for this repository as of 2026-09-26. Report by email instead, using the route above.

## Version coverage

The package is private (`private: true`), with version `2026.36.2` in the manifest. There is no documented security support window or response-time commitment. Include the exact version and source or package identity in a report; do not infer broad support from historical runtime evidence.

## Limits

Vulnerabilities in a remote Hub service, a caller's credential or checkpoint
store, or a caller-supplied transport require the owner of that component to
remediate. They remain relevant here when SDK behaviour bypasses its documented
boundaries or enables the flaw.
