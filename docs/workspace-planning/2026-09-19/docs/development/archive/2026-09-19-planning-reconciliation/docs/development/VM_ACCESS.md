# VM access — Debian ARM64 active

Updated 2026-09-18. Hub owns this shared environment. The owner-deleted guests
were not reused; `debian13-arm64` was recreated from the pinned recipe and all
historical guest addresses, host keys and credentials remain obsolete.

## Current guest

| Guest | Verified platform | Resources | State and owner |
| --- | --- | --- | --- |
| `debian13-arm64` | Debian 13.6 (`arm64`/`aarch64`), kernel `6.12.95+deb13-cloud-arm64`, Lima 2.2.0 VZ | 4 CPU, 4 GiB RAM, 32 GiB sparse disk; about 31.2 GB initially free | Stopped after cold-restart acceptance; Hub owns start/stop and cleanup |

The image is Debian's `20260712-2537` generic-cloud ARM64 image. Lima verified
the recipe's pinned SHA-512
`8543d795f2fde630eb66c492f245a8c1da19dedc636e0a8e7b3d0f95920e1a05aa911ef2d82d177d41cc53ced5fccbd2a3945d07fa5e15018914c4d864bb07ed`.
The accepted instance occupied about 1.22 GB of host blocks; the retained Lima
image cache occupied about 1.38 GB. This is allocation evidence, not the sparse
disk's 32 GiB capacity.

No Mac guest exists. `/Users/bolyki/dev/teslatlas-lab/vms/tart` contains only
Tart's empty state directories so the shared status wrapper can report an empty
inventory. Creating a Mac guest remains gated by the next macOS acceptance need.

## Access and lifecycle

Use the checked-in wrappers from the workspace root:

```sh
scripts/dev/vm.sh debian start
scripts/dev/vm.sh debian ssh uname -a
scripts/dev/vm.sh debian stop
scripts/dev/vm.sh status
```

The alias is `teslatlas-debian13-arm64` at `127.0.0.1:60022`, user `bolyki`.
The current owner-only access files are:

- config: `/Users/bolyki/dev/teslatlas-lab/access/ssh_config`;
- private key: `/Users/bolyki/dev/teslatlas-lab/access/debian13-arm64-g0-20260918T153546Z`;
- public key: the same path with `.pub`;
- strict known-hosts file: the same path with `.known_hosts`.

Password authentication and agent forwarding are disabled; `IdentitiesOnly` and
strict host-key checking are enabled. The accepted ED25519 host fingerprint is
`SHA256:Q7mBR/PbEZKdZmDwEmqPtDG9p7qxhRQ2GCUA4qqsdY0`. The ECDSA and RSA fingerprints
are retained in the compact G0 evidence. Never replace this with disabled host-key
checking or inherit a historical fingerprint.

## G0 acceptance

The guest passed two stopped-state checks with no listener on port 60022 and a
full stop/start persistence cycle with an unchanged sentinel, strict dedicated-key
login and byte-identical ED25519, ECDSA and RSA host keys. The first cold restart
exposed Lima NoCloud instance-ID churn: Debian cloud-init regenerated host keys.
The accepted guest now has
`/etc/cloud/cloud.cfg.d/99-teslatlas-preserve-ssh-host-keys.cfg` with
`ssh_deletekeys: false`; the checked-in recipe carries the same fix for rebuilds.

Core versions captured before product installation include bash 5.2.37, curl
8.14.1, systemd 257.13, OpenSSL 3.5.6 and OpenSSH 10.0p1. No compiler, SQLite CLI
or Teslatlas product was installed for G0. The authoritative compact receipt is
`/Users/bolyki/dev/teslatlas-lab/vms/lima/debian13-arm64/g0-evidence/summary-20260918T153546Z.txt`.

Keep access material owner-only under `/Users/bolyki/dev/teslatlas-lab/access/`.
Hub must issue a fresh bounded handoff naming exact source/artifact, endpoint,
private inputs, expiry, actions and teardown owner for every runtime session.
Historical r7/r11 and other closed cohorts remain evidence only. This VM does not
authorize production credentials, public ingress, Tesla account/vehicle actions,
or reuse of any prior runtime identity.
