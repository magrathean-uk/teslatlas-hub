# Contributing

Bug reports, documentation fixes and focused changes are welcome. Before you
start substantial work, discuss any change to the protocol, storage,
authentication, migration, licensing or branding.

## How changes are made

Only the maintainer changes the official repository. To propose a change, open
a pull request. The maintainer reviews it and may accept, amend or decline it.
See [Governance](../docs/governance/governance.md).

## Prepare a change

Use synthetic data. Never submit credentials, VINs, precise journeys, private
logs or production databases. Describe the problem you are solving, keep the
change small, and leave unrelated code alone.

In the owner's workspace, follow its agent guidance:

- work on the existing `main` checkout and leave routine changes uncommitted;
- identify tested edits by base commit plus dirty-file digest; and
- commit or push only when the owner asks.

## Check the result

Choose tests for the changed behaviour and its failure paths. The
[source build guide](../docs/guides/build-from-source.md) explains standalone
Cargo checks and the limits of the current packaging helpers. In the maintained
workspace, use its existing command runner and build coordination.

What each change needs:

- **Documentation-only changes** need no build. Check relative links from each
  document's location.
- **Rust changes** need formatting, the relevant tests and Clippy.
- **Platform, migration or packaging changes** also need their ordinary user
  path and recovery checks.

For tracked documentation and provenance changes, run these read-only checks:

```sh
python3 scripts/verify-repository-layout.py
python3 scripts/verify-provenance.py
```

Report the commands you actually ran, the exact commit and any unresolved
failures. Local results are not GitHub status checks.

[Clean Development](https://github.com/magrathean-uk/clean-development) can
keep build output and caches outside source trees.

## File headers

Every source file begins with `SPDX-License-Identifier: AGPL-3.0-only`. The
exception is the Tesla Auth adaptation, which keeps `MIT`. In third-party or
adapted files, keep the original notices and add a dated note of your
changes. See [Provenance](../docs/legal/provenance.md).

## Contributor terms

By opening a pull request, you agree to these terms for the material in it
(your **contribution**):

1. **Sign-off.** Sign off every commit under the
   [Developer Certificate of Origin 1.1](../docs/governance/developer-certificate-of-origin-1.1.md)
   (`git commit -s`), using your real name.
2. **Licence.** You license your contribution under AGPL-3.0-only, the Hub's
   licence. You also give MAGRATHEAN UK LTD (**Magrathean**) the right to
   license it on other terms.
3. **Assignment.** Before a substantial contribution is merged, Magrathean
   will ask you to sign an assignment agreement for
   [individuals](../docs/governance/individual-contributor-assignment-agreement.md)
   or [organisations](../docs/governance/corporate-contributor-assignment-agreement.md).
   Signed agreements are kept private.
4. **Your rights.** You may go on using your own contribution for any purpose.
   You are credited as its author in the Git history and the release notes.
5. **Issues and comments.** Only pull requests are contributions. Magrathean
   claims no rights in issues, comments or discussions. If the maintainer wants
   to use code posted there, you will be asked to submit it as a pull request.
6. **Disclosure.** In the pull request, identify anything you did not write
   yourself, with its source and licence, and any employer or client
   restriction.
7. **No obligation.** Magrathean need not accept or keep any contribution.

## Submit

Use the pull request template. GitHub holds the source only. Do not add CI,
security automation, releases or binary publication unless the owner asks.

Report vulnerabilities privately through [SECURITY.md](SECURITY.md). Read the
[code of conduct](CODE_OF_CONDUCT.md).
