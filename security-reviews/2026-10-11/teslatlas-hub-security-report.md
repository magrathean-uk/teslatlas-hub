# Security Review: hub

## Scope

Offline Standard single-pass Hub repository scan at current HEAD.

- Scan mode: repository
- Target kind: git_revision
- Target ID: target_sha256_395efdefb7ebbfebc2a749c9acde8bb03b9ac4c8a6c36d8b7a6cec08cdce74e4
- Revision: f269f6641004f25996ad3e84802fd6a2e56b4312
- Inventory strategy: repository
- Included paths: .
- Excluded paths: none
- Artifacts reviewed: src/api, src/auth, src/storage, src/collection, src/sync, macos, docs/operations

Limitations and exclusions:
- No builds, private lab, source/vehicle network, cloud calls, secrets or exploit execution.
- Retired installer/Edge/Viewer and packaging reachability not fully validated.
- Repository coverage is partial.

### Scan Summary

| Field | Value |
| --- | --- |
| Scan outcome | completed |
| Reportable findings | 2 |
| Severity mix | medium: 1, low: 1 |
| Confidence mix | high: 1, medium: 1 |
| Coverage | partial |
| Validation mode | static_source_trace |

Canonical artifacts: `scan-manifest.json`, `findings.json`, and `coverage.json`. This report is a deterministic projection of those files.

## Threat Model

Teslatlas Hub is a Rust telemetry and sync service with an AppKit control app. It ingests vehicle/Fleet/TeslaMate data, stores credentials and history locally, serves a read API and immutable sync packs to paired devices, and supports optional encrypted backups.

### Assets

- Tesla/Fleet credentials and legacy token pairs
- Per-vehicle telemetry, routes and location history
- Pairing secrets, device bearer credentials and vehicle grants
- SQLite catalogue, sync packs and encryption/recovery material
- TLS private keys and backup generations
- Local vehicle command authority and audit receipts
- Conditional privileged AppKit installer path

### Trust Boundaries

- Remote Fleet/provider or TeslaMate data to Hub parsing and local store
- Unauthenticated/paired HTTP requests to API handlers, packs and vehicle grants
- Loopback control surface versus private-LAN TLS paired surface
- AppKit/CLI operator inputs to filesystem, backup, restore and process launch
- Hub to paired App/Home Assistant protocol and transport
- Operator-confirmed local vehicle commands through mode-0600 Unix socket to provider actions
- Default AppKit installer wrapper to admin package installer (shipped reachability unverified)

### Attacker Capabilities

- Unauthenticated network client may reach configured listener if exposed
- Paired client may hold vehicle-limited bearer and send malformed requests
- Compromised external provider or TeslaMate source can supply malformed/large data
- Local process/user may interact with loopback/API or operator-selected paths, subject to OS permissions

### Security Objectives

- Authenticate paired requests and enforce per-device car scope
- Keep token, TLS and backup secrets private
- Bound untrusted request and import resource use
- Prevent path traversal or integrity loss in packs and backup/restore
- Keep loopback/development/private-LAN modes isolated and explicitly gated

### Assumptions

- Offline read-only static review; no builds, networks, vehicle, private lab or secret reads.
- Retired installer, Edge and Viewer are tracked as historical context; active Mac Hub prioritized.
- No browser client operated by product in v7.
- Current source exposes vehicle commands and installer wiring despite retirement language in AGENTS; neither mismatch is itself a validated exploit.

## Findings

| Finding | Severity | Confidence | Detailed write-up |
| --- | --- | --- | --- |
| [Attacker-writable backup parent allows staging-path substitution](#finding-1) | medium | medium | inline below |
| [Backup verification does not authenticate a substituted generation](#finding-2) | low | high | inline below |

### Confidence Scale

| Label | Meaning |
| --- | --- |
| high | Direct evidence supports the finding with no material unresolved blocker. |
| medium | Evidence supports a plausible issue, but material runtime or reachability proof remains. |
| low | Evidence is incomplete and the item is retained only for explicit follow-up. |

<a id="finding-1"></a>

### [1] Attacker-writable backup parent allows staging-path substitution

| Field | Value |
| --- | --- |
| Severity | medium |
| Confidence | medium |
| Confidence rationale | Parent admission, pathname staging and plaintext copy sequence are source-backed; exploit timing and actual shared-parent deployment were not tested. |
| Category | filesystem_race |
| CWE | CWE-367 |
| Affected lines | src/storage/data_recovery.rs:579, src/storage/data_recovery.rs:297, src/storage/db/maintenance.rs:515 |

#### Summary

When an operator selects a non-sticky backup parent writable by another local actor, that actor can rename/replace the randomly named staging directory after creation. Subsequent pathname-based backup writes can land in an attacker-controlled directory, exposing the plaintext catalogue and vehicle history. The documented private parent is not affected.

#### Root Cause

Backup accepts a writable parent and later uses an unpinned staging pathname for private-data writes.

**parent** — `src/storage/data_recovery.rs:579-599`

Destination parent is checked as a directory but not for ownership, write access by others, or sticky-bit isolation.

```rust
if !parent_metadata.file_type().is_dir() ...; let parent = parent.canonicalize()?;
```

**stage** — `src/storage/data_recovery.rs:206-245`

Staging is a pathname child of the selected parent, with no retained directory descriptor.

```rust
let path = parent.join(format!(".teslatlas-data-recovery-{}.staging", Uuid::new_v4())); builder.create(&path)?; ... no_replace_rename(&self.path, destination)?;
```

**copy** — `src/storage/data_recovery.rs:297-299`

Backup writes are addressed through that mutable staging pathname.

```rust
let staging = StagingDirectory::create(&parent)?; let payload = staging.path.join(DATA_DIRECTORY); backup_snapshot.copy_to(&payload)?;
```

**sqlite** — `src/storage/db/maintenance.rs:515-548`

Plaintext catalogue is created via pathname beneath the staging path.

```rust
fs::create_dir(destination)?; ... let catalogue = destination.join("hub.sqlite"); self.backup_catalogue_to(&catalogue)?;
```

**packs** — `src/storage/db/maintenance.rs:559-589`

Referenced immutable packs are also copied by mutable path.

```rust
let backup = packs.join(&filename); fs::copy(&source, &backup)?;
```

#### Validation

Traced parent admission through staging creation, pathname copy, SQLite backup and pack copy.

Validation method: static_source_trace

**parent** — `src/storage/data_recovery.rs:579-599`

Destination parent is checked as a directory but not for ownership, write access by others, or sticky-bit isolation.

```rust
if !parent_metadata.file_type().is_dir() ...; let parent = parent.canonicalize()?;
```

**stage** — `src/storage/data_recovery.rs:206-245`

Staging is a pathname child of the selected parent, with no retained directory descriptor.

```rust
let path = parent.join(format!(".teslatlas-data-recovery-{}.staging", Uuid::new_v4())); builder.create(&path)?; ... no_replace_rename(&self.path, destination)?;
```

**copy** — `src/storage/data_recovery.rs:297-299`

Backup writes are addressed through that mutable staging pathname.

```rust
let staging = StagingDirectory::create(&parent)?; let payload = staging.path.join(DATA_DIRECTORY); backup_snapshot.copy_to(&payload)?;
```

**sqlite** — `src/storage/db/maintenance.rs:515-548`

Plaintext catalogue is created via pathname beneath the staging path.

```rust
fs::create_dir(destination)?; ... let catalogue = destination.join("hub.sqlite"); self.backup_catalogue_to(&catalogue)?;
```

**packs** — `src/storage/db/maintenance.rs:559-589`

Referenced immutable packs are also copied by mutable path.

```rust
let backup = packs.join(&filename); fs::copy(&source, &backup)?;
```

Counterevidence and remaining uncertainty:
- Random staging name and 0700 mode protect private parents.
- Final publication uses no-replace rename; it does not pin prior writes.

Limitations:
- No runtime race proof; requires non-sticky shared parent.

#### Dataflow

Staging created, then copy_to and SQLite/pack copies resolve the path again.

- **Source:** Mutable staging child under attacker-writable parent

- **Sink:** Plaintext catalogue and pack writes

- **Outcome:** Backup data may be redirected to actor-readable path

**parent** — `src/storage/data_recovery.rs:579-599`

Destination parent is checked as a directory but not for ownership, write access by others, or sticky-bit isolation.

```rust
if !parent_metadata.file_type().is_dir() ...; let parent = parent.canonicalize()?;
```

**stage** — `src/storage/data_recovery.rs:206-245`

Staging is a pathname child of the selected parent, with no retained directory descriptor.

```rust
let path = parent.join(format!(".teslatlas-data-recovery-{}.staging", Uuid::new_v4())); builder.create(&path)?; ... no_replace_rename(&self.path, destination)?;
```

**copy** — `src/storage/data_recovery.rs:297-299`

Backup writes are addressed through that mutable staging pathname.

```rust
let staging = StagingDirectory::create(&parent)?; let payload = staging.path.join(DATA_DIRECTORY); backup_snapshot.copy_to(&payload)?;
```

**sqlite** — `src/storage/db/maintenance.rs:515-548`

Plaintext catalogue is created via pathname beneath the staging path.

```rust
fs::create_dir(destination)?; ... let catalogue = destination.join("hub.sqlite"); self.backup_catalogue_to(&catalogue)?;
```

**packs** — `src/storage/db/maintenance.rs:559-589`

Referenced immutable packs are also copied by mutable path.

```rust
let backup = packs.join(&filename); fs::copy(&source, &backup)?;
```

#### Reachability

Parent-level rename authority defeats child mode 0700 when writes are pathname-relative.

- **Attacker:** Local actor with write/rename authority over the non-sticky destination parent

- **Entry point:** Operator-selected backup destination parent

- **Outcome:** Plaintext vehicle/history backup can be written to actor-controlled storage

Preconditions:
- Operator chooses a parent writable by another actor and not protected by sticky semantics
- Actor observes the staging child and substitutes its pathname before backup copy

Limitations:
- Default documented mode-0700 parent does not satisfy attacker prerequisite.
- No race execution or live backup was performed.

#### Severity

**Medium** — Confidentiality impact is vehicle/location history, but exploitation requires an unsafe operator-selected shared parent and a timing race.

Lower if backup destination parents are always owner-private or descriptor-relative writes already prevent substitution in the deployed path.

#### Remediation

Reject backup parents writable by untrusted principals (including unsafe ACLs), or pin parent/staging directory descriptors and perform all backup creation/copies relative to those descriptors without following replaced path components.

Tests:
- Under a non-sticky shared parent, attempt to rename/swap staging during backup and assert no data writes escape the original inode.
- Verify normal private-parent backup and no-replace publication still work.

<a id="finding-2"></a>

### [2] Backup verification does not authenticate a substituted generation

| Field | Value |
| --- | --- |
| Severity | low |
| Confidence | high |
| Confidence rationale | All inspected backup integrity links are plain hashes; source admission and restore checks validate consistency and schema, not keyed origin. |
| Category | integrity |
| CWE | CWE-345 |
| Affected lines | src/storage/data_recovery.rs:940, src/storage/data_recovery.rs:1048, src/storage/data_recovery.rs:399 |

#### Summary

An actor who can replace an off-host/stored data-backup generation can alter ordinary catalogue data and recompute the member, manifest and completion SHA-256 values. verify-backup and restore-data check consistency but not origin, so a later explicit restore can install falsified vehicle or history data.

#### Root Cause

The backup format has no authenticated signature or MAC anchored outside the backup medium.

**private** — `src/storage/data_recovery.rs:612-625`

Source admission checks real directory and 0700 mode, not an authenticated origin.

```rust
if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE { ... }
```

**hash** — `src/storage/data_recovery.rs:924-963`

Completion marker binds the manifest with recomputable plain SHA-256.

```rust
marker.manifest_sha256 != sha256_bytes_hex(&manifest_bytes)
```

**members** — `src/storage/data_recovery.rs:1039-1060`

Member bytes are checked only against recomputable manifest hashes.

```rust
if sha256_file_hex(&path)? != member.sha256 { ... }
```

**install** — `src/storage/data_recovery.rs:386-475`

A self-consistent source generation is copied into new Hub data and published after checks.

```rust
restore_data_backup(source, destination) ... copy_verified_member(&from, &to, member)?;
```

#### Validation

Traced backup source admission, manifest/member digest checks, restore copy and subsequent database reads.

Validation method: static_source_trace

**restore** — `src/application/main/dispatch.rs:292-304`

Operator-selected backup source enters restore.

```rust
Command::RestoreData { source, destination } => { let report = restore_data_backup(source, destination)?; }
```

**private** — `src/storage/data_recovery.rs:612-625`

Source admission checks real directory and 0700 mode, not an authenticated origin.

```rust
if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE { ... }
```

**hash** — `src/storage/data_recovery.rs:924-963`

Completion marker binds the manifest with recomputable plain SHA-256.

```rust
marker.manifest_sha256 != sha256_bytes_hex(&manifest_bytes)
```

**members** — `src/storage/data_recovery.rs:1039-1060`

Member bytes are checked only against recomputable manifest hashes.

```rust
if sha256_file_hex(&path)? != member.sha256 { ... }
```

**install** — `src/storage/data_recovery.rs:386-475`

A self-consistent source generation is copied into new Hub data and published after checks.

```rust
restore_data_backup(source, destination) ... copy_verified_member(&from, &to, member)?;
```

**read** — `src/api/server.rs:1359-1435`

Paired API later serves database-derived vehicle and current-state data.

```rust
published_vehicles_in_scope(...); current_observations_for_vehicle(vehicle_id)
```

Counterevidence and remaining uncertainty:
- Private 0700 source directory, schema/pack consistency and installation-ID checks apply.
- Recovery is an explicit operator action and does not mint credentials.

Limitations:
- Offline review; no controlled restore or tamper test.

#### Dataflow

Plain hashes are recomputed and validated before copying; no external trust anchor is checked.

- **Source:** Attacker-substituted backup generation

- **Sink:** Restored SQLite catalogue served to paired clients

- **Outcome:** Falsified data can be installed

**restore** — `src/application/main/dispatch.rs:292-304`

Operator-selected backup source enters restore.

```rust
Command::RestoreData { source, destination } => { let report = restore_data_backup(source, destination)?; }
```

**private** — `src/storage/data_recovery.rs:612-625`

Source admission checks real directory and 0700 mode, not an authenticated origin.

```rust
if permission_mode(&metadata) != PRIVATE_DIRECTORY_MODE { ... }
```

**hash** — `src/storage/data_recovery.rs:924-963`

Completion marker binds the manifest with recomputable plain SHA-256.

```rust
marker.manifest_sha256 != sha256_bytes_hex(&manifest_bytes)
```

**members** — `src/storage/data_recovery.rs:1039-1060`

Member bytes are checked only against recomputable manifest hashes.

```rust
if sha256_file_hex(&path)? != member.sha256 { ... }
```

**install** — `src/storage/data_recovery.rs:386-475`

A self-consistent source generation is copied into new Hub data and published after checks.

```rust
restore_data_backup(source, destination) ... copy_verified_member(&from, &to, member)?;
```

**read** — `src/api/server.rs:1359-1435`

Paired API later serves database-derived vehicle and current-state data.

```rust
published_vehicles_in_scope(...); current_observations_for_vehicle(vehicle_id)
```

#### Reachability

Consistency checks do not authenticate who created the backup.

- **Attacker:** Actor able to alter a stored/off-host backup generation but not live Hub data

- **Entry point:** Operator-selected verify-backup or restore-data source

- **Outcome:** Falsified vehicle/current history or lost legitimate records after recovery

Preconditions:
- Actor can replace backup contents before later restore
- Altered generation preserves schema, installation ID, file modes and pack consistency
- Operator explicitly restores that generation

Limitations:
- No proof-of-concept generation was built or restored.
- Pairing/device authority is stripped and credentials are stored separately.

#### Severity

**Low** — Requires attacker control of stored backup media and an explicit operator restore; does not restore credentials or pairing authority.

Raise if untrusted backup transfer is a supported common workflow without independent authenticity verification.

#### Remediation

Authenticate each backup generation with a signature or MAC whose key/trust anchor remains outside the backup medium; verify authenticity before accepting catalogue or members. Document that current verify-backup proves consistency only.

Tests:
- Tamper with a valid generation, recompute all plain hashes, and assert authenticated verification rejects it.
- Verify intact backups and credential-stripping behavior still succeed.

## Reviewed Surfaces

| Surface | Risk Area | Outcome | Notes |
| --- | --- | --- | --- |
| API routing, auth and vehicle grants | API routing, auth and vehicle grants | No issue found | Authenticated TLS, grant checks and pack scoping reviewed. |
| Provider/Fleet/TeslaMate ingestion | Provider/Fleet/TeslaMate ingestion | No issue found | Representative ingestion/auth bounds reviewed; no independent issue found. |
| Pairing and credential custody | Pairing and credential custody | No issue found | Pairing transaction, secret custody and TLS read protections reviewed. |
| Sync packs and local store | Sync packs and local store | No issue found | Scoped pack lookups and streaming controls reviewed. |
| Backup, restore and AppKit control | Backup, restore and AppKit control | Reported | Two conditional backup-boundary findings; AppKit command/installer guidance mismatch noted without exploit. |

## Open Questions And Follow Up

- Large repository not exhaustively line-reviewed; release packaging, full import/export parsing and live platform behavior require follow-up.
  - Follow-up prompt: Review deferred unit hub-residual-source-review and close its stated proof gap. Paths: src, macos, packaging.
