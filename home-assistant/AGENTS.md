# Teslatlas Home Assistant

This repository owns `custom_components/teslatlas_hub`, a read-only Home Assistant integration for the public Hub HTTP profile. Keep changes within the assigned scope.

## Boundaries

- Use the embedded `hub-http-v1@1.0.0` profile. Do not invent routes, add SSE or commands, read Hub storage or collector internals, or request Tesla credentials.
- Preserve one non-overlapping polling authority per entry, the four-read concurrency bound, strict certificate validation, identity checks before sending saved credentials, credential persistence order and diagnostics redaction.
- Preserve stable device/entity IDs and distinguish missing data from zero, outage and vehicle removal.
- Preserve existing work and the independent `main` checkout. Do not create branches, worktrees or stashes in the owner's coordinated workspace. Commits follow the current task's authority; pushes require an explicit instruction.
- GitHub is source storage only. Do not add CI, release, artifact-upload, Dependabot or HACS automation. Publication, public ingress and real-vehicle work require explicit task authority.
- Keep credentials, locations, private fixture paths and local account details out of Git and reports.

## Checks

See [CONTRIBUTING.md](CONTRIBUTING.md) for the pinned environment and format scope. With that environment already prepared, run the checks that cover the changed behavior. These are standalone command forms; apply the parent execution rules when working in the coordinated workspace:

```sh
uv run --locked pytest tests/test_config_flow.py tests/test_coordinator.py tests/test_diagnostics.py
uv run --locked ruff check .
```

Client changes also need `tests/integration/test_current_hub_client.py`; entity changes need `tests/test_sensor.py`; setup/unload or migration changes need `tests/test_init.py` and `tests/test_migration.py`. Include `tests/test_documentation.py` for documentation changes. Complete the relevant checks and repair failures within the authorized scope.

Use `codebase-memory-mcp` for indexed code navigation when available, and confirm findings in source. Use targeted `rg` reads when it is unavailable; do not recreate CodeGraph tooling.

## Evidence and coordination

Keep local unit tests, synthetic fixtures, installed UI behavior and real-Hub acceptance distinct. Record the exact source, invocation and result. Do not turn an old receipt into a current runtime claim.

When this checkout is inside the owner's multi-repository workspace, follow the applicable parent instructions and current master plan. Local `docs/development/PLAN.md`, `STATUS.json` and old handoffs are historical evidence, not independent permission to resume a runtime. The current task controls allowed actions and output locations.

Read [architecture](docs/architecture.md), [protocol binding](docs/protocol-readiness.md) and [security](SECURITY.md) when touching those boundaries. Use a bounded independent worker only when its benefit exceeds the overhead; give it fresh context, a distinct scope and a clear stop condition.
