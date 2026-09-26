# Security policy

## Scope

Teslatlas Hub is self-hosted software for vehicle telemetry collection, local
storage, paired-device sync, and optional provider integrations. This policy
covers vulnerabilities in this repository and its released artefacts.

The project does not publish a supported-release lifecycle table. Reports may
identify a released version, a source commit, or the current `main` branch.

## Report privately

Email [contact@magrathean.uk](mailto:contact@magrathean.uk) with the subject
`SECURITY`. Do not use public issues for vulnerability reports.

Include the affected version or commit, platform and deployment topology,
reproduction steps, impact, preconditions, and redacted evidence. Do not send
credentials, private keys, VINs, precise locations, travel history, database
dumps, backups, pairing material, or other personal data.

The repository has no verified public statement of a response-time commitment,
bounty, or private GitHub advisory configuration. This policy makes none.

## Security boundaries

Reports are especially useful where they could break a documented boundary:

- provider access and refresh tokens, Fleet keys, pairing authority, signing
  keys, vehicle identity, location, history, or recovery keys;
- TLS and paired-device bearer authentication for remote sync;
- short-lived, single-use pairing invitations, where only the claim is an
  unauthenticated mutation;
- the private bearer on the supervised loopback Fleet Telemetry route;
- the private local bearer and matching `Host` header required for protected
  loopback routes;
- the requirement for TLS on a non-loopback bind;
- redaction of credentials and identifying telemetry in logs and diagnostics;
- bounded input, retention, retry, migration, and schema-admission behaviour;
  or
- release provenance, licence material, or source-identity integrity.

The controls above describe the intended design. A report that demonstrates a
control failure remains in scope.

## Testing boundaries

Only test systems and accounts you are authorised to use. Do not test Tesla,
TeslaMate, Apple, GitHub, a vehicle, another person's system, or another
provider merely because Teslatlas Hub can interoperate with it. Avoid actions
that could affect vehicle safety, credentials, personal data, availability, or
third-party services.

Account support, deployment help, and requests to recover data are not
vulnerability reports. A weakness in a third-party service is outside this
repository unless it is caused by Teslatlas Hub code or packaging.
