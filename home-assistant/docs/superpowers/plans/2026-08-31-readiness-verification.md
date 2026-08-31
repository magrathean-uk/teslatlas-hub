# Readiness and Main Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make repository claims match the implemented protocol-aligned, production-gated foundation; prove the merged result locally and independently; then fast-forward GitHub `main` without publishing a release or HACS package.

**Architecture:** Documentation and package tests encode the same five readiness gates as the approved design. Local protocol and integration harnesses provide reproducible evidence, independent reviewers cover contract/runtime/Home Assistant boundaries, and the final Git step permits only a verified fast-forward from the latest remote state.

**Tech Stack:** Markdown, JSON, Python 3.14.2+, pytest/coverage, Ruff, Git, GitHub remote, Teslatlas protocol conformance runner.

**Spec:** `docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md`

## Global Constraints

- Start only after every task in the other three `2026-08-31` implementation plans passes and is committed.
- Keep production pairing/transport, Zeroconf, commands, HACS publication, releases, brand assets, topics, Actions, Dependabot, and automation out of scope.
- Distinguish vendored contract/unit/fixture proof from live Hub, clean-install, and publication proof.
- Never modify, clean, commit, or push `/Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant`.
- Never force-push. If `origin/main` moved, stop the push and repeat the compare/reconcile/review cycle.
- Execute Tasks 1-4 in order.

---

### Task 1: Replace stale readiness and architecture claims

**Files:**
- Modify: `README.md`
- Rewrite: `docs/architecture.md`
- Rewrite: `docs/protocol-readiness.md`
- Rewrite: `docs/plans/2026-08-30-foundation.md`
- Modify: `docs/superpowers/specs/2026-08-30-hacs-local-push-foundation-design.md`
- Modify: `docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md`
- Create: `tests/test_documentation.py`

**Interfaces:**
- README reports protocol `1.2.0` parsing/fixture support and the pending production adapter, not a deployable integration.
- Architecture documents bounded query/SSE validation, synchronized fixture sessions, atomic projection/checkpoint storage, exact entities, and fail-closed config gates.
- Readiness lists the five open deployment/publication gates after the local contract gate.
- Superseded plans/specs remain as history but point to the approved `2026-08-31` design; they no longer present stale behavior as current.

- [ ] **Step 1: Create the complete failing documentation truth test module**

```python
from pathlib import Path


ROOT = Path(__file__).parents[1]
CURRENT_DOCS = (
    "README.md",
    "docs/architecture.md",
    "docs/protocol-readiness.md",
)
HISTORICAL_DOCS = (
    "docs/plans/2026-08-30-foundation.md",
    "docs/superpowers/specs/2026-08-30-hacs-local-push-foundation-design.md",
)


def test_current_docs_name_exact_open_gates_and_no_stale_runtime_claims():
    corpus = "\n".join(
        (ROOT / path).read_text()
        for path in ("README.md", "docs/architecture.md", "docs/protocol-readiness.md")
    )
    for required in (
        "79ced4c",
        "protocol `1.2.0`",
        "pairing and origin binding",
        "snapshot/stream continuity",
        "production adapter remains disabled",
    ):
        assert required.casefold() in corpus.casefold()
    for stale in (
        "protocol remains unfrozen",
        "manual and zeroconf config flows",
        "1, 2, 4, 8, 16",
        "hub sensors cover collector health",
    ):
        assert stale.casefold() not in corpus.casefold()


def test_readme_states_distribution_boundaries():
    readme = (ROOT / "README.md").read_text()
    assert "not production-installable" in readme
    assert "not HACS-published" in readme


def test_docs_do_not_claim_command_support():
    corpus = "\n".join((ROOT / path).read_text() for path in CURRENT_DOCS)
    assert "No commands, services, buttons, or switches are implemented." in corpus
    for forbidden in ("command support is enabled", "vehicle commands are supported"):
        assert forbidden.casefold() not in corpus.casefold()


def test_fixture_proof_is_not_reported_as_live_hub_proof():
    readiness = (ROOT / "docs/protocol-readiness.md").read_text()
    assert "Fixture proof: complete" in readiness
    assert "Live Hub proof: open" in readiness


def test_historical_documents_are_visibly_superseded():
    approved = "2026-08-31-concurrent-foundation-merge-design.md"
    for path in HISTORICAL_DOCS:
        value = (ROOT / path).read_text()
        assert "Status: Superseded" in value
        assert approved in value
```

The snippet above is the complete contents of `tests/test_documentation.py`; do not retain earlier documentation tests in that file.

- [ ] **Step 2: Run the focused test and verify red**

Run: `uv run pytest tests/test_documentation.py -q`

Expected: FAIL on obsolete protocol, Zeroconf, entity, reconnect, and status claims.

- [ ] **Step 3: Replace the README status and behavior sections exactly**

Replace everything from `## Current status` through the line immediately before `## Hard boundaries` with:

```markdown
## Current status

The integration validates a vendored representation of Teslatlas protocol `1.2.0` from commit `79ced4c`. Its schema, bounded JSON/SSE, query, event, storage, entity, migration, diagnostics, and Home Assistant lifecycle behavior are exercised with deterministic fixtures.

The production adapter remains disabled. Pairing and origin binding, Zeroconf, snapshot/stream continuity against a deployed Hub, live Hub proof, clean-install proof, release, and HACS publication remain open. This repository is not production-installable and not HACS-published.

## Implemented Home Assistant behavior

- One fixed Teslatlas Hub parent device.
- Vehicle sensors: battery level, estimated range, odometer, charging state, activity state, inside temperature, outside temperature, data quality, and observation timestamp.
- Vehicle binary sensors: locked and climate on.
- Exact protocol `1.2.0` schema validation and all-or-nothing vehicle/current fixture traversal.
- Bounded JSON and WHATWG SSE parsing with synchronized fixture sessions.
- Atomic projection/checkpoint persistence and fail-closed continuity transitions.
- HTTPS endpoints, except literal loopback HTTP; pairing secrets remain transient.
- Reauthentication, reconfiguration, config-entry migration, Repairs, and allow-listed diagnostics.
- No commands, services, buttons, or switches are implemented.

These are fixture-backed integration facts. They are not live Hub, clean-install, release, or HACS evidence.
```

Replace the whole `## HACS status` section, through the line before `## Licence`, with:

```markdown
## HACS status

Packaging metadata exists for local validation only. HACS publication remains blocked until pairing/origin binding, Zeroconf, deployed snapshot/stream continuity, live Hub and clean-install proof, an approved licensed brand asset, required repository topics, a public release, and current HACS validation all pass. No HACS submission or release has occurred.
```

- [ ] **Step 4: Rewrite `docs/architecture.md` with this exact body**

````markdown
# Home Assistant integration architecture

## Responsibility

Map released public Teslatlas Hub protocol `1.2.0` data into read-only Home Assistant devices and entities while every unproved deployment boundary fails closed.

## Contract and data path

The integration vendors Apache-2.0 schemas, examples, the SSE contract, and compatibility profile from protocol commit `79ced4c`. JSON responses and SSE frames are byte-bounded before parsing. Schema and semantic checks create immutable candidates; no partial query, event, projection, or checkpoint becomes visible.

Fixture data follows this path:

```text
validated discovery -> all query pages + current resources -> synchronized session
                                                        |-> atomic snapshot publish
validated SSE -> ordered immutable projection ----------|-> atomic checkpoint commit
```

The production adapter remains disabled: it encodes no pairing route, credential exchange, mDNS contract, or deployed synchronized-session claim.

## Identity, security, and continuity

Configuration accepts HTTPS or literal loopback HTTP only. Discovery origins must match the configured scheme, canonical host, and effective port before authentication. Pairing material is transient; only a freshly authenticated, version-confirmed query may establish Hub identity. Reauthentication increments credential generation atomically. Reconfiguration cannot forward stored secrets to an unproved endpoint.

A synchronized fixture session binds Hub ID, API and event origins, protocol version, normalized filters, credential generation, query snapshot, SSE stream, and replay checkpoint. Binding or continuity failure makes data unavailable and raises an allow-listed Repair issue. Replacement data is validated and committed before publication.

## Devices and entities

One fixed Teslatlas Hub parent owns each vehicle device. Every vehicle exposes battery level, estimated range, odometer, charging state, activity state, inside temperature, outside temperature, data quality, observation timestamp, locked, and climate-on entities. Only the two protocol-optional temperatures may be omitted; missing required fields reject the containing resource or event. Protocol `null` maps to Home Assistant unknown. Connection loss maps to unavailable.

## Privacy and boundaries

Diagnostics contain only entry version, TLS use, protocol/capability classifications, availability, connection/error classification, aggregate vehicle/data-quality counts, and replay-checkpoint presence. They exclude endpoint, port, bearer, pairing material, Hub/vehicle identity or name, location, precise vehicle timestamps outside the disabled observation-timestamp entity, replay/request identifiers, headers, bodies, and raw exceptions.

No code reads Hub storage, imports Hub-private source, asks for Tesla credentials, polls Tesla, calls private Tesla APIs, or implements commands, services, buttons, or switches.
````

- [ ] **Step 5: Rewrite `docs/protocol-readiness.md` with this exact body**

````markdown
# Public protocol readiness

## Verified local evidence

- Contract gate: fixture-complete for the vendored protocol representation from commit `79ced4c`, profile `1.2.0`.
- Fixture proof: complete for bounded discovery/query/SSE validation, synchronized fixture sessions, atomic projection/checkpoint storage, exact entities, migration, Repairs, diagnostics, and lifecycle tests.
- Live Hub proof: open.
- The production adapter remains disabled.

Fixture proof is not live Hub proof and is not clean-install, release, or HACS-publication proof.

## Open gates

1. Pairing and origin binding: publish and validate the deployed pairing, credential, redirect, and secret-origin contract.
2. Zeroconf: publish and validate service and TXT semantics without inventing deployment fields.
3. Snapshot/stream continuity: prove a deployed query snapshot and SSE stream share one binding, visibility boundary, filter set, credential generation, and replay history.
4. Live Hub and clean install: validate against a fresh supported Hub using only public artifacts and no Tesla credentials in Home Assistant.
5. HACS publication: approve a licensed brand asset, set required topics, create and verify a release, and pass then-current HACS validation.

No commands, services, buttons, or switches are implemented. Command support requires separate scope and authorization.

## Local verification

```bash
/Users/bolyki/dev/source/teslatlas-protocol/tools/check
uv sync --frozen --group dev
uv run pytest -q --cov=custom_components/teslatlas_hub --cov-branch --cov-fail-under=95
uv run ruff format --check .
uv run ruff check .
git diff --check
```

These commands prove only the checked source revision and local fixture harness.
````

- [ ] **Step 6: Mark historical documents superseded with exact text**

Replace all of `docs/plans/2026-08-30-foundation.md` with:

```markdown
# Home Assistant integration foundation plan

Status: Superseded

This historical plan was replaced by the [approved concurrent foundation merge design](../superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md). It is retained only to identify the earlier direction; do not execute it.
```

Insert immediately below the title in `docs/superpowers/specs/2026-08-30-hacs-local-push-foundation-design.md`:

```markdown
Status: Superseded

Current authority: [Concurrent Home Assistant foundation merge design](2026-08-31-concurrent-foundation-merge-design.md).

The remaining body is historical provenance and must not be executed as current guidance.
```

Replace the `## Status` section of `docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md`, stopping immediately before `## Goal`, with:

```markdown
## Status

Approved for implementation on 2026-08-31.

The implementation base is `teslatlas-home-assistant` commit `55d7024`. The second local repository contains no competing implementation: it remains at `0c14212` with one untracked design plan. That plan is design evidence, not executable code to copy.

The protocol baseline is `teslatlas-protocol` commit `79ced4c`, profile `1.2.0`. Pairing, credential rotation/revocation, mDNS/Zeroconf, deployment synchronization, and live Hub proof remain open gates.
```

- [ ] **Step 7: Run the focused documentation gate**

Run: `uv run pytest tests/test_documentation.py -q`

Run: `git diff --check`

Expected: PASS.

- [ ] **Step 8: Commit the documentation truth change**

```bash
git add README.md docs/architecture.md docs/protocol-readiness.md docs/plans/2026-08-30-foundation.md docs/superpowers/specs/2026-08-30-hacs-local-push-foundation-design.md docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md tests/test_documentation.py
git commit -m "docs: align readiness with merged foundation"
```

### Task 2: Lock packaging, provenance, and scope boundaries

**Files:**
- Rewrite: `tests/test_package.py`
- Verify only: `tests/test_protocol_schema.py`
- Modify: `custom_components/teslatlas_hub/manifest.json`
- Modify: `hacs.json`

**Interfaces:**
- Manifest remains domain `teslatlas_hub`, `integration_type=hub`, `iot_class=local_push`, config-flow enabled, with exact Home Assistant requirement `jsonschema[format-nongpl]==4.26.0` and no Zeroconf key; `pyproject.toml` permits `>=4.26,<5` and `uv.lock` pins `4.26.0` at this baseline.
- Vendored provenance records Apache-2.0 source, exact commit, source paths, and verified SHA-256 for each artifact.
- Package contains no command/service platform, `.github` automation, brand asset, or second integration.
- `hacs.json` remains packaging metadata only; it does not imply publication.

- [ ] **Step 1: Replace `tests/test_package.py` with the complete failing boundary suite**

```python
from __future__ import annotations

import ast
import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).parents[1]
INTEGRATION = ROOT / "custom_components" / "teslatlas_hub"
DOMAIN = "teslatlas_hub"


def load_json(path: Path) -> dict[str, object]:
    return json.loads(path.read_text(encoding="utf-8"))


def test_manifest_is_gated_local_push_package():
    manifest = load_json(INTEGRATION / "manifest.json")
    assert manifest["domain"] == DOMAIN
    assert manifest["name"] == "Teslatlas Hub"
    assert manifest["version"] == "0.1.0"
    assert manifest["integration_type"] == "hub"
    assert manifest["iot_class"] == "local_push"
    assert manifest["config_flow"] is True
    assert "zeroconf" not in manifest
    assert manifest["requirements"] == ["jsonschema[format-nongpl]==4.26.0"]


def test_repository_has_no_out_of_scope_publication_or_command_surface():
    assert not (ROOT / ".github").exists()
    assert not (ROOT / "brand").exists()
    assert not {
        "button.py",
        "switch.py",
        "services.yaml",
        "services.json",
    }.intersection(path.name for path in INTEGRATION.iterdir())


def test_repository_has_exactly_one_custom_integration():
    manifests = sorted(
        path.relative_to(ROOT).as_posix()
        for path in (ROOT / "custom_components").glob("*/manifest.json")
    )
    assert manifests == ["custom_components/teslatlas_hub/manifest.json"]


def test_hacs_metadata_has_exact_keys():
    assert load_json(ROOT / "hacs.json") == {
        "name": "Teslatlas Hub",
        "homeassistant": "2026.8.3",
    }


def test_runtime_has_no_forbidden_storage_or_tesla_imports():
    tracked = subprocess.check_output(
        [
            "git",
            "ls-files",
            "custom_components/teslatlas_hub/*.py",
            "custom_components/teslatlas_hub/**/*.py",
        ],
        cwd=ROOT,
        text=True,
    ).splitlines()
    forbidden = {
        "aiosqlite",
        "sqlite3",
        "tesla",
        "tesla_fleet_api",
        "teslajsonpy",
        "teslapy",
        "teslatlas",
        "teslatlas_hub",
        "teslatlas_service",
    }
    found: set[str] = set()
    for relative in tracked:
        tree = ast.parse((ROOT / relative).read_text(encoding="utf-8"))
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                found.update(alias.name.split(".", 1)[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                found.add(node.module.split(".", 1)[0])
    assert found.isdisjoint(forbidden)


def test_runtime_never_references_protocol_checkout_path():
    tracked = subprocess.check_output(
        ["git", "ls-files", "custom_components/teslatlas_hub"],
        cwd=ROOT,
        text=True,
    ).splitlines()
    forbidden = "/Users/bolyki/dev/source/teslatlas-protocol"
    for relative in tracked:
        path = ROOT / relative
        if path.is_file():
            assert forbidden not in path.read_text(encoding="utf-8", errors="ignore")
```

The snippet above is the complete `tests/test_package.py`. Provenance digest coverage remains in the complete bidirectional `test_copied_file_digest_matches_provenance` from Contract Task 1; do not duplicate or weaken it here.

- [ ] **Step 2: Run package/provenance tests and verify red**

Run: `uv run pytest tests/test_package.py tests/test_protocol_schema.py -q`

Expected: FAIL on the old Zeroconf/empty-requirements assertions and missing new scope checks.

- [ ] **Step 3: Apply the exact allowed metadata values**

Set `custom_components/teslatlas_hub/manifest.json` to the following key/value object, preserving JSON formatting but adding no keys:

```json
{
  "domain": "teslatlas_hub",
  "name": "Teslatlas Hub",
  "codeowners": ["@magrathean-uk"],
  "config_flow": true,
  "documentation": "https://github.com/magrathean-uk/teslatlas-home-assistant",
  "integration_type": "hub",
  "iot_class": "local_push",
  "issue_tracker": "https://github.com/magrathean-uk/teslatlas-home-assistant/issues",
  "requirements": ["jsonschema[format-nongpl]==4.26.0"],
  "version": "0.1.0"
}
```

Set `hacs.json` exactly to:

```json
{
  "name": "Teslatlas Hub",
  "homeassistant": "2026.8.3"
}
```

Confirm, without editing, that `pyproject.toml` contains `jsonschema[format-nongpl]>=4.26,<5` and `uv.lock` resolves distribution `jsonschema` version `4.26.0`. Stop if either differs; repair dependency resolution in Contract Task 1 rather than hiding it here. Do not add release metadata, topics, brand files, or workflows.

- [ ] **Step 4: Run the focused package gate**

Run: `uv run pytest tests/test_package.py tests/test_protocol_schema.py -q`

Run: `uv lock --check && uv run python -m json.tool custom_components/teslatlas_hub/manifest.json >/dev/null && uv run python -m json.tool hacs.json >/dev/null`

Expected: PASS.

- [ ] **Step 5: Commit the package-boundary change**

```bash
git add tests/test_package.py custom_components/teslatlas_hub/manifest.json hacs.json
git commit -m "test: lock package and provenance boundaries"
```

### Task 3: Run full gates and independent review

**Files:**
- None. This task is read-only unless a separate exact repair plan is required.

**Interfaces:**
- Protocol source remains exact commit `79ced4c7fdc79520ad31d72a0280bf5f3f19f407` and passes its own full check.
- Integration passes full branch coverage at least 95%; `sse.py`, `checkpoint.py`, and `coordinator.py` each pass 100% branch coverage.
- Independent reviewers report no remaining P1/P2 contract/security, runtime/privacy, or Home Assistant migration/package findings.

- [ ] **Step 1: Verify the protocol source and vendored byte provenance**

Run: `test "$(git -C /Users/bolyki/dev/source/teslatlas-protocol rev-parse HEAD)" = "79ced4c7fdc79520ad31d72a0280bf5f3f19f407"`

Run: `git -C /Users/bolyki/dev/source/teslatlas-protocol status --short`

Expected: no output.

Run: `/Users/bolyki/dev/source/teslatlas-protocol/tools/check`

Expected: PASS with 54 protocol tests and 31 conformance runs.

Run: `uv run pytest tests/test_protocol_schema.py -q`

Expected: every recorded digest and copied fixture/artifact passes.

- [ ] **Step 2: Run the complete integration gate from the lockfile**

Run: `uv sync --frozen --group dev`

Run: `uv run pytest -q --cov=custom_components/teslatlas_hub --cov-branch --cov-report=term-missing --cov-fail-under=95`

Run: `uv run pytest tests/test_sse.py --cov=custom_components.teslatlas_hub.sse --cov-branch --cov-fail-under=100 -q`

Run: `uv run pytest tests/test_checkpoint.py --cov=custom_components.teslatlas_hub.checkpoint --cov-branch --cov-fail-under=100 -q`

Run: `uv run pytest tests/test_coordinator.py tests/test_init.py --cov=custom_components.teslatlas_hub.coordinator --cov-branch --cov-fail-under=100 -q`

Run: `uv run pytest tests/test_config_flow.py --cov=custom_components.teslatlas_hub.config_flow --cov-branch --cov-fail-under=100 -q`

Run: `uv run ruff format --check . && uv run ruff check .`

Run: `uv run python -c 'import json,pathlib,subprocess; [json.loads(pathlib.Path(path).read_text()) for path in subprocess.check_output(["git", "ls-files", "*.json"], text=True).splitlines()]'`

Run: `git diff --check && git status --short --branch`

Expected: all commands PASS; status contains no uncommitted files.

- [ ] **Step 3: Request three independent reviews in parallel with these exact scopes**

Invoke `superpowers:requesting-code-review` and spawn all three reviewers in one parallel batch. Use these prompts verbatim:

```text
Read-only review. Compare docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md and protocol commit 79ced4c7fdc79520ad31d72a0280bf5f3f19f407 against integration range 1a61d78..HEAD. Audit protocol artifacts/provenance, schemas/semantics, bounded bodies, discovery, query, config-flow secret/origin handling, errors, logs, diagnostics, and privacy. Report concrete file:line P1/P2 findings or exactly: no P1/P2 found. Do not edit.
```

```text
Read-only review. Compare docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md and protocol commit 79ced4c7fdc79520ad31d72a0280bf5f3f19f407 against integration range 1a61d78..HEAD. Audit SSE framing/bounds, event validation/order, synchronized binding, projection/checkpoint atomicity, storage corruption, coordinator states, reconciliation, cancellation, replay, unload/remove, and Repairs. Report concrete file:line P1/P2 findings or exactly: no P1/P2 found. Do not edit.
```

```text
Read-only review. Compare docs/superpowers/specs/2026-08-31-concurrent-foundation-merge-design.md and protocol commit 79ced4c7fdc79520ad31d72a0280bf5f3f19f407 against integration range 1a61d78..HEAD. Audit Home Assistant device/entity identity, exact entity set, availability/unknown semantics, disabled timestamp, migrations, registry retirement/addition, translations, package metadata, documentation truth, and scope exclusions. Report concrete file:line P1/P2 findings or exactly: no P1/P2 found. Do not edit.
```

Each reviewer must report concrete `file:line` P1/P2 findings or exactly `no P1/P2 found`; a missing/null reviewer response is an orchestration failure, not approval.

- [ ] **Step 4: Stop and plan any valid P1/P2 before editing**

If any reviewer reports a valid P1/P2, stop this plan before editing. Invoke `superpowers:writing-plans` and create `docs/superpowers/plans/2026-08-31-independent-review-repairs.md` with the finding's exact files, failing test code, minimal implementation code, focused commands, and commit. Execute and verify that plan through the user-selected workflow, then return to Step 5. If a proposed finding conflicts with the approved contract, record exact spec/protocol evidence and ask the same reviewer to re-evaluate it.

- [ ] **Step 5: Re-run every exact gate after the last review repair**

Run all of the following, even when no repair was required:

```bash
test "$(git -C /Users/bolyki/dev/source/teslatlas-protocol rev-parse HEAD)" = "79ced4c7fdc79520ad31d72a0280bf5f3f19f407"
git -C /Users/bolyki/dev/source/teslatlas-protocol status --short
/Users/bolyki/dev/source/teslatlas-protocol/tools/check
uv run pytest tests/test_protocol_schema.py -q
uv sync --frozen --group dev
uv run pytest -q --cov=custom_components/teslatlas_hub --cov-branch --cov-report=term-missing --cov-fail-under=95
uv run pytest tests/test_sse.py --cov=custom_components.teslatlas_hub.sse --cov-branch --cov-fail-under=100 -q
uv run pytest tests/test_checkpoint.py --cov=custom_components.teslatlas_hub.checkpoint --cov-branch --cov-fail-under=100 -q
uv run pytest tests/test_coordinator.py tests/test_init.py --cov=custom_components.teslatlas_hub.coordinator --cov-branch --cov-fail-under=100 -q
uv run pytest tests/test_config_flow.py --cov=custom_components.teslatlas_hub.config_flow --cov-branch --cov-fail-under=100 -q
uv run ruff format --check .
uv run ruff check .
uv run python -c 'import json,pathlib,subprocess; [json.loads(pathlib.Path(path).read_text()) for path in subprocess.check_output(["git", "ls-files", "*.json"], text=True).splitlines()]'
git diff --check
git status --short --branch
```

Expected: protocol status has no output; protocol check reports 54 tests and 31 conformance runs; every other command passes; integration status is clean.

If any scoped file changed during a repair, resend the exact matching Step 3 prompt. If files cross scopes, resend every implicated prompt in parallel. Wait for a non-null response from each and require exactly `no P1/P2 found`. A missing response is an orchestration failure and must be retried, not counted as approval.

### Task 4: Fast-forward GitHub main and verify the result

**Files:** None unless the remote moved and reconciliation produces an approved reviewed merge.

**Interfaces:**
- Local `main` and `origin/main` finish at the same reviewed commit.
- Separate local evidence repository finishes byte-for-byte and status-for-status unchanged from the execution preflight.
- No tag, release, HACS submission, topic, workflow, branch deletion, or force-push occurs.

- [ ] **Step 1: Recheck preservation against the original suite preflight**

Retrieve `SEPARATE_REPOSITORY_ORIGINAL_BASELINE` captured before Contract Task 1. Re-read the separate repository's `rev-parse HEAD` and `status --porcelain=v1 -uall` now. Also recalculate the sorted SHA-256 manifest for every untracked path, including `docs/plans/2026-08-30-local-push-hacs.md`, using this read-only command:

```bash
python3 - <<'PY'
import hashlib
import subprocess
from pathlib import Path

root = Path("/Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant")
raw_paths = subprocess.check_output(
    ["git", "-C", str(root), "ls-files", "--others", "--exclude-standard", "-z"]
)
for raw_path in sorted(path for path in raw_paths.split(b"\0") if path):
    relative = raw_path.decode("utf-8")
    with (root / relative).open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    print(f"{digest}  {relative}")
PY
```

Compare current HEAD, full porcelain output, path set, and every digest byte-for-byte with the original suite-preflight values. If any value differs, stop before fetch/push and report the preservation failure; do not clean or repair the separate repository.

Run in the implementation repository:

`git status --short --branch && git branch --show-current && git rev-parse HEAD`

Expected: branch `main`, clean tree.

- [ ] **Step 2: Fetch and prove a fast-forward is still possible**

Run: `git fetch origin main`

Run: `git rev-list --left-right --count origin/main...HEAD`

Expected first number: `0`. If it is nonzero, do not push or edit. Inspect the new remote commits read-only, invoke `superpowers:writing-plans`, and create `docs/superpowers/plans/2026-08-31-remote-main-reconciliation.md` with exact changed files, tests, merge method, conflict resolutions, gates, and review range. Execute that separately approved plan, then restart Task 3.

Run: `git merge-base --is-ancestor origin/main HEAD`

Expected: exit 0.

- [ ] **Step 3: Push reviewed main without force**

Run: `git push origin main`

Expected: fast-forward success.

- [ ] **Step 4: Verify remote and preservation**

Run: `test "$(git rev-parse HEAD)" = "$(git ls-remote origin refs/heads/main | cut -f1)"`

Expected: exit 0.

Re-read the separate repository with:

```bash
git -C /Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant rev-parse HEAD
git -C /Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant status --porcelain=v1 -uall
python3 - <<'PY'
import hashlib
import subprocess
from pathlib import Path

root = Path("/Users/bolyki/Documents/Codex/2026-08-30/teslatlas-home-assistant/outputs/teslatlas-home-assistant")
raw_paths = subprocess.check_output(
    ["git", "-C", str(root), "ls-files", "--others", "--exclude-standard", "-z"]
)
for raw_path in sorted(path for path in raw_paths.split(b"\0") if path):
    relative = raw_path.decode("utf-8")
    with (root / relative).open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    print(f"{digest}  {relative}")
PY
git status --short --branch
```

Compare the first three outputs exactly to `SEPARATE_REPOSITORY_ORIGINAL_BASELINE`, not merely to the late Task 4 recapture.

Expected: separate repository unchanged; implementation repository clean and `main` aligned with `origin/main`.

- [ ] **Step 5: Report proof boundaries**

Report exact final SHA, protocol SHA, test/conformance counts, coverage, Ruff/JSON/diff results, reviewer outcomes, and remote equality. State explicitly: production transport still disabled; no live Hub proof, clean-install proof, release, or HACS publication occurred.
