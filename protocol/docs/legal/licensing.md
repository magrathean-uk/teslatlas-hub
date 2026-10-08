# Licensing

Teslatlas Protocol is licensed under the Apache License, Version 2.0.

The controlling licence text is the unmodified Apache License 2.0 in the
repository root [`LICENSE`](../../LICENSE). Copyright and attribution are
recorded in [`NOTICE`](../../NOTICE); MAGRATHEAN UK LTD holds the copyright.
See the [company's legal page](https://github.com/magrathean-uk/.github/blob/main/LEGAL.md)
for the licensing, trade mark and contribution terms that apply across
Magrathean repositories.

The protocol contains source-neutral schemas, specifications, fixtures, and
independently authored examples. It does not include Teslatlas Hub
implementation or proprietary Teslatlas product source. Do not copy material
from those projects into this repository without confirming the relevant
rights and licence terms.

## Redistribution and notices

Redistributors must comply with Apache-2.0, including carrying the licence and
the `NOTICE` file and preserving the applicable copyright, patent, trade mark
and attribution notices. Apache-2.0 grants no trade mark permission beyond what
its terms allow.

[`third-party-notices.md`](third-party-notices.md) lists the Python packages
locked in `uv.lock` for the local tooling. The following licences were checked
against the licence files in the exact installed distributions and, where
linked, the matching tagged upstream sources:

- `cryptography 50.0.1`: `Apache-2.0 OR BSD-3-Clause`, with the tagged
  [licence selector](https://github.com/pyca/cryptography/blob/50.0.1/LICENSE).
- `cffi 2.1.1`: `MIT-0`, with the tagged
  [licence file](https://github.com/python-cffi/cffi/blob/v2.1.1/LICENSE).
- `pycparser 3.0`: `BSD-3-Clause`, with the tagged
  [licence file](https://github.com/eliben/pycparser/blob/release_v3.00/LICENSE).
- `zstandard 0.25.0`: `BSD-3-Clause`, checked against the `LICENSE` shipped in
  the exact installed distribution used by the local profile generator.

These checks cover only the locked versions. A different wheel, source
distribution, build or version can carry different terms. Before
redistributing a dependency, inspect that exact artefact and preserve every
licence, notice, attribution and other required material it ships.

## Contributions

Public repositories under a permissive licence accept pull requests under the
terms in [CONTRIBUTING](../../.github/CONTRIBUTING.md). Do not copy Hub
implementation code or proprietary application code into this repository.
Record the origin and licence of any third-party material you propose to add.
