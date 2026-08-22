# Provenance and independent-development record

## Purpose

This file records sources and treatment. It is **not** a blanket clean-room declaration.

## Original implementation

Teslatlas Hub is a Rust self-hosted collector, local store, sync service, CLI and platform controller maintained by MAGRATHEAN UK LTD.

## TeslaMate reference material

Compatibility work consulted public TeslaMate material, including source, schema, migrations, documentation, fixtures and observable behaviour.

Reviewed compatibility revision:

`7054517c10475f39f480edeae8f90c6f717985a3`

The repository contains TeslaMate-specific facts, names, schema mappings, fingerprints, behavioural compatibility and fixtures. It must not claim that no TeslaMate material or influence exists.

## Tesla Auth reference material

The native macOS OAuth onboarding flow adapts the endpoint constants, PKCE
authorization shape, `tesla://auth/callback` handling, China issuer routing,
and no-redirect 30-second token exchange from Tesla Auth `v0.15.0`, revision
`68da1f850e9cb87ac0e54c608d5a2e90d3ad1608` (MIT, © 2021 Adrian Kumpf).
The Wry/Tao GUI and Rust dependency graph are not bundled; macOS uses native
WebKit, CryptoKit, Security, and URLSession.

## File classification

Every release file must be classified as:

- `MAGRATHEAN-ORIGINAL`
- `COMPANY-ASSIGNED-CONTRIBUTION`
- `TESLAMATE-COMPATIBILITY`
- `THIRD-PARTY`
- `GENERATED`
- `DATA-OR-FACTS`
- `UNKNOWN`

`UNKNOWN` blocks release.

## Protectable expression

A file containing copied, adapted or closely translated protectable TeslaMate expression must preserve applicable upstream rights and notices and remain under a compatible licence.

Facts, methods, protocols and interfaces are assessed separately from expression. Compatibility alone is not a legal conclusion either way.

## Automated scans

Exact-blob, shared-string and similarity scans are evidence tools only.

- An empty result does not prove independent creation.
- An unavailable repository is not a passed scan.
- A non-match does not resolve non-literal copying.
- A match does not prove infringement.

Record tool version, inputs, hashes, exclusions and adjudication.

## High-priority paths

Review all `teslamate*`, legacy authentication, Owner API, streaming and TeslaMate fixture paths before each release.

## Proprietary app boundary

No Hub implementation source may move into the proprietary app unless MAGRATHEAN UK LTD owns every relevant right or has a separate licence from every rightholder.

Shared protocol facts must be maintained separately from covered implementation.

## Baseline

Reviewed Hub `main`: `a2b8431028abb8d84465196fceb0c951de901cee`.
