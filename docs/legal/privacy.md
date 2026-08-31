# Privacy and data-protection roles

## Self-hosted software

Teslatlas Hub is intended to run under the operator's control. It can process precise location, journeys, charging, vehicle identifiers, account tokens, network information and security logs.

The public source code does not make MAGRATHEAN UK LTD controller or processor for data that never reaches Magrathean.

## Operator responsibility

A person or organisation deciding why and how deployment data is processed ordinarily acts as controller or equivalent responsible party. The operator must address:

- lawful basis and notices;
- drivers, employees, family and passengers;
- access control and least privilege;
- retention, export and deletion;
- processor contracts and transfers;
- incident response and rights requests;
- employee monitoring and DPIA requirements.

## Magrathean processing

MAGRATHEAN UK LTD is responsible for personal data it actually receives for its own purposes, including support, website, security reports, commercial services or hosted infrastructure.

Before submitting diagnostics, support material, security reports, or other
personal data, read the live [Magrathean privacy notice](https://teslatlas.eu/privacy/).
That notice must describe the actual flow.

Before an official beta is published, the external notice must be verified as
consistent with this repository's disclosed support, geocoder, and terrain flows.
Any contradiction or missing disclosure on `teslatlas.eu` is an external
publication blocker; changing this repository does not update that website.

## Optional geocoder-provider disclosure

Geocoding is disabled by default and has no public default provider. Enabling
it requires an operator-supplied HTTPS endpoint. For each uncached lookup, Hub
sends the vehicle's precise latitude and longitude, zoom level 19, requested
language, Hub user agent, operator IP address, and ordinary request metadata to
that provider. A successful response can contain a precise street address and
provider identifiers; Hub caches that response with the lookup coordinate in
the operator-controlled database.

The operator selects the recipient and is responsible for its privacy,
retention, transfer, attribution, database-right, and rate-limit terms. Use a
self-hosted or otherwise authorised Nominatim-compatible service; do not assume
the public OpenStreetMap Foundation Nominatim service permits vehicle tracking,
bulk enrichment, or submission of personal data. Disable all geocoder egress
with `geocoder.enabled = false`.

## Optional terrain-provider disclosure

When terrain enrichment is enabled and a required tile is not cached, Hub sends
an HTTPS request to AWS
`https://elevation-tiles-prod.s3.amazonaws.com/`, then falls back to the
ESA-hosted SRTMGL1 endpoint
`https://step.esa.int/auxdata/dem/SRTMGL1/` if needed. The URL identifies the
one-degree latitude/longitude tile containing the vehicle location. The
providers therefore receive the operator IP address, request metadata, and an
approximate one-degree location tile; Hub does not place the precise coordinate
in the request URL. This is SRTMGL1 elevation data, not ESA WorldCover.

Disable this egress with `terrain.enabled = false`. Source builds and packaged
macOS and Debian configurations default to terrain disabled. Existing cached
terrain remains operator-controlled local data.

## Default product expectations

Hub should, unless clearly disclosed otherwise:

- function without a Magrathean account;
- avoid advertising and analytics identifiers;
- keep vehicle history under operator control;
- redact credentials and precise location from ordinary logs;
- require affirmative action before a diagnostic transfer;
- expose outbound destinations;
- provide retention and deletion controls.

Correct documentation if code differs.

Deployers processing telemetry for another person or organisation should also
use the [data-protection notice and checklist](../operations/data-protection-for-deployers.md).
