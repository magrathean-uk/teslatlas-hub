# Findings and subsystem ledger

Hub base: `54bd87462b15af2c9e5314e9ac1bcd9a49f5256d`

Findings remain in this file even when rejected or fixed. Status describes the review-branch state, not an executed validation result.

## Subsystem ledger

### S1 — configuration, CLI, process admission and lifecycle ownership

Status: source review complete; focused execution not available.

Reviewed production paths and bounded responsibilities:

- `src/lib.rs`: crate policy, public module/export boundary and build/legal constants;
- `src/main.rs`: complete CLI, early-return paths, writable-store admission, macOS worker ownership, shutdown paths and command tests;
- `src/config.rs`: complete strict configuration model, defaults, validation and tests;
- `src/crypto.rs`: process-global Rustls provider installation;
- `src/hub_user_process.rs`: admitted-process capability and retained lock ownership;
- `src/user_lifetime_lock.rs`: descriptor-retained Unix directory/lock admission and tests;
- `src/macos_launch_agent.rs`: S1-only preflight/admission interface; package/update details remain assigned to S7;
- `src/server.rs`: S1-only listener/task ownership and shutdown; route/authentication semantics remain assigned to S6;
- `src/performance_profile.rs`: host-profile derivation and its non-raising contract.

Inputs and trust boundaries examined: argv/stdin distinction, exact configuration bytes and digest, local filesystem identity, per-user instance lock, process signals, task cancellation, TCP bind, TLS/plaintext exposure defaults, source URL/config secrets and operator-selected import limits.

Evidence summary:

- configuration parsing denies unknown fields and rejects remote plaintext Hub listeners, unsafe public TLS origins, unsafe Owner API/geocoder/stream URLs, partial TeslaMate source configuration and invalid read bounds;
- the macOS Serve supervisor retains and stops both collector and listener tasks, waits for collector readiness before constructing the server and aborts non-cooperative workers after a bound;
- read-only commands return before writable `HubStore::initialize`;
- mutable commands did not all share the admitted-process lock at the base commit (HUB-001);
- non-macOS `Serve` had no compiled body and therefore returned success after initialising writable state without serving (HUB-002);
- a configured host-profile maximum was implemented as an override and could raise parallel PostgreSQL COPY lanes (HUB-003);
- repository enforcement does not currently match the documented branch/CI controls (HUB-004).

S1 code commits:

- `0acef3685fae99c302710313da6c2ad3c459eeae` — import-profile cap and regression test;
- `77fbbbfe13b9aae2a28078a174fd7500683d722a` — mutable-command admission, explicit unsupported-platform failure and focused tests.

No build, test, formatter, linter or runtime command was executed for these commits in this session. Required validation is recorded in `04-VALIDATION.md`.

## HUB-001

ID: HUB-001

Severity: P1 — serious

Confidence: confirmed

Status: fixed

Affected paths: `src/main.rs`, `src/hub_user_process.rs`, `src/user_lifetime_lock.rs`

Invariant: Every command that mutates, repairs, snapshots or serves the live Hub data directory must retain the same admitted-process lifetime lock for the whole sensitive operation.

Failure scenario: While the LaunchAgent service owns the data directory, a second CLI process invokes `init`, `repair`, `pair` or `backup`. At the base commit those commands bypass `command_requires_user_hub_admission`, then open writable Hub state. `init` may execute schema work; `repair` may change catalogue/files; `pair` writes credentials; and `backup` takes a supposedly coherent data snapshot outside the declared single-instance authority. The result can be a race with live publication, repair or credential state and persistent damage or an invalid recovery generation.

Evidence: At Hub base `54bd874...`, the comment before admission states that every mutating/serving command takes the same lock, but `command_requires_user_hub_admission` matched only `Serve`, `Observe` and `Migrate`. The common command path subsequently called `HubStore::initialize` for `Init`, `Pair`, `Repair` and `Backup`. `AdmittedUserHub` retains `UserLifetimeLock`, and the common path already revalidates sensitive access and the loaded store path when an admission exists.

TeslaMate comparison, if applicable: Not applicable; this is local Hub process authority.

Remediation: Commit `77fbbbfe...` adds `Init`, `Pair`, `Repair` and `Backup` to the admitted-command set. The existing common path therefore retains the same lock and performs path revalidation before opening writable state. `Install` retains its separate explicit admission path. Read-only commands remain lock-free and immutable/read-only as before.

Tests or validation: Added `tests::every_live_mutation_and_service_command_requires_the_instance_lock` under macOS cfg. Not run. Human/CI command is recorded in `04-VALIDATION.md`.

Compatibility/rollback: No data or wire format changes. Concurrent CLI operations that previously raced with a live service now fail closed with the existing admission error. Roll back commit `77fbbbfe...` only if the owner accepts reintroducing concurrent mutable access.

## HUB-002

ID: HUB-002

Severity: P2 — material

Confidence: confirmed

Status: fixed

Affected paths: `src/main.rs`

Invariant: A CLI command that claims to start the service must either bind and own the service lifecycle or return a non-zero error before creating/mutating state.

Failure scenario: On Linux or another non-macOS target, an operator or service manager runs `teslatlas-hub serve`. The `Command::Serve` arm contained only a `#[cfg(target_os = "macos")]` block. On other targets the command opened/initialised the SQLite store, executed no listener or collector code, and returned success. A supervisor could report a successful service invocation while no endpoint existed.

Evidence: At the base commit, `Serve` was present in the cross-platform Clap enum and reached the common writable-store path. Its entire implementation body was conditionally removed on non-macOS. No fallback error existed.

TeslaMate comparison, if applicable: Not applicable; Linux Hub service support is planned rather than claimed equivalent to TeslaMate deployment today.

Remediation: Commit `77fbbbfe...` rejects `Serve` under `cfg(not(target_os = "macos"))` immediately after argument/default-path resolution and before loading configuration or initialising Hub state. The error states that the runtime is not yet supported and Linux support is planned.

Tests or validation: Added non-macOS test `tests::serve_fails_before_initialising_state_on_an_unsupported_platform`, asserting an error and absence of the configured data directory. Not run.

Compatibility/rollback: This intentionally converts a false-success no-op into a failure. No data/wire changes. A future Linux runtime should replace the explicit gate only when it owns listener, cancellation, instance admission and service-manager semantics.

## HUB-003

ID: HUB-003

Severity: P2 — material

Confidence: confirmed

Status: fixed

Affected paths: `src/config.rs`, `src/performance_profile.rs`

Invariant: A performance-profile maximum is an upper bound. It may lower configured import concurrency but must never raise the operator-selected PostgreSQL COPY lane count; a disabled profile must not alter it.

Failure scenario: The operator configures four COPY lanes and a profile maximum of eight. The base `read_limits` implementation replaces four with eight. This raises PostgreSQL connections, memory pressure and source load despite the profile module's explicit non-raising contract. The `enabled` flag was also ignored by `read_limits`.

Evidence: At the base commit, `TeslaMateConfig::read_limits` assigned `max_parallel_copy_lanes` directly to `limits.parallel_copy_lanes`; the existing test expected four to become eight. `src/performance_profile.rs` derives lanes with `min` and states that profile discovery never raises configured safety limits.

TeslaMate comparison, if applicable: TeslaMate is only the read-only source here; the defect affects Hub migration resource control rather than TeslaMate semantics.

Remediation: Commit `0acef368...` independently validates a supplied maximum, preserves configured lanes when profiling is disabled and applies `min(configured, maximum)` when enabled.

Tests or validation: Replaced the old raising assertion with `config::tests::teslamate_read_limits_only_apply_a_non_raising_parallel_lane_cap`, covering a higher cap, a lower cap, disabled profiling and invalid zero. Not run.

Compatibility/rollback: No storage or wire change. Imports that accidentally used more lanes now use at most the configured count. Rollback would restore unexpected source load and is not recommended.

## HUB-004

ID: HUB-004

Severity: P2 — material

Confidence: confirmed

Status: blocked

Affected paths: repository settings; `.github/`; `GITHUB_REPOSITORY_SETTINGS.md`

Invariant: Required correctness/security checks and owner review must be enforceable before changes reach `main`.

Failure scenario: A change bypasses the documented review/check requirements because `main` is unprotected and no committed Actions workflow supplies the required Rust/macOS gates. Repository prose cannot enforce merge controls or prove a reviewed commit.

Evidence: The baseline GitHub branch response reported `main` as unprotected; the complete tree contained issue/pull-request templates but no workflow file. `GITHUB_REPOSITORY_SETTINGS.md` describes branch protection/check controls as required.

TeslaMate comparison, if applicable: Not applicable.

Remediation: Blocked in this pull request because branch protection is external repository infrastructure and no unreviewed CI architecture is being introduced as a side effect of the code audit. The owner must configure protection and an approved CI workflow separately or before merge.

Tests or validation: Re-query branch protection and exact pull-request-head workflow/status checks before merge.

Compatibility/rollback: No code change. Enabling branch protection can affect maintainer workflow; document emergency/bypass ownership and required checks before applying it.
