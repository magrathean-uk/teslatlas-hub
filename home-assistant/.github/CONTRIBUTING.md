# Contributing

How to propose and validate a change to the read-only Hub integration.

Keep changes focused on the public Hub integration. Report a reproducible bug through the [issue tracker](https://github.com/magrathean-uk/teslatlas-home-assistant/issues); use [SECURITY.md](SECURITY.md) for sensitive reports. Explain the user-visible behavior and the checks that establish it.

## Local environment

[pyproject.toml](../pyproject.toml) requires Python `>=3.14.2` and uses `uv`. The locked development environment uses Home Assistant `2026.9.4`, `pytest-homeassistant-custom-component` `0.13.367` and Ruff `0.16.9`. For a standalone checkout, run these from the repository root. In the coordinated Teslatlas workspace, use its current parent execution rules for these commands:

```sh
uv sync --locked --group dev
uv run --locked pytest
uv run --locked pytest --cov=custom_components/teslatlas_hub --cov-report=term-missing
uv run --locked ruff check .
```

These are contributor commands, not a record that they passed on your machine. If working in the coordinated Teslatlas workspace, follow its current environment and execution rules.

Consider [Clean Development](https://github.com/magrathean-uk/clean-development) for managing development caches and supported build output.

## Format scope

The documented product-owned format check is:

```sh
uv run --locked ruff format --check custom_components tests/test_client.py tests/test_config_flow.py tests/test_coordinator.py tests/test_diagnostics.py tests/test_documentation.py tests/test_init.py tests/test_migration.py tests/test_models.py tests/test_package.py tests/test_sensor.py tests/helpers.py tests/conftest.py tests/integration/test_current_hub_client.py
```

The retained matrix harness has a separate historical format exception. This command does not establish full-tree formatting; do not reformat unrelated tooling as part of an integration change.

## Choose checks for the change

| Change | Relevant tests |
| --- | --- |
| Pairing, identity or reconfiguration | `tests/test_config_flow.py` |
| HTTP validation, TLS or credential rotation | `tests/integration/test_current_hub_client.py`, `tests/test_coordinator.py` |
| Entity values and dynamic vehicles | `tests/test_sensor.py`, `tests/test_models.py` |
| Setup, unload or schema migration | `tests/test_init.py`, `tests/test_migration.py` |
| Redaction | `tests/test_diagnostics.py` |
| Documentation and package layout | `tests/test_documentation.py`, `tests/test_package.py` |

Fixtures must be synthetic and redacted. Live tools such as `tools/test-live-hub` require private fixture, CA, synchronization and receipt inputs; they are not ordinary unit tests and must not be run against an unapproved runtime. Read their source and the current task scope before using them.

## Review expectations

Describe what changed, why, the exact checks run and any remaining acceptance gap. Preserve stable entity identities, bounded polling, strict TLS, bearer rotation order and diagnostic redaction. Never include tokens, invitations, locations or raw Home Assistant configuration in a report.

A passing local suite does not prove installed Home Assistant behavior or real-Hub compatibility. Runtime claims need evidence from the exact component and runtime, including the UI flow, refresh, reauthentication, reload and restart behavior.

GitHub is used for source storage. Do not add CI, hosted builds, release publishing, dependency bots or HACS automation. Preserve the owner's branch and task restrictions when working in the coordinated workspace.

## Licensing

Preserve the [Apache-2.0 licence](../LICENSE) and existing attribution. Identify the origin and licence of new third-party code or assets. Do not import another Teslatlas repository's licence, contributor assignment process or commercial terms. See [licensing and attribution](../docs/legal/licensing.md).
