# Edge matrix contract handoff

This is the Edge-owned handoff for the reviewed `edge_v2` matrix publication.
It is source evidence for the Hub integrator; it does not authorize an
installed run or change Hub-owned files.

Source checkout: Edge `main`, baseline HEAD
`67650cd989f25835bd185d0d8c0ff4e3c8ff1bb2` (working tree contains the
authorized Edge changes). The fixed coordinator and contract are:

| Path | SHA-256 |
| --- | --- |
| `tools/interop/client_lanes/edge.py` | `926a0b876a9d06a28474ef4d43ec04d3f7414f0d16c9e25b0e66ff8a86cc19be` |
| `tools/interop/client_lanes/edge_contract/matrix-contract.json` | `328c62d724c96011ba06c4d4d1fae6b362dae4e0e740323bf17e8ad3d8cf8915` |
| `tools/interop/client_lanes/edge_contract/matrix_contract.py` | `bf1219b1766e3f2af0f90d45903e961117bcce3275fc2e0424dd6e146851605f` |
| `tools/interop/client_lanes/edge_contract/phases.json` | `15000c3bfdc02578426c8484f2cc96f2152f9a89d66cb70a1862ef675a98dbf5` |
| `tools/interop/client_lanes/edge_contract/edge-installed-vectors.json` | `d40a853cd02111ff200083be81588526dc7d2cf28c3daa168d2ea7dd9dda6eb1` |

The manifest's six raw schema bindings are:

| Schema ID | File SHA-256 |
| --- | --- |
| `edge_fixture_v1` | `5f65a6a91e8a580ffe81bb5ee922ef39505ebaa19c232a80603efe822c9c9db6` |
| `edge_checkpoint_v1` | `859d2a6f90a8c610c6810608e9bf63cf1e0f0670e6e50262c88f7190a31d2f41` |
| `edge_http_v1` | `bb4e3c87df84053fe7a298ca5fc42c99424533b5da83d89372a50dcd75d2be0a` |
| `edge_fault_v1` | `9eba37bec670157bd9641be14ee8cea446162c4c822ed503f95cf8b2909e4152` |
| `edge_snapshot_v1` | `6279358ff76721ca0b97bba86bfc5573bf83e4dd88779b5c94d375d0ebf7b19e` |
| `edge_cleanup_v1` | `3d6d1fcece6772bb1125c5a79027caab7b2a12726063a291b231317b10ce1ce8` |

The coordinator consumes the common `matrix-adapter-session` input, requires
the staged `edge_executable` and its artifact-bound installed manifest, and
does not accept executable text, argv, environment names, target paths or
fault switches from job data. It runs the fixed 31-phase table over one Unix
broker attachment, reads actor evidence from deterministic
`actors/<actor_id>/phase-<six-digit-ordinal>-<schema_id>.json` paths, binds raw
evidence and controller proof hashes, writes normalized/actor/completion
files, then waits for the runner's accepted `close_completed` acknowledgement.

Hub owns the shared registry/import, installed fixture and launch inventory,
the four-lane Edge fixture, process/namespace/store observers, cleanup and all
target acceptance. The current Hub checkout still lacks
`hub/tests/interop/edge_installed`; Docker daemon/runtime, service-manager,
vehicle receiver-wire and installed target evidence remain open.
