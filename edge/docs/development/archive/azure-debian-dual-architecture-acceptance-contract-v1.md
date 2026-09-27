# Azure Debian dual-architecture acceptance contract v1

This contract prepares the future Debian 13 `amd64` and `arm64` VM runs. It
does not provision Azure resources, publish an image, connect a vehicle, or
authorize a release. Hub owns the fixture, target access, execution, and final
acceptance decision once the owner supplies both fresh VMs.

## Sequencing gate

Azure is the final Debian bootstrap, `.deb`, and Docker gate. Do not request
Azure access or use a supplied VM until all remaining local development and
Hub integration work is complete, the current macOS gate has a recorded result,
and the VPS verification has a recorded result. Neither Azure VM can replace
or defer those Mac or VPS checks. The Hub coordinator must attach their
receipts to the Azure identity manifest before bootstrap. A source build or
read-only VPS inventory alone does not satisfy either gate; a missing,
blocked, or failed prerequisite makes the Azure run **blocked**.

The current proposed VPS AMD64 candidate is documented in
[the isolated VPS contract](vps-amd64-isolated-candidate-v1.md). Its owner
authorization, fixture inputs, execution, and clean receipt must exist before
the VPS prerequisite becomes complete.

The VMs can establish the two Debian installed and Docker architecture rows.
They do not establish the still-required macOS arm64 row or passive
vehicle-wire provenance.

## Run isolation and preconditions

Use one fresh VM and one private run root for each target:

| Target cell | Required processor evidence | Permitted execution |
| --- | --- | --- |
| `edge_v2__debian13_amd64` | Debian 13, `uname -m` equivalent to x86_64, native container image | native only |
| `edge_v2__debian13_arm64` | Debian 13, `uname -m` equivalent to aarch64, native container image | native only |

Run no test against a production Hub, Edge, vehicle receiver, spool, credential,
or certificate. The Hub fixture must use its reviewed synthetic source and keep
normal and `edge-test-faults` actors separate. Give every target a distinct
fixture root, Hub store, Edge state directory, credentials, certificates,
ports, and evidence directory. Do not copy an initialized spool or a private
key between architectures.

Before a target starts, Hub records a private, owner-only identity manifest.
The manifest must contain these values without secret contents:

| Identity | Required record |
| --- | --- |
| Edge source | Git HEAD, dirty-source inventory digest, `edge.py` SHA-256 `926a0b876a9d06a28474ef4d43ec04d3f7414f0d16c9e25b0e66ff8a86cc19be`, and the paired [source handoff](../../.superpowers/sdd/2026-09-08-working-product-plan/edge-arm64-coordinator-handoff-2026-09-08.json) digest |
| Edge contract | Matrix, validator, phase, vector, and six raw-schema hashes from that handoff |
| Build inputs | Rust and pinned Go identities, Edge/bridge binary SHA-256 values, build logs, and `teslatlas-edge storage-format` output |
| Container inputs | Docker Engine and Compose versions, Dockerfile/Compose digests, image config digest/ID, image architecture, and a proof that the image matches the target architecture |
| Runtime inputs | Kernel and Debian release identity, cgroup mode, effective UID/GID, selected ports, and native/emulated result |
| Configuration | Redacted config and fixture hashes, separate TLS role inventory, secret-file mode/owner table, and SHA-256 values of secret files only; never capture their bytes |
| State baseline | Empty-state directory inode/device identity, spool/key/credential file inventory, permissions, and hashes after `init` but before fixture delivery |
| Hub fixture | Reviewed fixture/runner/observer source identities, job/session/actor bindings, target cell, and the normal/fault binary identities |

Abort a target as **blocked** before workload execution if any required identity
is absent, the source pin differs, the target is emulated, or any state path is
shared across target cells. A changed identity creates a new run; it is never
silently added to an existing receipt.

## Evidence rules

The Hub runner writes one owner-only receipt per target and phase. Every receipt
contains this closed record:

```json
{
  "schema_version": 1,
  "run_id": "unique run identifier",
  "cell_id": "edge_v2__debian13_amd64 or edge_v2__debian13_arm64",
  "phase": "named phase below",
  "status": "passed, failed, blocked, or pending",
  "identity_manifest_sha256": "sha256",
  "started_at": "UTC timestamp",
  "finished_at": "UTC timestamp",
  "commands": [{"argv_sha256": "sha256", "exit_code": 0, "stdout_sha256": "sha256", "stderr_sha256": "sha256"}],
  "observations": [{"kind": "named observation", "path": "owner-only path", "sha256": "sha256"}],
  "cleanup": {"status": "passed, failed, blocked, or pending", "evidence_sha256": "sha256"}
}
```

The final per-target receipt must reference every phase receipt by path and
SHA-256, list all passed/failed/blocked cases, and state whether normal and
fault actors ran. Evidence may retain aggregate IDs, counts, hashes, status
codes, and fixed synthetic fixture identifiers. It must not retain raw
telemetry, VINs, bearer values, private keys, certificates, or decoded payloads.

An interrupted phase is **failed** unless the receipt proves that no durable
mutation began; it is never inferred to be clean from process exit status.

## Required phases

### 1. Bootstrap and containment

Install the reviewed package or build, then verify the exact binaries/image,
non-root Edge identity, owner-only state/config/secrets, listener topology,
and separation between receiver and Edge private mounts. Capture the initial
state inventory after `init` and `doctor`. Confirm that no Tesla credential or
vehicle-command material is present.

For Docker, prove UID/GID `10001:10001`, receiver access only to its bearer and
vehicle TLS mounts, Edge-only access to spool/key and Hub credentials, and
loopback-only health/readiness. For native installation, prove the same access
boundary through service identity, mode/owner inventory, and process/listener
observation. A configuration parse, image build, or healthy listener alone
does not pass this phase.

### 2. Functional normal lane

Run the reviewed Hub synthetic fixture through its normal actor. Require the
fixed `edge_v2` contract cases to produce evidence-bound results:

- `candidate_artifact_identity`, `installed_service_runtime`, and
  `edge_linux_runtime`;
- `edge_mtls_bearer_identity`, `edge_no_v1_downgrade`, and
  `edge_source_owner_exclusivity`;
- `edge_uninterrupted_delivery_parity`, `edge_restart_delivery_parity`,
  `edge_duplicate_reenqueue_dedup`, and
  `edge_gap_interleaving_later_ack_rejected`;
- `edge_unsupported_event_bounded_disposition`,
  `edge_conflicting_identity_blocks_ack`, and
  `edge_clean_shutdown_resume`.

Retain the Edge durable admission boundary, Hub application/disposition,
receipt, contiguous frontier, request/response summaries, and aggregate spool
counts. The receipt must bind each result to its actor, raw evidence, and the
controller observation. A fixture must prove the ARM64 coordinator case as
`arm64` on the ARM64 VM and `amd64` on the AMD64 VM.

### 3. Restart and persistence lane

With pending synthetic work and a Hub outage/retry boundary, separately test:

1. Edge restart;
2. receiver restart;
3. Hub restart;
4. full ordered stop and start; and
5. Docker replacement or native binary service replacement that preserves the
   same state directory.

For each action, capture pre/post state inventories, pending/receipt/frontier
counts, process and listener evidence, shutdown elapsed time, and the final
Hub result. The record must show that a retry does not delete an unacknowledged
spool occurrence and that recovery drains only after Hub's durable result.
Preserve the five-second application drain and ten-second service-manager
allowance as measured evidence, not assumed configuration facts.

### 4. Fault lane

Run the reviewed fault actor in a new isolated fixture root. It must provide
separate receipts for:

- `edge_transaction_fault_redelivery` at every registered transaction point;
- `edge_lost_ack_body_redelivery`;
- `edge_pending_publication_offline_recovery`; and
- the negative authentication, v1 route/body/redirect, identity-conflict, gap,
  duplicate, and unsupported-event boundaries required by the contract.

Each fault receipt must include before/after aggregate state, injected fault
identity, process observation, raw witness binding, recovery action, Hub
receipt/frontier result, and cleanup result. A fault run passes only when the
expected fail-closed or redelivery outcome is observed and all test resources
are cleaned up.

### 5. Upgrade and rollback boundaries

Use separate disposable state roots for each of these cases:

1. a receipt-free format-2 spool migrates to format 3 while preserving pending
   state and marking old v1 history unknown/pruned;
2. a format-2 spool with receipts stops with `ReceiptRecoveryRequired` before
   recovery or marker replacement, preserving marker, receipts, and pending
   bytes;
3. a format-3 spool rejects an older binary before it opens state; and
4. a documented rollback restores a complete pre-upgrade backup into a new,
   empty state root and reaches the expected readiness result.

Record format marker bytes only as a hash plus declared version, file counts,
spool/key identity hashes, and pre/post inventory hashes. Do not delete or
reconcile receipts to force a pass.

### 6. Cleanup and uninstall

After every test phase, stop receiver before Edge, wait for the bounded stop,
and retain final process/listener, state inventory, and Hub completion evidence.
For the final fresh-VM uninstall case, first make and verify an encrypted
backup of the test state. Then prove services/containers, listeners, temporary
fixture resources, test certificates, and ephemeral credentials are absent.
Record the removed-path inventory and the preserved backup manifest, never the
backup's secret contents. Do not use `down -v`, state deletion, or an uninstall
as recovery during any earlier phase.

## Acceptance decision

Hub may mark a Debian target **passed** only when every required phase has a
matching identity manifest, complete receipt chain, clean cleanup evidence, and
the expected runtime architecture. Any unavailable capability, failed cleanup,
or omitted evidence produces **blocked** or **failed**, with no substitute
claim from source tests or a container build.

After both Debian VMs pass, report them as Debian installed/Docker architecture
evidence only. Keep macOS arm64, passive vehicle ingress, release, and public
publication status separate until their own evidence exists.
