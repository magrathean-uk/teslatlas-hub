# Third-party notices

Every release must include a generated notice bundle based on its exact lockfile and distributed artefacts. This policy is not a substitute for that inventory.

## TeslaMate compatibility

Upstream: https://github.com/teslamate-org/teslamate  
Reviewed revision: `7054517c10475f39f480edeae8f90c6f717985a3`  
Licence: GNU Affero General Public License version 3  
Copyright: applicable TeslaMate contributors and rightsholders

Teslatlas Hub includes compatibility logic informed by public TeslaMate source, schema, migrations and behaviour. No affiliation or endorsement is claimed.

## Tesla Auth OAuth flow

Upstream: https://github.com/adriankumpf/tesla_auth

Reviewed release: `v0.15.0`

Reviewed revision: `68da1f850e9cb87ac0e54c608d5a2e90d3ad1608`

Licence: MIT

Copyright: © 2021 Adrian Kumpf

The native macOS onboarding flow adapts the public OAuth endpoint, PKCE,
callback, issuer-routing, and token-exchange behaviour from Tesla Auth. The
upstream GUI/runtime is not bundled. The MIT licence text is included in the
release notice bundle.

> MIT License
>
> Copyright (c) 2021 Adrian Kumpf
>
> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in
> all copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
> SOFTWARE.

## Dependencies

For every dependency record:

- name and version;
- source and checksum;
- licence expression and text;
- copyright/NOTICE requirements;
- link/bundle/vendor/build/test status;
- modifications;
- source-offer duties;
- transitive relationship.

Unknown or incompatible licensing blocks release.

## Tesla Vehicle Command SDK and `tesla-http-proxy`

Upstream: https://github.com/teslamotors/vehicle-command
Reviewed release: `v0.4.1`
Reviewed revision: `49977a18fd68567501d59e16a6c9e4a8b9348544`
Licence: Apache License 2.0

The macOS service includes the upstream `cmd/tesla-http-proxy` executable as a
separate process. It is built from the pinned upstream source for arm64 with a
macOS 13 deployment target. A tracked patch adds the reviewed Go 1.27 runtime
default and a dated Apache change notice to the private build copy of `go.mod`;
the patch and original module sources are included in release evidence. No Tesla command private key, TLS private key,
OAuth token, or session cache is included in the application or package.

The upstream `LICENSE` file applies to this component. Its Go module graph is
fixed by the upstream `go.mod` and `go.sum`; the exact source revision and
build inputs, overlay checksum, and modified-file checksum are recorded in
`docs/legal/provenance.md`.

## Tesla Fleet Telemetry receiver

Upstream: https://github.com/teslamotors/fleet-telemetry

Reviewed release: `v0.9.4`

Reviewed revision: `d64c73ab65e7c5fb5fc12b35fe507e2c6054227b`

Licence: Apache License 2.0

The macOS service package and optional Debian Fleet package include a separately
executed receiver built from the pinned upstream source with a Teslatlas patch. The patch adds a strict
loopback HTTP dispatcher: decoded vehicle and connectivity records are sent to
Hub with a private bearer, and reliable vehicle-record acknowledgement waits
for Hub's successful commit. The packaged runtime configuration selects no
message-queue dispatcher; the CGO-only Kafka and ZMQ integrations are
unavailable in this build.

The upstream `LICENSE` file applies to this modified component. Its source
revision, archive checksum, patch checksum, build targets, and Go toolchain are
recorded in `docs/legal/provenance.md` and the checked-in bridge lock file. No receiver
TLS private key, vehicle credential, Fleet token, or loopback bearer is bundled.

## Data and services

### Optional terrain data

Teslatlas Hub does not bundle terrain tiles. When terrain enrichment is enabled,
a cache miss first retrieves a `skadi` HGT tile from the
[Mapzen Terrain Tiles dataset on AWS](https://registry.opendata.aws/terrain-tiles/)
and may fall back to an SRTMGL1 HGT tile hosted by
[ESA STEP](https://step.esa.int/auxdata/dem/SRTMGL1/).

The AWS registry identifies `elevation-tiles-prod` as Mapzen Terrain Tiles and
points to the upstream [Tilezen attribution requirements](https://github.com/tilezen/joerd/blob/master/docs/attribution.md).
Deployments that enable this provider must retain **Mapzen** credit and the
source credits applicable to the requested tiles. The upstream attribution
inventory covers ArcticDEM/DigitalGlobe and NSF awards; Geoscience Australia;
Austria's DGM; Canadian government elevation data; Copernicus EU-DEM; NOAA
ETOPO1; Mexico INEGI; Land Information New Zealand; Norway Kartverket; UK
Environment Agency terrain; and USGS 3DEP, GMTED2010, and SRTM. Consult the
linked upstream notice for its exact current wording and conditions rather than
treating this summary as a substitute.

For SRTM-derived elevation, retain: **SRTM data courtesy of the U.S. Geological
Survey.** The underlying dataset is NASA Shuttle Radar Topography Mission
Global 1 arc second version 3, produced by NASA JPL and distributed through the
USGS/NASA LP DAAC; dataset DOI:
[`10.5067/MEaSUREs/SRTM/SRTMGL1.003`](https://doi.org/10.5067/MEaSUREs/SRTM/SRTMGL1.003).
ESA is identified only as the fallback file host, not as the SRTM data owner.
Use of that host remains subject to ESA's current website/service terms.

Packaged macOS and Debian configurations disable terrain egress until the
operator enables it. See `docs/legal/privacy.md` and `docs/guides/configuration.md` for the
location disclosure and disable control.

Hub supports operator-selected Nominatim-compatible reverse-geocoding services
but does not configure a public default provider. If a deployment uses
OpenStreetMap-derived data, retain the attribution and ODbL/database-right
notices required by that data source in every consuming interface. The public
OpenStreetMap Foundation Nominatim service has a separate usage policy,
including personal-data, bulk/systematic-query, vehicle-tracking, caching, and
attribution constraints; compatibility with its protocol is not permission to
use that public endpoint.

Other map, geocoding, weather, timezone, certificate, and API providers may
impose attribution, caching, database-right, privacy, and rate conditions
independent of software copyright. Record the actual provider and current terms
for each release and deployment.
