# TypeScript T7 installed-cell handoff

Status: preparation complete; no target was started and no matrix cell is claimed.

This handoff is limited to the six TypeScript installed cells. It is not authority to install a package, start an installed host, run a browser, create a fixture, or reuse B1/R1 private material.

## Fixed runner contract

`installed_registry.py` contains the only two entries for this handoff:

| Adapter | Actor | Reviewed contract | Required cases | Target execution |
| --- | --- | --- | ---: | --- |
| `typescript_node` | `sdk_node` | JSON `7800f75b03e046f53e56b7356fac95089aae73ab7719e82ead88f39e45ae3299`; validator `600e1cc363d309e163b87e19498e1daf4ba309c325aeb8402513168ff1c12ef2` | 21 | `local` on all three Hub targets; runner-owned Unix broker |
| `typescript_browser` | `sdk_browser` | JSON `3a04e33415330d2d3ce53d489841f0a28b768d982d2af5f255323d2f0cf6acb9`; validator `784d1618efdc6739248426d7ee6538772a3560c1569c903f75fb2af72755bf92` | 23 | `local` on all three Hub targets; runner-owned Unix broker |

Both entries bind exactly these six callbacks: `build_session_input`, `launch_adapter`, `runtime_inventory`, `admit`, `build_supplement`, and `execution_logs`. Runtime JSON cannot select another executable, validator, broker, actor, output directory, or callback.

The Hub TypeScript lane is pinned to these reviewed bytes: `run.mjs` `7faa39b56310c0a2cec1de36327743cae9700bb19cd1118993aed8cffb20afe3`, `installed_contract.mjs` `cc8f124090655d9c86ff9861ccb826fcc88a7bb2839db435ffbc3882a13f8fb5`, `typescript_lane.mjs` `55833f7d8dc16434009d13c7398ddd40404fc836082e1bbe39083a9ac0addcf2`, `scenarios.mjs` `f04aa42956d1476dc3fbd21eab60200bf12cf3e823fdddfcc472aaf0f85b2dcd`, `node-worker.mjs` `f1dfea5570dd93906028b5a3214dc0aecf241f98a7ccc41b2a52e1f395f161bf`, and `browser.mjs` `2a698cb07adec2ec336b2ffb15208a62c135f721cfc43f34ce382a7302022677`.

Every cell must use the 81-member `@teslatlas/sdk@2026.36.2` archive `03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c` and the matrix-pinned Node `v26.7.0`. The immutable archive may be copied from a trusted source, but each cell must verify a distinct installed `@teslatlas/sdk` package root against it.

## Independent cell inputs

| Cell | Hub target | Required Hub service mode | Additional private environment inputs |
| --- | --- | --- | --- |
| `typescript_node__macos_arm64` | macOS arm64 | installed app + LaunchAgent | target-local SDK package root |
| `typescript_node__debian13_arm64` | Debian 13 arm64 | installed deb + systemd | target-local SDK package root |
| `typescript_node__debian13_amd64` | Debian 13 amd64 | installed deb + systemd | target-local SDK package root |
| `typescript_browser__macos_arm64` | macOS arm64 | installed app + LaunchAgent | SDK package root, remote root, SSH config/alias, Playwright entry, local log |
| `typescript_browser__debian13_arm64` | Debian 13 arm64 | installed deb + systemd | SDK package root, remote root, SSH config/alias, Playwright entry, local log |
| `typescript_browser__debian13_amd64` | Debian 13 amd64 | installed deb + systemd | SDK package root, remote root, SSH config/alias, Playwright entry, local log |

Each cell needs fresh, non-aliased owner-only bindings for its environment JSON, installed-session configuration, registration inventory, host registration/controller bundle, descriptor, target-local package installation, scratch root, output root, and receipt. It must have a distinct session ID, instance nonce, guest run ID, certificate copy, scenario/profile copy, artifact manifest, command descriptor, and evidence tree. No registration, broker socket, private input, process, browser state, output, or receipt can be shared between cells.

There are deliberately no pre-created `SessionInput` files. The fixed adapter creates one only after the shared runner has opened the selected host session: it derives an owner-only fresh directory from the new broker socket/session ID, stages the live certificate and current profile/scenario/product bindings, and writes `session-input.json` with exclusive `0600` files beneath a `0700` root. A pre-created file would have to invent the new descriptor/certificate/session data or open a target early. Both are outside this preparation-only authorization.

## Required evidence per started cell

All 21 Node cases are required. Browser cells add real CORS and normal browser TLS validation for 23 total. The owner receipt must prove:

- malformed/wrong, expired, replayed, and revoked credential behavior;
- real claim, credential loss re-authentication, and rotation with old credential rejection;
- identity/discovery, exact current values, 25/25/1 pagination, terminal/foreign cursor handling, and conditional ETag `304` continuation;
- Hub restart and outage recovery with a fresh process observation;
- runner-owned clean close, final stopped proof, child-process absence, and endpoint closure;
- for browser: real preflight/CORS and ordinary trusted-vs-untrusted TLS behavior.

The runner must bind observed native OS/architecture/service-manager identity, candidate Hub executable hash/version, installed SDK manifest, Node/browser runtime identity, profile, source identities, and every pre/post-run artifact/source check. The selected Debian ARM64 B1 Node consumer and any Viewer recovery receipt are explicitly excluded: they are not installed-matrix rows and cannot satisfy a T7 cell.

## Start gate

Before an owner starts one named cell, it must supply a fresh approved target, actual installed Hub/service observation, target-local candidate artifact and package-root inspection, a new private registration/session set, and a fresh private environment file. Browser cells additionally require their own SSH/Playwright binding. The registry then creates the real `SessionInput` during dispatch; it is not a hand-authored template.

The next action is not a launch. It is an explicit per-target authorization with those fresh inputs, beginning only after the canonical plan's prerequisite gate permits the named matrix row.
