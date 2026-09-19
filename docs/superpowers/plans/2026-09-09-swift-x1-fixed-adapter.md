# Swift X1 Fixed Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Register the reviewed Swift adapter in Hub and make its installed-runner callbacks source-fixed, fail-closed, and ready for target-specific X1 SessionInputs and receipts.

**Architecture:** `swift_installed.py` already stages a hash-bound SessionInput and supplies the reviewed coordinator capability. Complete the six `FixedInstalledAdapter` seams there: the launcher starts only that coordinator; the runtime inventory is written from worker-observed output; admission re-runs the reviewed Swift semantic validator; supplement/log callbacks retain only bound completion, cleanup, and command evidence. `installed_registry.py` exposes precisely this source-fixed entry; matrix job JSON remains unable to select code, topology, or a service executable.

**Tech Stack:** Python 3.11+, hash-bound private files, Swift matrix coordinator/worker wire, existing installed-runner supervision.

**Spec:** `hub/docs/development/PLAN.md` H3 and `docs/development/MASTER_PLAN.md` B2/X1.

## Global Constraints

- Keep the Swift SDK a developer resource; do not create a resident Swift service.
- Source identity, actor topology, executable launch authority, and runtime facts are fixed in Hub source; runtime JSON supplies only validated private inputs.
- Use the existing runner close-before-ack sequence and fail closed on missing, foreign, or changed evidence.
- Keep macOS execution `local`, Debian execution `docker_exec_pipe`, and the coordinator broker `unix` for all three targets.
- Preserve unrelated dirty files; do not commit, push, publish, create a release, run CI, or start an installed session while completing source work.

---

### Task 1: Register the reviewed Swift topology

**Files:**
- Modify: `tools/interop/matrix_runner/test_installed_registry.py:53-98`
- Modify: `tools/interop/matrix_runner/installed_registry.py:67-114`

**Interfaces:**
- Produces `FIXED_INSTALLED_REGISTRY["swift"]` with `swift_macos` and `swift_linux`, `swift_installed.CONTRACT`, `execution_by_target()`, `broker_kind_by_target()`, and all six callbacks.
- Consumes no job-supplied callback, command, execution topology, or broker kind.

- [ ] **Step 1: Write the failing registry test**

```python
swift = installed_registry.FIXED_INSTALLED_REGISTRY["swift"]
self.assertEqual(("swift_macos", "swift_linux"), swift.required_actor_ids)
self.assertEqual(
    (("macos_arm64", "local"), ("debian13_amd64", "docker_exec_pipe"),
     ("debian13_arm64", "docker_exec_pipe")),
    swift.execution_by_target,
)
```

The test must also load the exact public reviewed contract and assert the callback names, so a registry row that merely has an adapter id cannot pass.

- [ ] **Step 2: Run the focused test and observe RED**

Run: `python3.11 -m unittest tools.interop.matrix_runner.test_installed_registry.InstalledRegistryTests.test_reviewed_entries_bind_exact_actor_contracts_and_topology`

Expected: FAIL because the fixed registry has no `swift` entry.

- [ ] **Step 3: Add only the source-fixed registry row**

```python
from . import swift_installed as _swift_installed

"swift": FixedInstalledAdapter(
    adapter_id="swift", client_id="swift", contract=_swift_installed.CONTRACT,
    required_actor_ids=("swift_macos", "swift_linux"),
    execution_by_target=_swift_installed.execution_by_target(),
    broker_kind_by_target=_swift_installed.broker_kind_by_target(),
    build_session_input=_swift_installed.build_session_input,
    launch_adapter=_swift_installed.launch_adapter,
    admit=_swift_installed.admit,
    build_supplement=_swift_installed.build_supplement,
    execution_logs=_swift_installed.execution_logs,
    runtime_inventory=_swift_installed.runtime_inventory,
),
```

- [ ] **Step 4: Run the focused test and observe GREEN**

Run the same command. Expected: PASS once the callback implementations exist.

### Task 2: Complete the source-fixed Swift adapter callbacks

**Files:**
- Modify: `tools/interop/matrix_runner/test_swift_installed.py`
- Modify: `tools/interop/matrix_runner/swift_installed.py:618-634`

**Interfaces:**
- `launch_adapter(job, cell, config, contract, session_input_path, session_input, *, deadline, session, running) -> SwiftCoordinatorProcess`
- `runtime_inventory(job, cell, config, contract, session_input, deadline) -> Mapping`
- `admit(..., admission_views, runtime_context, deadline) -> Mapping`
- `build_supplement(..., result, runtime_context) -> file binding`
- `execution_logs(..., result) -> Mapping`

- [ ] **Step 1: Write focused failing behavior tests**

Add one narrow test for each boundary:

```python
def test_launch_adapter_uses_the_reviewed_coordinator_and_fixed_workers(self): ...
def test_runtime_inventory_requires_observed_worker_runtime_not_job_metadata(self): ...
def test_admit_rejects_foreign_worker_evidence_before_semantic_admission(self): ...
def test_supplement_binds_closed_runner_evidence_and_both_swift_actors(self): ...
def test_execution_logs_bind_the_retained_coordinator_stream(self): ...
```

Use literal actor ids, a private temporary coordination root, and bound files. Mock only the external worker process boundary; retain real private-file and semantic-validation behavior. Each test must name the production mutation it catches in a short comment.

- [ ] **Step 2: Run the focused tests and observe RED**

Run: `python3.11 -m unittest tools.interop.matrix_runner.test_swift_installed`

Expected: FAIL with missing Swift adapter callbacks, not an unrelated fixture error.

- [ ] **Step 3: Add the smallest fail-closed callback implementation**

The launcher must call `start_coordinator_process` around `run_coordinator`, with two closed workers built from fixed actor/session values. Runtime inventory must write and re-read a private runtime document from the coordinator output, and never copy `job["runtime"]` into `runtime_actual`. Admission must reconstruct the reviewed Swift `AdmissionContext` from bound normalized/actor/runtime/controller data and require every contract case to have the reviewed result (`installed_service_runtime` remains pending). Supplement must bind completion, both retained actor claims, closed session evidence, transport cleanup, command outcome, and controller observations before writing `installed-supplement.json`. Log collection must bind the source-fixed coordinator log and command record.

- [ ] **Step 4: Run focused tests and the registry test**

Run:

```bash
python3.11 -m unittest tools.interop.matrix_runner.test_swift_installed tools.interop.matrix_runner.test_installed_registry
```

Expected: PASS. The negative tests must show a foreign runtime/evidence fails closed.

### Task 3: Review the target-specific boundary without launching it

**Files:**
- Modify: `docs/development/STATUS.json`
- Modify: this plan

**Interfaces:**
- Consumes the green source registry/callback suite.
- Produces an explicit X1 handoff that says source boundary is ready, while macOS, Debian AMD64, and Debian ARM64 SessionInputs/actual Swift receipts remain separate target-owned work.

- [ ] **Step 1: Run source hygiene gates**

Run:

```bash
python3.11 -m py_compile tools/interop/matrix_runner/swift_installed.py tools/interop/matrix_runner/installed_registry.py
git diff --check
```

Expected: PASS with no generated file or unrelated dirty-tree rewrite.

- [ ] **Step 2: Record the exact boundary and limits**

Update status only after source checks pass. State that this is source-level callback/registry proof, not a SessionInput, installed host, real HTTP, physical device, or final X1 acceptance receipt.

- [ ] **Step 3: Complete plan checkboxes only for observed work**

Do not mark a target launched or accepted without its own retained receipt and cleanup proof.

## Self-Review

- Spec coverage: Task 1 fixes the entry and topology; Task 2 implements every required runner callback with bounded evidence; Task 3 keeps source proof distinct from target/session acceptance.
- Placeholder scan: each callback has a named behavior test, source boundary, and verification command; no runtime command is delegated to job metadata.
- Type consistency: registry uses `adapter_id == client_id == "swift"`; SessionInput retains both exact actor ids and their immutable Unix broker contract.

## Execution Handoff

The direct owner request authorizes inline, test-first execution in this existing dirty checkout. Do not create a worktree, branch, commit, or external publication.
