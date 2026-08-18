# Validation ledger

Hub base: `54bd87462b15af2c9e5314e9ac1bcd9a49f5256d`

This file distinguishes direct evidence from required-but-unavailable execution. Repository tests are specifications until a GitHub check or human run supplies results for the exact review-branch head.

## Evidence available in this session

- GitHub commit creation and exact per-commit diffs for review-branch changes.
- GitHub branch/commit/tree/file inspection.
- Source-level examination of unit/integration tests.
- Pull-request-triggered workflow runs and commit statuses, if the repository supplies them later.

No Cargo, Xcode, PostgreSQL, SQLite CLI, macOS runtime, Linux target, Tesla endpoint, TeslaMate deployment or physical-device execution is available in this session.

## Checks actually run

None.

No build, formatter, linter, dependency audit, unit test, integration test, runtime test, package build, notarisation check, migration, backup/restore, network comparison or physical-device test has been run for the review branch.

## Mandatory whole-repository gates — not run

```sh
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo test --locked --doc
cargo deny check
```

## macOS gates — not run

Run on the supported macOS/Xcode host with the repository's intended arm64 toolchain:

```sh
scripts/build-macos-app.sh
scripts/build-macos-service-package.sh
xcodebuild -project macos/TeslatlasHubApp/TeslatlasHubApp.xcodeproj \
  -scheme TeslatlasHubApp \
  -configuration Release \
  test
```

`project.yml` is the committed XcodeGen source; generate the project through the repository build path before invoking `xcodebuild` if the `.xcodeproj` is absent.

## S1 focused gates — not run

### Import-profile cap

```sh
cargo test --locked \
  config::tests::teslamate_read_limits_only_apply_a_non_raising_parallel_lane_cap \
  -- --exact
```

Acceptance criteria:

- a maximum above the configured lane count does not raise it;
- a lower maximum lowers it;
- a disabled profile preserves the configured value;
- zero remains rejected.

### macOS mutable-command admission

```sh
cargo test --locked --bin teslatlas-hub \
  tests::every_live_mutation_and_service_command_requires_the_instance_lock \
  -- --exact
```

Additional runtime gate on macOS:

1. start the installed Hub service against a disposable migrated store;
2. invoke `init`, `pair`, `repair` and `backup` separately against the same `data_dir`;
3. each command must fail with the existing-process/admission error without changing the catalogue, pack set, pairing rows or backup destination;
4. stop the service, rerun each command and verify the intended operation succeeds;
5. restart the service and verify readiness plus one strictly newer durable observation.

### Unsupported non-macOS Serve

```sh
cargo test --locked --bin teslatlas-hub \
  tests::serve_fails_before_initialising_state_on_an_unsupported_platform \
  -- --exact
```

Intended Linux compile/check gate:

```sh
cargo check --locked --all-targets --target x86_64-unknown-linux-gnu
cargo test --locked --all-targets --target x86_64-unknown-linux-gnu
```

The target/toolchain is unavailable here. Acceptance criteria: `serve` exits non-zero with the unsupported-platform message and does not create the configured data directory. This is an interim fail-closed gate, not evidence that Linux service support exists.

## Exact-head evidence rule

Before the draft can leave draft state, record for the final PR head:

- commit SHA;
- each check/workflow name and conclusion;
- workflow run URL/ID where available;
- OS/toolchain versions for macOS and Linux jobs;
- PostgreSQL/TeslaMate fixture version for migration gates;
- any intentionally skipped test with reason and owner approval.

A success on the Hub base, another branch or an earlier review commit does not validate the pull-request head.
