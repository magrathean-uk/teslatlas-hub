# teslatlas-hub

Rust crate at this directory (`Cargo.toml`). `#![forbid(unsafe_code)]`.

## Current programme and model policy

Use `../docs/development/MASTER_PLAN.md`, the shared `PRODUCT_SPEC.md`, and
`docs/development/PLAN.md`. The owner requires a working bootstrap first;
polished installers and cross-platform acceptance come last. Product development is now authorized under
`../docs/development/START_AUTHORIZATION.md`; activate the saved full-plan goal.

The owner selected **GPT-5.6 Terra with XHigh reasoning** for Hub development.
Companions use GPT-5.6 Luna/Max. Follow the current coordination/resource policy;
older model recommendations and archived holds are superseded. Do not change global model settings.

```bash
cargo check
cargo test
cargo clippy --all-targets
```

Graph project: `Users-bolyki-dev-source-teslatlas-service-hub`. Skills: `teslatlas-cargo`, and `teslatlas-core-nav` / `teslatlas-ffi` when the change crosses into the app core.

Do not search `../upstream/`, `../repository-roots/`, or `../app/vendor/`.

## Release versions

Distribution is source-only. Do not create GitHub Releases or upload prebuilt
assets unless explicitly requested. Ordinary fixes need no new release or tag.
Preserve existing source tags; users build their own packages.

Future Hub releases use `YEAR.WEEK.REVISION` calendar versions, starting with
the agreed `2026.36.1` (`v2026.36.1` tag), not a new `1.x` version. Follow
[the versioning policy](docs/releases/versioning.md) for ISO week-year rollover,
revision allocation, platform metadata, and immutable tags. Preserve historical
`v1.0.0` and prerelease tags. A documented version is not a published release.

## Local execution

Run task-relevant disposable local checks and repair failures without repeated approval when the lane is open. Existing owner pauses, workspace authority, production and release gates remain in force.

