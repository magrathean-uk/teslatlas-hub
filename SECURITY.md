# Security policy

## Supported versions

| Release line | Security support |
|---|---|
| v1.0.0-beta.1 | Supported only after the complete official beta release is published |
| v1.0.0-alpha.1 and earlier | Unsupported; no supported upgrade exists until the official beta is published |
| `main` and untagged builds | Reports accepted, but not a supported release channel |

## Private reporting

Report vulnerabilities through
[GitHub private vulnerability reporting](https://github.com/magrathean-uk/teslatlas-hub/security/advisories/new)
or email `contact@magrathean.uk` with subject `SECURITY`.

Do not publish credentials, VINs, precise travel history, database dumps, signing keys or exploit details.

Include affected version/commit, platform, topology, reproduction, impact, preconditions and redacted evidence.

## Response and coordinated disclosure

Magrathean will acknowledge a usable private report, identify a contact for
follow-up, assess scope and severity, and provide material status changes while
remediation is active. Response time depends on impact, reproducibility, and
vehicle or credential risk; these expectations are not a paid-support SLA.

Keep exploit details private while a fix or mitigation is being prepared.
Reporter and maintainer should agree a reasonable disclosure date based on the
risk and deployment path. Magrathean will not require indefinite silence, and
will publish a security advisory or release note when affected users need to
act. Credit is offered when requested and consented to; no bounty is promised.
If active exploitation or immediate safety risk is suspected, say so in the
first line of the report.

## Safe harbour

MAGRATHEAN UK LTD will not pursue a good-faith researcher solely for authorised testing that:

- targets the researcher's own deployment or a Magrathean-controlled test system;
- avoids persistence, destructive changes, denial of service and personal data;
- stops when vehicle safety, credentials or third-party systems could be affected;
- reports promptly and permits reasonable remediation time; and
- does not condition non-disclosure on payment.

This does not authorise testing of Tesla, TeslaMate, Apple, GitHub, a vehicle, another user's system or another provider.

## Excluded conduct

No safe harbour covers vehicle commands, phishing, credential stuffing, access to another person's telemetry, destructive payloads, large-scale scanning, denial of service or unlawful conduct.

## Release integrity

Official releases should provide signed checksums, exact source, SBOM, dependency notices and verification instructions.
