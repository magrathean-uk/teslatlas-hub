# SPDX and source-header policy

## Magrathean-original Hub files

`MAGRATHEAN-ORIGINAL` in `provenance-manifest.json` records current repository
treatment. It does not itself establish who authored a file or whether rights
were assigned. Apply a Company copyright line only where the release signer's
separate title review supports it.

Use:

```text
SPDX-FileCopyrightText: 2026 VERIFIED RIGHTSHOLDER NAME
SPDX-License-Identifier: AGPL-3.0-only
```

Replace the placeholder with the actual verified file rightsholder. Do not add
a Company copyright line until the separate title review supports it.

Every Rust and Swift source file and every project build, packaging, evidence,
or test shell/Python script carries at least its verified
`SPDX-License-Identifier`. A copyright line remains conditional on title
evidence. The separately classified Tesla Auth adaptation retains `MIT`; the
other current implementation and tooling files use `AGPL-3.0-only`.

Where required, add:

```text
SPDX-FileContributor: György Bolyki
```

## TeslaMate-derived or adapted files

Do not overwrite upstream ownership. Preserve applicable upstream copyright and licence notices and add a clear modification notice with date and upstream revision.

## Third-party files

Retain the upstream SPDX identifier and notices. Do not relabel third-party material as Magrathean-owned.

## Generated files

Identify the generator, source input and governing licence. Do not hand-edit generated files unless provenance records the divergence.

Binary or historical generated material whose editable source or generator is
not tracked must have an explicit manifest exception. That exception must say
what is missing and what the release signer must verify; classification does
not cure missing chain-of-title evidence.

## Classification gate

Every tracked file must match exactly one rule in
`provenance-manifest.json`. Third-party, generated and data/fact rules require
origin, licensing and release-treatment metadata. `UNKNOWN`, missing coverage
and overlapping rules block release. Run `scripts/test-provenance.sh` after
changing the verifier or schema.
