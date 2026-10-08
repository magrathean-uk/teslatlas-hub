# Teslatlas Compute

Shared Rust map kernel (raster kernel, raw-position cleaning), compiled into both the
App and the Hub. Follow `../AGENTS.md` and `../docs/development/MASTER_PLAN.md`; the plan
wins on process, model, scope and commit policy. Run commands through
`../scripts/dev/run.sh teslatlas-compute COMMAND...`.

- Both consumers pin this crate by commit (the App by git rev, the Hub by rev today and
  by workspace path after the move). Output-changing fixes bump `ALGORITHM_VERSION`.
- The crate moves into the `magrathean-uk/teslatlas-hub` Cargo workspace as the
  `compute/` package, keeping its Apache-2.0 licence, after an Authorisations row.
- No dependencies, no unsafe, no network code. Legal files are owner-controlled.
- Short local commit per reviewed batch; push only on an owner request.

<!-- clean-development-policy:v2 (canonical text: ~/dev/source/dev-bootstrap/snippets/clean-development-policy.md) -->
## Clean development (mandatory)

Follow the Clean Development rules in the workspace `../AGENTS.md`: routed caches, builds and scratch outside the tree, `clean-development run --session session-only` for anything that writes output, and `dev-env-check`, `dev-projects-check` and `dev-audit` before finishing.
