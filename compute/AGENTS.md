# Teslatlas Compute

Shared Rust map kernel (raster kernel, raw-position cleaning), compiled into both the
App and the Hub. Follow `../../AGENTS.md` and `../../docs/development/MASTER_PLAN.md`; the
plan wins on process, model, scope and commit policy. Run commands through
`../../scripts/dev/run.sh hub cargo COMMAND -p teslatlas-compute ...`.

- The Hub uses this crate as the `compute` path member of its Cargo workspace (root
  `Cargo.toml`). The App uses a vendored copy at a recorded commit
  (`../../app/vendor/teslatlas-compute/PROVENANCE.md`), so Hub work cannot change it.
  Output-changing fixes bump `ALGORITHM_VERSION`.
- The crate lives in the `magrathean-uk/teslatlas-hub` Cargo workspace as the
  `compute/` package and keeps its Apache-2.0 licence.
- No dependencies, no unsafe, no network code. Legal files are owner-controlled.
- Short local commit per reviewed batch; push only on an owner request.

<!-- clean-development-policy:v4 (clean-development 0.6.0; inherited from ../AGENTS.md) -->
## Clean development (mandatory)

Follow the [Hub clean-development policy](../AGENTS.md#clean-development-mandatory).
The Hub root `../.clean-development.json` (schema v2) governs this package. Cargo
output stays in the Hub's git-ignored `target/`; caches and scratch follow
`~/dev/machine/env.zsh`. Use the Hub runner shown above; the Hub's macOS app goes under `$TESLATLAS_LAB/build/hub/dist` and its Xcode
staging under `$TESLATLAS_DERIVED_DATA`.
