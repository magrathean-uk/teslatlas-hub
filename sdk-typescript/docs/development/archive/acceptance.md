# Historical SDK acceptance

Acceptance belongs to an exact source revision, package digest, and runtime environment. The records linked here are historical evidence. They are not proof that the current checkout builds, that a local Hub is running, or that any remote deployment works.

## Recorded checks

| Record | Bounded result | Limit |
| --- | --- | --- |
| [macOS packed Node/browser G4](development/macos-arm64-packed-node-browser-g4-2026-09-18-r1.json) | The recorded packed SDK exercised the current-Hub profile with Node and Chrome on macOS ARM64 | Synthetic source-built Hub; no installed service, real vehicle data, or platform floor claim |
| [Candidate package admission](development/f3-f6-candidate-package-admission-2026-09-19-r1.json) | Admission of a particular archive and source export | Does not prove a consumer runtime or update/rollback lifecycle |
| [Native Linux ARM64 Docker package](development/f6-native-linux-arm64-docker-package-2026-09-20-r1.json) | Bounded package installation/import pass for archive `070906b5e3ead04a32223ca996d88ebf6f22be252821e56ef1839da3a13e23d7` | No final Hub/browser journey, complete platform matrix, publication, or production acceptance |
| [Development snapshot](development/PLAN.md) | Records the 2026-09-22 cleanup and historical semantic comparison | Former external artifacts and runtime fixtures were removed |

The original 2026-09-12 narrative remains in repository history. Its retired fixture paths, one-attempt authorizations, and platform pauses must not be reused as current instructions. Checked-in receipt files retain their original evidence and limitations.

## Current work

In the multi-repository workspace, use the parent programme plan and applicable instructions for current scope. `docs/development/STATUS.json` is a dated SDK snapshot, not the current programme status. Viewer remains outside this SDK's development scope.

For local checks, use [CONTRIBUTING.md](../CONTRIBUTING.md). For a real-Hub journey, obtain a fresh handoff that identifies the exact package and source, endpoint and Hub identity, trust inputs, caller credentials, permitted operations, and cleanup owner. Never reuse a closed invitation, historical private runtime, or receipt as a substitute for a new run.

Keep normal TLS and browser CORS enforcement enabled. A mock, generated schema check, or packed import does not prove ordinary-user behavior. Report only the checks actually run and the exact artifacts they cover.

The older `tools/platform-support.json` acceptance-boundary text says the Docker lane is unrun, while the later Docker receipt records a bounded historical pass. That metadata discrepancy is not a reason to broaden either claim. The [Docker guide](docker.md) explains the package-only boundary.
