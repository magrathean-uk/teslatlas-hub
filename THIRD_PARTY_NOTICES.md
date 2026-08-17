# Third-party notices

This repository uses third-party software and may include compatibility material. Every release must contain a machine-generated notice bundle based on the exact lockfile and artefacts distributed. This file is the policy and high-level notice; it is not a substitute for the generated inventory.

## TeslaMate compatibility material

Upstream: TeslaMate  
Repository: `https://github.com/teslamate-org/teslamate`  
Reviewed compatibility revision: `7054517c10475f39f480edeae8f90c6f717985a3`  
Licence: GNU Affero General Public License version 3  
Copyright: TeslaMate contributors and other applicable rightsholders

Teslatlas Hub includes optional compatibility logic informed by public TeslaMate source, schema, migrations and behaviour. See `PROVENANCE.md`. No affiliation or endorsement is claimed.

## Rust dependencies

The exact dependency set is fixed by `Cargo.lock`. A release must generate and publish:

- package name and version;
- source and checksum;
- declared licence expression;
- applicable licence text;
- required notice;
- dependency relationship;
- whether code is linked, bundled, vendored, build-only or test-only.

Dependencies with an unknown, unapproved, non-redistributable or source-unavailable licence must block release.

## Vendored material

A vendored dependency must retain its upstream licence, copyright and notices in its directory. Modification must be recorded. A package-manager declaration alone is not enough.

## Data and services

Map, geocoding, elevation, time-zone, certificate, API and other data providers may impose attribution, caching, rate, database-right or redistribution conditions separate from software copyright. A release or deployment must record the provider and applicable terms.

## No implied endorsement

Third-party names are used only for identification, attribution or compatibility. Their owners do not sponsor or endorse Teslatlas Hub.
