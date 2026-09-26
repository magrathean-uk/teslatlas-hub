# GitHub repository settings

GitHub is source storage for Teslatlas Hub. Do not add build, test, release or
security automation without an explicit owner request. Current distribution is
source-only; existing tags are historical snapshots. See
[Source publishing and local builds](../releases/releasing.md).

## Repository presentation

The landing page is `.github/README.md`, also referenced by `Cargo.toml`.
Keep documentation under `docs/` and preserve `CITATION.cff` for citation metadata.
The layout gate forbids tracked root Markdown and tool-specific configuration.
Local agent guidance is separate from tracked public documentation.

A suitable About description is:

> Self-hosted Tesla telemetry collector and local Teslatlas sync hub, written in Rust.

Website: [teslatlas.eu](https://teslatlas.eu).
Suggested topics: `tesla`, `telemetry`, `self-hosted`, `rust`, `sqlite`, `teslamate`,
`macos`, `debian`.

## Review remote settings separately

Check the live repository before stating which controls are enabled. Source files
do not establish private advisory availability, branch protection, required reviews,
secret scanning, dependency alerts or merge settings. Do not advertise automated
checks when validation was performed locally.

`CODEOWNERS` identifies review ownership. The contribution and security policies
supply the rights process and private reporting route. Keep executed agreements,
signatures and identity records outside the public repository.

Changes to remote settings, tags or publication require a separate authorised
operation. Documentation changes do not apply them.
