# Viewer R1 51-Drive Fixture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prepare a fresh, private Hub-owned R1 fixture that supplies a synthetic 51-drive `25/25/1` browser session and bounded Hub recovery controls.

**Architecture:** Add one fixed `viewer-r1-51-drives` scenario identifier to the existing test-only seed, fixture runner, and private supervisor. The identifier resolves only to a reviewed repository scenario file; callers cannot supply arbitrary scenario paths. The guest handoff remains owner-only and contains no invitations, credentials, raw cursors, or browser state.

**Tech Stack:** Rust interop fixture, Python 3.13 private runner/supervisor, SQLite, trusted loopback TLS, Node/Viewer handoff.

**Spec:** `hub/docs/development/PLAN.md` H4/H7 and `teslatlas-viewer/docs/development/PLAN.md` R1.

## Global Constraints

- Keep installed Hub `8443`, HA `18483`, and retired Edge r2-r4 roots untouched.
- Use a fresh owner-only output root and a fixed `127.0.0.1:18480` port only after a free-port check.
- Seed only synthetic data; do not activate Tesla credentials, wake a vehicle, or alter a live collector.
- Retain normal TLS validation and exact Viewer origin CORS; never enable certificate bypass.
- Preserve every existing B1 scenario API and use no arbitrary scenario path/config input.
- Do not commit, push, publish, create a release, or upload an artifact.

---

### Task 1: Add a fixed Viewer R1 seed scenario

**Files:**
- Modify: `tests/interop/seed.rs:40-155`
- Modify: `examples/interop_fixture.rs:5-27`
- Modify: `tests/interop_fixture.rs:1-132`
- Create: `tests/interop/scenario-viewer-r1-51-drives.json`

**Interfaces:**
- Consumes: `--output NEW_ABSOLUTE_DIRECTORY --port PORT [--source-id UUID]` fixture CLI.
- Produces: Optional `--scenario viewer-r1-51-drives`, a 51-row first-vehicle history ordered as literal pages `1051..1027`, `1026..1002`, `1001`, and the existing empty second vehicle/current/charge data.

- [x] **Step 1: Write the failing Rust test**

```rust
#[tokio::test]
async fn viewer_r1_fixture_has_exact_twenty_five_twenty_five_one_drive_pages() {
    let prepared = seed::prepare_viewer_r1(&root, 18444).unwrap();
    let page_one = get(&app, "/v1/vehicles/.../drives?limit=25", token).await;
    assert_eq!(ids(&page_one), (1027..=1051).rev().collect::<Vec<_>>());
    // Follow the two opaque cursors and assert the literal second and terminal pages.
}
```

- [x] **Step 2: Run the focused test to verify it fails**

Run: `cargo test --locked --test interop_fixture viewer_r1_fixture_has_exact_twenty_five_twenty_five_one_drive_pages`

Expected: FAIL because `prepare_viewer_r1` and the fixed scenario selector do not exist.

- [x] **Step 3: Implement the smallest fixed scenario selector**

```rust
pub fn prepare_viewer_r1(root: &Path, port: u16) -> Result<PreparedFixture> {
    prepare_with_scenario(root, port, FixtureScenario::ViewerR1, Uuid::new_v4())
}
```

```rust
// `FixtureScenario::ViewerR1` generates only IDs 1001 through 1051 for the
// first synthetic vehicle; `FixtureScenario::B1` retains the five existing IDs.
```

Make the example accept only `--scenario viewer-r1-51-drives`; reject every other scenario value. Add the matching static scenario with literal `25/25/1` page IDs and existing synthetic current/empty-vehicle/charge expectations.

- [x] **Step 4: Run focused Rust verification**

Run: `cargo test --locked --test interop_fixture`

Expected: PASS, including the original three-page B1 fixture test and the new literal 51-drive test.

### Task 2: Bind the fixed scenario to both private launchers

**Files:**
- Modify: `tools/interop/fixture.py:30-310`
- Modify: `tools/interop/test_fixture.py:25-250`
- Modify: `tools/interop/client_lanes/supervisor.py:20-50`

**Interfaces:**
- Consumes: Owner-only runner JSON field `scenario_id: "viewer-r1-51-drives"`.
- Produces: Fixture `scenario.json` copied from the one fixed repository scenario; exact seed argv with `--scenario viewer-r1-51-drives`; supervisor descriptor carrying that copied scenario digest and its existing `stop`, `start`, `revoke`, and `pair` controls.

- [x] **Step 1: Write failing fixture tests**

```python
def test_viewer_r1_selector_emits_fixed_seed_argument_and_copied_scenario(self):
    self.config["scenario_id"] = "viewer-r1-51-drives"
    loaded = self.load()
    self.assertEqual(
        fixture.seed_command(loaded, self.executable, self.root / "new", 18480)[-2:],
        ["--scenario", "viewer-r1-51-drives"],
    )
```

```python
def test_unknown_scenario_selector_fails_before_creating_state(self):
    self.config["scenario_id"] = "../../untrusted"
    with self.assertRaises(ValueError):
        self.load()
```

- [x] **Step 2: Run the focused Python test to verify it fails**

Run: `python3.11 -m unittest tools.interop.test_fixture.ConfigTests.test_viewer_r1_selector_emits_fixed_seed_argument_and_copied_scenario tools.interop.test_fixture.ConfigTests.test_unknown_scenario_selector_fails_before_creating_state`

Expected: FAIL because the config selector is currently unknown and `seed_command` has no scenario argument.

- [x] **Step 3: Implement fixed-selector mapping only**

```python
SCENARIO_SOURCES = {
    "b1-five-drives": Path(__file__).resolve().parents[2] / "tests/interop/scenario.json",
    "viewer-r1-51-drives": Path(__file__).resolve().parents[2] / "tests/interop/scenario-viewer-r1-51-drives.json",
}
```

Validate the selector against this mapping, pass it to the seeded binary, and use the same mapping when `fixture.py` and `supervisor.py` create their private `scenario.json` copy. Preserve B1 default behavior exactly.

- [x] **Step 4: Run focused and surrounding checks**

Run: `python3.11 -m unittest tools.interop.test_fixture && python3.11 -m py_compile tools/interop/fixture.py tools/interop/client_lanes/supervisor.py`

Expected: PASS. The B1 configuration tests remain green, unknown selectors are rejected before any output root, and the private supervisor uses the same selected scenario.

### Task 3: Build and prepare the guest-only R1 descriptor

**Files:**
- Guest-only create: `/home/bolyki.guest/teslatlas-fixtures/viewer-r1-20260909.run.json`
- Guest-only create: `/home/bolyki.guest/teslatlas-fixtures/viewer-r1-20260909/` after explicit Viewer start direction only
- Modify: `docs/development/STATUS.json`

**Interfaces:**
- Consumes: Rebuilt guest `interop_fixture`, current release Hub binary, profile `hub-http-v1@1.0.0`, owner-only config, and Viewer’s exact static origin.
- Produces: A mode-0600 redacted handoff naming only the descriptor path, trusted Hub URL/CA path, scenario digest/pages, origin, lifecycle-control ownership, and start/stop boundaries.

- [x] **Step 1: Recheck guest targets before mutation**

Run: guest read-only checks for retired r2-r4 receipts, 18480 availability, installed 8443/HA 18483 listeners, and exact staged binary/profile identity.

Expected: r2-r4 remain preserved, 18480 is free, unrelated listeners remain present, and no non-Hub process changes.

- [x] **Step 2: Rebuild only the seed if source bytes changed**

Run: acquire the shared heavy-build lock, build the release `interop_fixture` example, verify its SHA-256, and release the lock.

Expected: a new guest seed candidate; do not rebuild or reinstall the Hub binary unless it changed.

- [x] **Step 3: Create and validate the owner-only descriptor without starting it**

```json
{
  "scenario_id": "viewer-r1-51-drives",
  "port": 18480,
  "allowed_origins": ["http://127.0.0.1:4173"]
}
```

Expected: `fixture.py` accepts the protected configuration; its fresh output root does not exist; no Hub process starts.

- [x] **Step 4: Publish redacted status only**

Record paths/digests/expected page shape, recovery-control owner, explicit start/stop command, and limits. Do not record invitations, bearer/device values, raw cursors, private certificates, or browser state.

## Self-Review

- Spec coverage: Task 1 supplies fixed 51-drive data; Task 2 keeps the public fixture and recovery supervisor bound to that exact scenario; Task 3 prepares only a fresh private handoff with trusted CORS/TLS and lifecycle controls.
- Placeholder scan: no TBD/TODO or arbitrary scenario source exists.
- Type consistency: `scenario_id` is one enum-like string across runner config, seed argv, static scenario, supervisor, and redacted descriptor.

## Execution Handoff

The owner-directed request requires inline execution in this session. Use the test-first steps above; do not create a branch, worktree, commit, or external publication.
