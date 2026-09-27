# Contributing

Bug reports, documentation fixes and focused code changes are welcome. Discuss
protocol, storage, authentication, migration, licensing or branding changes
before substantial implementation.

## Prepare a change

Use synthetic data. Do not submit credentials, VINs, precise journeys, private
logs or production databases. Describe the user-visible problem, keep the scope
small and preserve unrelated work. In the owner's workspace, follow its local
agent guidance and single-branch policy.

In that workspace, develop locally on the existing `main` checkout and leave routine
changes uncommitted. Identify tested edits by base commit plus dirty-file digest.
Commit and push only when the owner requests a source checkpoint or push.

Sign off each commit under DCO 1.1 with `git commit -s`. Non-trivial external
contributions require a signed individual or corporate copyright assignment
before merge. A maintainer arranges that privately when acceptance is likely;
executed agreements and identity records do not belong in GitHub.

Disclose consulted implementations and specifications with their revisions,
copied or adapted material and licences, generated assets and material AI use.
Disclose employment, client or confidentiality restrictions and any movement
from proprietary Teslatlas code. See the
[contributor agreement process](../docs/governance/contributor-agreement-process.md)
and [provenance policy](../docs/legal/provenance.md).

## Check the result

Choose tests for the changed behaviour and its failure paths. The
[source build guide](../docs/guides/build-from-source.md) explains standalone
Cargo checks and the limits of the current packaging helpers. In the maintained
workspace, use its existing command runner and build coordination.

For tracked documentation and provenance changes, these read-only checks apply:

```sh
python3 scripts/verify-repository-layout.py
python3 scripts/verify-provenance.py
```

Check relative links from each document's location. Documentation-only changes
do not need a build. Rust changes need formatting, relevant tests and Clippy;
platform, migration or packaging changes also need their ordinary user path and
recovery checks. Report commands actually run, the exact commit and unresolved
failures. Local results are not GitHub status checks.

Consider [Clean Development](https://github.com/magrathean-uk/clean-development)
for keeping supported build output and caches outside source trees.

## Submit

Use the pull request template to explain outcome, validation, risks and rights.
GitHub stores source; do not add CI, security automation, releases or binary
publication without the owner's explicit request. Existing tags are historical.

Report vulnerabilities privately through [SECURITY.md](SECURITY.md). Submission
does not guarantee acceptance. Read the [code of conduct](CODE_OF_CONDUCT.md).
