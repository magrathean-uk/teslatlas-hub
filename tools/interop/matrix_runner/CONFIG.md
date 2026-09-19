# Matrix runner private config

`config.schema.json` dispatches two closed contracts. Version 1 preserves the
historical deterministic, interim, and direct native runner behavior. Version
2 is reserved for installed acceptance and requires a hash-bound
`cohort_inputs` manifest plus `installed_session` and `client_execution` on
every job. Its aggregate receipt repeats the exact cohort binding and requires
a per-cell runner-owned supplement field; a null supplement keeps the row
incomplete. The config and its `environment_file` inputs must be owner-only
regular JSON files.
The environment file is a JSON object of environment variable names to string
values; it is the private boundary for credential/config paths and other
adapter settings. Raw credentials must remain in separately referenced private
files and must never appear in `argv`.

Every source and artifact has a typed `role`. Actual runs require the exact
workspace checkout for each source role. A version 1 `hub_executable` artifact
is probed with its fixed `--version` interface. Version 2 first admits the
immutable installed-host registration and package/target identity, so a
foreign Linux executable is never invoked on macOS. A `typescript_sdk_tarball` is
inspected at `package/package.json`. Digest equality without the matching
embedded product identity is rejected.
Companion packages use the same fixed archive metadata check. The current Swift
product is an SDK plus `TeslatlasHubSDKExample`; it has no supported standalone
client executable/version-probe contract in this runner. Foreign-host Hub and
Edge executable observations are also unsupported until a host-bound verifier
exists, so those jobs remain pending before local execution. The Home Assistant
archive binds `custom_components/teslatlas_hub/manifest.json` with domain
`teslatlas_hub`.
`protocol_fixture_seed` is explicitly tooling with `embedded_version` set to
`tooling`; it cannot masquerade as a calendar-version product artifact.

Every job names one cell from `hub/docs/compatibility/matrix.json`. `argv` is a
structured argument array and `command_files[0]` binds its executable. Actual
protocol jobs execute the exact conformance wrapper with the `actual-hub`
adapter selected. Actual Node/browser jobs execute the exact reviewed
`client_lanes/run.mjs` entrypoint and an owner-only descriptor whose mode and
evidence output are bound to the job. All source
identities, artifacts, and command files are rehashed before and after the
child. Config, environment, evidence, logs, descriptors, and aggregate receipt
must be outside the entire workspace source root and cannot alias each other.
`evidence_path` must be absent and is
created during this run. `stdout_json` saves bounded stdout as the evidence;
`file_json` requires the child to create the bounded owner-only JSON evidence.
Each child gets a new owned process group. The runner cleans every live group
member after normal exit, failure, timeout, or interruption. Bounded owner-only
stdout, stderr, and command-outcome records are retained beside the evidence.

Version 1 `actual_hub_acceptance` retains its historical direct-client meaning.
Version 2 uses the installed lifecycle: registered session open, initial
verified observation, one adapter attachment, immutable evidence readiness,
runner close with independent stopped proof, exact acknowledgement, then child
exit zero. Source snapshots, retained build inputs, exports, and outputs are
revalidated before and after the installed matrix. Unsupported reviewed
adapter registries remain pending and cannot make an 18-cell receipt complete.

`actual_hub_acceptance` enforces fixed adapter and target identities. A bounded
owned-user-process Hub may execute actual client cases, but there is no fixed
installed-host verifier in this slice. `installed_service_runtime` therefore
always remains pending, regardless of caller-supplied process/service-looking
JSON, until a later host slice invokes and binds such a verifier. That future
verifier must observe PID, UID, start time, executable hash/version, service
manager unit/plist state, listener, and config identity on the actual Hub host.
`interim_actual_protocol_smoke` permits
only the protocol adapter and can never produce a complete matrix. The latter
exists for bounded native fixture checks while final packages are frozen.
`deterministic_runner_test` exercises schema and admission rules only and is
always labelled that way in the receipt.

Normalized adapter evidence has these exact top-level keys:

```json
{
  "schema_version": 1,
  "execution_kind": "actual_hub_acceptance",
  "adapter": "protocol_actual_hub",
  "cell_id": "protocol_actual_hub__macos_arm64",
  "product_version": "2026.36.2",
  "profile_id": "hub-http-v1",
  "profile_revision": "1.0.0",
  "profile_sha256": "64 lowercase hexadecimal characters",
  "source_identities": [],
  "artifacts": [],
  "runtime": {},
  "cases": []
}
```

Each case has exact keys `id`, `status`, `expected`, `actual`, `evidence_kind`,
and `request_transcript`. A transcript entry has exact keys `method`, `route`,
`status`, and `request_id`. A passed `http` case requires independently derived
equal expected/actual values, fixed per-case required facts, and at least one
operation-bound request entry. Empty, null-only, and unrelated health-request
assertions are rejected. The Node and browser adapters have exact typed
assertion allowlists bound to the reviewed interoperability scenario, including
nulls, zero values, Unicode, drive page order, terminal cursor state, lifecycle
booleans, and integer HTTP statuses. Other adapters remain pending until their
case contracts exist. An
`identity` case instead has an empty transcript and a nonempty
`process_evidence` object binding the verified file/process facts. A genuine
actual-client preflight rejection may use `zero_request` with an empty
transcript and equal `{outgoing_requests: 0, typed_error: ...}` values. Each row
must still contain real transport evidence. The declared unsupported-operation
zero-request cases are stricter: they require an empty transcript and exact
`{"outgoing_requests": 0}` values. All matrix cases are required, so
`not_applicable` cannot discharge any of them. A required case omitted by a
valid adapter remains pending and keeps `--require-complete` nonzero.
