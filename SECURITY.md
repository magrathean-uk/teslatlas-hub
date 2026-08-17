# Security policy

## Supported versions

Security fixes are provided only for release lines expressly marked as supported. Alpha and development branches may change without compatibility support. The current support matrix must be stated in each release.

## Private reporting

Report a suspected vulnerability to:

GitHub private vulnerability reporting, or `contact@magrathean.uk` with subject `SECURITY`

Include:

- affected version and commit;
- operating system and architecture;
- deployment topology;
- reproduction steps;
- impact and realistic attack preconditions;
- logs or proof with secrets removed;
- whether the issue has been disclosed elsewhere;
- a safe contact method.

Do not open a public issue for an undisclosed vulnerability.

## Do not include

Never send live passwords, refresh tokens, access tokens, signing keys, unredacted VINs, precise travel history, private certificates or a production database unless Magrathean has expressly requested a secure transfer method.

## Research safe harbour

Magrathean will not pursue a good-faith researcher solely for testing that:

- targets only a Magrathean-controlled test instance or an instance the researcher owns or has explicit authority to test;
- avoids personal data beyond the minimum proof;
- avoids persistence, destructive changes, denial of service and material service degradation;
- avoids social engineering and physical intrusion;
- stops when sensitive data or unsafe vehicle behaviour could be affected;
- reports promptly and allows reasonable remediation time;
- does not demand payment as a condition of withholding harmful disclosure.

This statement does **not** authorise testing of Tesla, TeslaMate, Apple, GitHub, a vehicle, another user's deployment, an app-store service, a cloud provider or any other third-party system. Magrathean cannot waive another person's rights or terms.

## Excluded activity

The safe harbour does not cover:

- sending vehicle commands;
- attempting to wake, move, unlock or control a vehicle;
- credential stuffing, phishing or interception;
- accessing another person's telemetry;
- persistence, ransomware, destructive payloads or supply-chain compromise;
- large-scale automated scanning;
- denial of service;
- public release of secrets or exploit code before remediation;
- conduct prohibited by applicable criminal law.

## Handling

Receipt will be acknowledged where practicable. Triage priority depends on exploitability, affected data, default exposure and supported versions. Magrathean may request a CVE, coordinate disclosure and credit a reporter who asks to be credited.

No bounty or reward is promised unless agreed in writing before payment.

## Release integrity

Official releases should include signed checksums, an SBOM, source archive, dependency notices and reproducible build instructions. Verify release artefacts before installation.
