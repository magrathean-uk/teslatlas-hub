# Security Policy

## System and scope

Teslatlas Viewer is a browser reference client for public Teslatlas Hub APIs.
Fixture mode uses deterministic local examples. Live mode configures a Hub HTTPS
origin, expected Hub identity, optional TLS identity, and pairing invitation
through the packaged public SDK browser entrypoint.

This policy covers this repository's Viewer source, package scripts, static
server, and documentation. Hub services, SDK implementation, deployment
configuration, and third-party dependencies may need separate reports to their
respective owners.

## Trust boundaries and security properties

- The SDK adapter is the Viewer network boundary. Views must not construct or
  call private transport routes.
- Live endpoints must be root HTTPS origins. User info, paths, queries, and
  fragments are rejected.
- Pairing requires the configured Hub identity and checks a configured TLS
  identity against the invitation when one is provided.
- The Viewer keeps live credentials in memory. It must not persist pairing
  material or credentials in browser storage, cookies, service workers, URLs,
  logs, copied errors, support bundles, or screenshots.
- A local session clear removes Viewer-held state. It does not assert that a
  Hub device was remotely revoked.

## Reportable findings

Report a plausible issue that could expose pairing material or credentials,
escape the static server's asset root, bypass endpoint, Hub identity, or
TLS-identity checks, access Hub data without
the expected authorisation, or cause the Viewer to misrepresent the state or
freshness of live data. Include a minimal reproduction and the affected source
revision when safe to do so.

## Reporting

This repository-specific policy supplements the
[Magrathean UK security policy](https://github.com/magrathean-uk/.github/blob/main/SECURITY.md).
The organisation policy supplies the reporting, research-scope, safe-harbour,
excluded-activity, and disclosure terms that apply here unless this policy says
otherwise.

Report suspected vulnerabilities privately by emailing `contact@magrathean.uk`
with the subject `SECURITY: teslatlas-viewer`. Do not open a public issue,
discussion, or pull request for a suspected vulnerability. GitHub's Report a
vulnerability option may be used when it is enabled for this repository; this
policy does not assert that it is enabled.

Do not send live credentials, invitations, access tokens, private endpoint
details, personal vehicle data, or production database extracts. Redact
evidence and provide the minimum material needed for safe reproduction. Include
the affected component and revision, deployment context and permissions,
reproduction or a minimal proof of concept, expected impact, and a safe contact
route for follow-up.

The organisation policy describes good-faith research and its safe-harbour
position. It does not promise a bounty, payment, response time, remediation
time, or fixed service level.

## Version status

The current source is a compatibility candidate and Viewer development is
deferred. No maintained-release support window is declared here. Include the
exact commit or package identity in a report; do not infer security support
from a product version or a historical test receipt.

## Limitations

This policy records source-informed scope and expected properties. It is not a
security audit, a guarantee that a control has been independently verified, or
an exclusion of findings in connected components.
