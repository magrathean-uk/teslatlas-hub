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
macOS 12 deployment target. No Tesla command private key, TLS private key,
OAuth token, or session cache is included in the application or package.

The upstream `LICENSE` file applies to this component. Its Go module graph is
fixed by the upstream `go.mod` and `go.sum`; the exact source revision and
build inputs are recorded in `PROVENANCE.md`.

## Data and services

Map, elevation, geocoding, weather, timezone, certificate and API providers may impose attribution, caching, database-right and rate conditions independent of software copyright.

Record the actual provider and terms for each release/deployment.
