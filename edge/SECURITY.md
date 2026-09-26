# Security Policy

## System and scope

Teslatlas Edge is an optional, user-operated ingress between the Tesla Fleet
Telemetry receiver sidecar and a home Teslatlas Hub. It accepts decoded
telemetry envelopes from the sidecar on loopback, stores them in an encrypted
bounded spool, and makes batches available to the configured Hub.

This policy covers the Rust Edge service, its configuration and local state,
the patched Fleet Telemetry sidecar integration, and the Hub pull and
acknowledgement API. It does not describe Tesla account administration,
vehicle commands, consumer APIs, a mandatory central relay, or hosted service
operations because Edge does not provide those surfaces. A finding that lets
Edge reach or expose one of those excluded surfaces remains in scope.

## Reporting a vulnerability

Use the existing email route in the [Magrathean UK organization security
policy](https://raw.githubusercontent.com/magrathean-uk/.github/main/SECURITY.md):
[contact@magrathean.uk](mailto:contact@magrathean.uk), with the subject
`SECURITY: teslatlas-edge`.

Do not disclose suspected vulnerabilities in a public issue, discussion or pull
request. Include the affected version or commit, deployment context, required
permissions, a minimal reproduction and the impact. Redact credentials, private
keys, personal data and raw telemetry. GitHub private vulnerability reporting is
an alternative only when the affected repository offers it; its availability
is not assumed here.

This project adopts the organization's published reporting route. It does not
promise mailbox monitoring, delivery, a response deadline or a support window.

## Trust boundaries

- Tesla vehicles connect to the pinned receiver sidecar using Tesla mTLS.
- The sidecar posts strict, bounded envelopes to Edge on loopback with a
  private bearer.
- A Hub client connects to Edge's TLS listener with a client certificate from
  the configured Hub CA and a rotating bearer. The bearer currently grants the
  complete queue.
- Edge state includes encrypted pending records, sequence state, gap notices,
  and acknowledgement receipts. Private keys, bearer files, and the credential
  store are security-sensitive local files.

## Security invariants

- The Edge admission listener must remain loopback-only. It must not be published by
  a reverse proxy, firewall rule, or tunnel.
- Hub batch and acknowledgement endpoints must require both TLS client
  authentication and a valid bearer before they expose or delete queue state.
- Untrusted envelopes, configuration, and acknowledgement input must be
  bounded and validated before use.
- Pending telemetry must remain encrypted at rest. Logs, errors, metrics, and
  health responses must not disclose payloads, VINs, transaction IDs, bearer
  values, certificate material, or coordinates.
- Edge must preserve exact acknowledgement semantics. A malformed,
  unauthenticated, stale, oversized, duplicate, or out-of-order acknowledgement
  must not delete pending records or gap notices.
- Resource limits, corruption, and unrecoverable state must fail closed rather
  than silently dropping or replacing telemetry. Gap evidence must remain
  payload-free.
- Edge must not acquire, store, or expose Tesla account credentials,
  vehicle-command keys, or vehicle-command routes.

## Reportable findings and severity context

Report vulnerabilities that could bypass either authentication layer, expose or
modify telemetry across the sidecar, Edge, or Hub boundary, disclose protected
local state or telemetry, weaken encryption or key handling, delete data
without a valid exact acknowledgement, bypass limits, or turn a loopback-only
endpoint into a reachable network service.

Realistic reachability and impact matter. A theoretical issue in an unbuilt
historical artifact is less useful than a defect reachable in the current
source, configuration, packaged artifact, or deployed service. Tests and
historical acceptance records express intended behavior but do not prove a
current installation or deployment is secure.

## Known limitations and assessment context

No project-specific supported-release window or response commitment is
documented. The organization reporting route above is published; mailbox
monitoring and delivery have not been tested.
Historical status records do not establish current live-service exposure or
installed behavior. This policy describes repository boundaries, not a
completed security audit. The macOS LaunchAgent deployment shares the interactive
user identity and is documented as a development deployment rather than a
production process-isolation boundary.
