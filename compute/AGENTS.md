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

<!-- clean-development-policy:v2 (canonical text: ~/dev/source/dev-bootstrap/snippets/clean-development-policy.md) -->
## Clean development (mandatory)

Follow the Clean Development rules in the workspace `../../AGENTS.md`: routed caches, builds and scratch outside the tree, `clean-development run --session session-only` for anything that writes output, and `dev-env-check`, `dev-projects-check` and `dev-audit` before finishing.
