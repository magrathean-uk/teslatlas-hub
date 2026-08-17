# Provenance and independent-development record

## Purpose

This file records what the project is, what was consulted and how third-party compatibility material is handled. It is not a blanket clean-room declaration.

## Original project

Teslatlas Hub is a Rust implementation of a self-hosted vehicle telemetry collector, local store, synchronisation service, CLI and platform bootstrap. Its architecture, Hub protocol, immutable-pack design, local application integration and Rust implementation are maintained by Magrathean UK Ltd.

## TeslaMate reference material

Development of the optional TeslaMate migration and behavioural-compatibility layer used public TeslaMate material, including:

- source code;
- PostgreSQL table, column, enum and relation definitions;
- migration identifiers and ordering;
- public documentation;
- observable runtime and API behaviour;
- authentication, retry, streaming and collection behaviour;
- public fixtures and expected data shapes.

The compatibility contract is pinned in source to TeslaMate revision:

`7054517c10475f39f480edeae8f90c6f717985a3`

The project currently contains exact source-schema facts, a migration-set fingerprint, TeslaMate-specific names and constants, and compatibility fixtures. It must therefore not state that no TeslaMate material is distributed.

## Legal treatment

TeslaMate is distributed under GNU Affero General Public License version 3. To reduce uncertainty, files containing copied, adapted or closely translated TeslaMate protectable expression must:

1. remain under GNU AGPL version 3 only;
2. preserve applicable upstream copyright and licence notices;
3. identify the upstream path and revision where reasonably possible;
4. state that the file has been modified or reimplemented in Rust and give the relevant date;
5. avoid claiming exclusive Magrathean ownership over upstream material.

Facts, data formats, methods of operation and interfaces are recorded separately from copied expression. Compatibility alone does not establish derivation; provenance evidence must decide the treatment of each file.

## Required file classifications

Every release file must be classified as one of:

- **MAGRATHEAN-ORIGINAL** — original Magrathean-owned expression;
- **CONTRIBUTOR** — third-party contribution under the project contribution terms;
- **TESLAMATE-COMPATIBILITY** — informed by or derived from TeslaMate public material;
- **THIRD-PARTY** — copied or vendored third-party work under its own licence;
- **GENERATED** — reproducibly generated from identified source;
- **DATA-OR-FACTS** — unprotectable facts or operator-provided data, with source recorded;
- **UNKNOWN** — prohibited from release until resolved.

## Current high-priority compatibility paths

The following paths require file-level review before the next stable release:

- `src/legacy_auth.rs`
- `src/owner_api.rs`
- `src/tesla_stream.rs`
- `src/teslamate.rs`
- `src/teslamate_credentials.rs`
- `src/teslamate_direct.rs`
- `src/teslamate_fragments.rs`
- `src/teslamate_import.rs`
- `src/teslamate_parity.rs`
- `src/teslamate_projection.rs`
- `src/teslamate_projection_state.rs`
- `src/teslamate_reader.rs`
- `src/teslamate_schema.rs`
- `src/teslamate_stage.rs`
- `src/teslamate_token.rs`
- `fixtures/teslamate-corpus/**`

This list is conservative and not exhaustive.

## Proprietary app boundary

The separate proprietary Teslatlas app may share Magrathean-owned protocol specifications or code only under a separate Magrathean licence. Upstream or community-owned GNU AGPL code must not be copied into that repository without a separate valid permission.

A signed CLA is required before a community contribution may be considered for dual licensing or reuse in a proprietary Magrathean product.

## Evidence retained privately

For every release, Magrathean should retain:

- source snapshots and commit hashes consulted;
- design records and issue history;
- dated authorship records;
- similarity reports;
- third-party licence texts;
- contributor agreements;
- build provenance, SBOM and checksums;
- release source archives;
- correspondence granting additional permission.

Do not publish personal signatures or confidential assignment documents.
