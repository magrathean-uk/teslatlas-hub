# Teslatlas Home Assistant

This repository owns `custom_components/teslatlas_hub`, a read-only Home Assistant
integration for the public Hub HTTP profile. Follow `../AGENTS.md` and
`../docs/development/MASTER_PLAN.md`; the plan wins on process, model, scope and commit
policy. Parked until the Hub is ready (owner). It is the reference third-party consumer
of the Hub read API and must keep working at every Hub API change; the Hub side checks
it in plan group G2-9. Carry forward the audit findings SD-1, SD-2, SD-4 and C-1 to C-4
when development resumes.

## Boundaries

- Use the Hub HTTP profile the Hub serves. Do not invent routes, add SSE or commands,
  read Hub storage or collector internals, or request Tesla credentials.
- Preserve one non-overlapping polling authority per entry, the four-read concurrency
  bound, strict certificate validation, identity checks before sending saved
  credentials, credential persistence order and diagnostics redaction.
- Preserve stable device and entity IDs and distinguish missing data from zero, outage
  and vehicle removal.
- Preserve existing work and the independent `main` checkout; no branches, worktrees or
  stashes. Short local commit per reviewed batch; push only on an owner request.
- GitHub is source storage only: no CI, release, artifact-upload, Dependabot or HACS
  automation. Publication needs an Authorisations row. No vehicle commands; no public
  ingress operated by Magrathean.
- Keep credentials, locations, private fixture paths and local account details out of
  Git and reports.
- Legal files (`LICENSE`, `NOTICE`, `docs/legal/`, contributor terms, copyright and
  attribution strings) are owner-controlled: change them only on the owner's explicit
  instruction.

## Checks

See [CONTRIBUTING.md](.github/CONTRIBUTING.md) for the pinned environment. Run the
checks that cover the changed behaviour through `../scripts/dev/run.sh teslatlas-home-assistant ...`:

```sh
uv run --locked pytest tests/test_config_flow.py tests/test_coordinator.py tests/test_diagnostics.py
uv run --locked --no-default-groups --group floor pytest
uv run --locked ruff check .
```

Client changes also need `tests/integration/test_current_hub_client.py`; entity changes
`tests/test_sensor.py`; setup, unload or migration changes `tests/test_init.py` and
`tests/test_migration.py`; documentation changes `tests/test_documentation.py`.

Keep local unit tests, synthetic fixtures, installed UI behaviour and real-Hub acceptance
distinct; record repo@sha, command and result. Read [architecture](docs/architecture/overview.md),
[protocol binding](docs/reference/protocol-readiness.md) and [security](.github/SECURITY.md)
when touching those boundaries.

<!-- clean-development-policy:v2 (canonical text: ~/dev/source/dev-bootstrap/snippets/clean-development-policy.md) -->
## Clean development (mandatory)

Follow the Clean Development rules in the workspace `../AGENTS.md`: routed caches, builds and scratch outside the tree, `clean-development run --session session-only` for anything that writes output, and `dev-env-check`, `dev-projects-check` and `dev-audit` before finishing.
