# Development

The package declares Swift tools 6.0, iOS 17 and macOS 14. These are manifest requirements, not proof that a particular revision has passed every platform floor. Linux `TeslatlasCurrentHub` uses the C shim and requires OpenSSL-backed libcurl plus libssl/libcrypto headers and libraries.

Run commands from the repository root. In a coordinated Teslatlas workspace, follow its current task order and build-lock rules. Use an external SwiftPM scratch directory; for example:

```sh
SDK_SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-sdk-swift.XXXXXX")"
swift package --scratch-path "$SDK_SCRATCH" dump-package
swift test --scratch-path "$SDK_SCRATCH" \
  --skip CurrentHubLiveTests \
  --skip CurrentHubMatrixWorkerTests \
  --skip CurrentHubAppleConsumerTests \
  --skip LiveHubBlackBoxTests
```

The example leaves its scratch directory available for inspection. Remove only the directory created for your run when finished. When release-mode behavior is relevant, use the same selection with `swift test -c release --scratch-path "$SDK_SCRATCH"` or build with `swift build -c release --scratch-path "$SDK_SCRATCH"`.

## Choose checks for the change

| Change | Relevant checks |
| --- | --- |
| Strict protocol models, routes or commands | `TeslatlasHubSDKTests` |
| Historical Hub-v1 binding or client | `TeslatlasHubV1CompatibilityTests`, excluding `LiveHubBlackBoxTests` |
| Current Hub binding, client or transport | `TeslatlasCurrentHubTests`, excluding the three opt-in suites above |
| Source handoff selection or validation | `tools/test_source_handoff.py` |
| Platform harness | `tools/test_platform_gate.py`, `tools/test_ios_runtime_host.py` |
| Matrix adapter | `tools/test_matrix_adapter.py` |

Use `--filter` for a focused Swift test when appropriate. Retain the live-suite exclusions when selecting an entire test target. Binding changes need matching fixture and hash validation, not only a successful compile.

The Python matrix checks require Python 3.11 or newer with the requirement from `tools/requirements-dev.txt` already available in an isolated environment:

```sh
python3 -m unittest discover -s tools -p 'test_matrix_adapter.py'
```

The other Python suites use the same discovery syntax with their own filename. Some harness checks invoke SwiftPM or Xcode tools; read their scope before selecting them.

## Live checks

`CurrentHubLiveTests`, `CurrentHubMatrixWorkerTests`, `CurrentHubAppleConsumerTests` and `LiveHubBlackBoxTests` are separate, provisioned journeys. Do not remove exclusions merely to make the command shorter. Current-Hub live tests require private configuration and a suitable reachable Hub; the legacy live test requires the exact historical binding. See the relevant client guide and [consumer example](../Examples/CurrentHubConsumer/README.md).

Use dedicated test credentials and synthetic data for state-changing pairing and rotation checks. Preserve ordinary user credentials and any live service. Never interpret fixture success as live-Hub, installer or physical-device acceptance.

## Source and platform checks

[Source distribution](source-distribution.md) explains the current-tree handoff. [Platform gates](development/platform-gate-preparation.md) explain the separately pinned historical snapshot used by `tools/platform_gate.py` and the Dockerfile. A platform-gate result for that snapshot does not validate a newer checkout.

GitHub is source storage. Local checks are the validation path; no hosted build or test automation is part of this workflow.
