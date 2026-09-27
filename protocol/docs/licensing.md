# Licensing

The controlling licence for Teslatlas Protocol is the unmodified Apache License
2.0 in the repository root [`LICENSE`](../LICENSE). The project documents this
as `Apache-2.0`; this guide does not modify the grant or add terms.

The protocol contains source-neutral schemas, specifications, fixtures, and
independently authored examples. It does not include Teslatlas Hub
implementation or proprietary Teslatlas product source. Do not copy material
from those projects into this repository without confirming the relevant
rights and licence terms.

## Redistribution and notices

Redistributors must comply with Apache-2.0, including carrying the licence and
preserving applicable copyright, patent, trademark, and attribution notices.
Apache-2.0 does not grant trademark permission except as its terms allow.

[`THIRD-PARTY-NOTICES.md`](../THIRD-PARTY-NOTICES.md) is an informational
dependency list. The three entries added to close the previous inventory gap
were checked against the licence files in the exact installed distributions
and the matching tagged upstream sources:

- `cryptography 46.0.7`: `Apache-2.0 OR BSD-3-Clause`, with the tagged
  [licence selector](https://github.com/pyca/cryptography/blob/46.0.7/LICENSE).
- `cffi 2.1.1`: `MIT-0`, with the tagged
  [licence file](https://github.com/python-cffi/cffi/blob/v2.1.1/LICENSE).
- `pycparser 3.0`: `BSD-3-Clause`, with the tagged
  [licence file](https://github.com/eliben/pycparser/blob/release_v3.00/LICENSE).
- `zstandard 0.25.0`: `BSD-3-Clause`, checked against the `LICENSE` shipped in
  the exact installed distribution used by the local profile generator.

This closes the documentation inventory gap for the locked artifacts; it is
not legal clearance and does not establish the contents or obligations of a
different wheel, source distribution, build, or version. The table is not a
substitute for the licence files supplied with an artifact. Before
redistributing a dependency, inspect that exact artifact and preserve every
licence, notice, attribution, and other required material it ships.

## Contributions and ownership

No contributor agreement, copyright-owner register, dual-licence grant, or
commercial licensing programme was identified in the reviewed source. Do not
infer any of them from repository location, history, or the Teslatlas Hub
project. Obtain the necessary rights-holder decision before changing the
licence, adding custom terms, or making ownership claims.
