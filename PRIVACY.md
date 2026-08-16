# Privacy and data-protection roles

## Local software

Teslatlas Hub is intended to run under the operator's control. The software can process highly sensitive practical data, including precise location, journeys, charging history, vehicle identifiers, account tokens, IP addresses, device identifiers and security logs.

The public source code does not by itself determine the legal role of every deployment.

## Self-hosting operator

A person or organisation that deploys the Hub and decides why and how personal data is processed will ordinarily be the controller, business or equivalent responsible party for that deployment. The operator must determine:

- lawful basis and, where required, consent;
- user and driver notices;
- data minimisation;
- access permissions;
- retention and deletion;
- rights handling;
- processor and subprocessor contracts;
- international transfers;
- incident response;
- whether a data-protection impact assessment is required;
- rules applying to employees, family members, passengers and fleet users.

A software licence is not a privacy notice for a deployment.

## Magrathean UK Ltd

Magrathean UK Ltd is not automatically controller or processor for data stored solely on an operator's machine. Magrathean becomes responsible for personal data it actually receives and processes for its own purposes, including data submitted through its website, email, support, security reporting, crash reporting if enabled, hosted infrastructure or commercial services.

The applicable Magrathean privacy notice is published at `https://magrathean.uk/privacy/`. Product-specific notices prevail for a product-specific processing activity.

## Default data-flow rules

Unless a feature and its notice expressly state otherwise, the Hub should:

- operate without a Magrathean account;
- avoid telemetry and advertising identifiers;
- keep vehicle data local;
- avoid uploading logs automatically;
- make outbound destinations visible and configurable;
- redact tokens, VINs, coordinates and user identifiers from routine logs;
- require an affirmative action before transmitting a diagnostic bundle;
- provide deletion, export and retention controls;
- fail closed where consent or credentials are absent.

Code and release documentation must be corrected if actual behaviour differs.

## Security reports and support submissions

Do not send live access tokens, passwords, refresh tokens, unredacted database dumps or full travel history in an issue. Use the private security channel in `SECURITY.md`.

Support material may be retained for the period needed to investigate and defend the matter, then deleted or anonymised in accordance with the applicable notice and legal obligations.

## Deployers

A deployment-ready notice and checklist are in `docs/DATA_PROTECTION_FOR_DEPLOYERS.md`. They are a starting point, not a substitute for a notice reflecting the actual deployment.
