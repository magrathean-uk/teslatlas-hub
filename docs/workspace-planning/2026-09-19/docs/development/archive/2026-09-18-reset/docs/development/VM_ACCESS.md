# Development VM access

> Scope update 2026-09-12: Viewer development, planning and acceptance are excluded by owner. Existing Viewer paths and receipts below are preserved historical context, not active work.


> **Active scope, 2026-09-09:** use the existing Debian ARM64 and Apple-silicon Mac environments to deliver working products. All x86/amd64/Intel Mac development, builds, provisioning and runtime/acceptance checks are paused under [ACTIVE_SCOPE.md](ACTIVE_SCOPE.md). The fresh `debian13-x86_64-20260909` VM and its unadmitted HA/Hub artifacts remain preserved; no restart, shutdown, cleanup or further inspection is requested. This supersedes historical statements below about which VM directories exist. Hub owns shared runtime changes, and every host/guest heavy build or VM install requires the shared lock before starting.

> Runtime update (2026-09-08 hourly check): the original clean-OS measurements below are a preparation snapshot. Hub is now installed and active on the primary Debian guest, and Viewer owns a disposable browser fixture. Read the current Hub/product STATUS.json and handoff before changing guest services or assuming the VM is empty.


Verified 2026-09-08 after the owner requested fresh, clearly named environments and consolidation. This file is the shared access reference for the master plan and every product plan. Product development is authorized under [START_AUTHORIZATION.md](START_AUTHORIZATION.md).

## Active environments

| Guest | System and capacity | Login | SSH address | Use |
| --- | --- | --- | --- | --- |
| `debian13-arm64` | Debian 13.6, ARM64 (`aarch64`), VZ; 4 CPUs, 4 GiB RAM, **32 GiB disk**, about 30 GiB free | `bolyki`; home `/home/bolyki.guest`; passwordless `sudo`; SSH key authentication | `127.0.0.1:60022`; alias `teslatlas-debian13-arm64` | Active working Hub/helper and reliability environment; inspect existing Docker/services before changes. |
| `macos13-arm64` | macOS 13.7.4, build `22H420`, ARM64; 4 CPUs, 4 GiB RAM, **50 GB disk**, about 19 GiB guest free | `admin`; home `/Users/admin`; passwordless `sudo`; SSH key authentication; unique GUI password stored privately | NAT address currently `192.168.64.10:22`; alias `teslatlas-macos13-arm64`. The wrapper resolves its current IP each time. | Active Mac working-product target; minimum setup/start/UI/service path now, polished lifecycle later. |

Both guests were created from clean OS bases during this preparation, then renamed from the temporary `d13-a64`/`m13-a64` names. They initially had no Teslatlas installation or product toolchains. That statement is preparation history: current products/toolchains and test data must be read from Hub/product handoffs before changing either guest. VM readiness itself is not installed-product acceptance.

The two approved active-target VM disks are in `~/dev/teslatlas-lab/vms/lima/debian13-arm64` and `~/dev/teslatlas-lab/vms/tart/vms/macos13-arm64`. The macOS OCI base is a reusable source image, not another running VM. Its duplicate-looking `latest` entry was only a symlink to the pinned digest; that redundant alias was removed. Sparse/APFS clone capacities do not represent additional physical allocations of 50 GB each.

## Normal access from the host

Run these from `/Users/bolyki/dev/source/teslatlas-service`:

```sh
scripts/dev/vm.sh status
scripts/dev/vm.sh debian ssh
scripts/dev/vm.sh mac ssh
scripts/dev/vm.sh debian ssh 'uname -m; cat /etc/os-release'
scripts/dev/vm.sh mac ssh 'uname -m; sw_vers'
scripts/dev/vm.sh debian stop
scripts/dev/vm.sh debian start
scripts/dev/vm.sh mac stop
scripts/dev/vm.sh mac start
```

`mac start` stays attached while the guest runs. Keep that terminal/tool session open, then use another session for SSH. Both guests are currently running without a scheduled auto-start. `mac shell` uses Tart's guest agent and `debian shell` uses Lima's managed SSH; these remain available for access repair. Only one heavy build or VM installation should run at a time.

The dedicated lab SSH configuration is `~/dev/teslatlas-lab/access/ssh_config`. For example:

```sh
ssh -F "$HOME/dev/teslatlas-lab/access/ssh_config" teslatlas-debian13-arm64
```

The Mac address in that file is a snapshot. Prefer `scripts/dev/vm.sh mac ssh`, which supplies the address reported by Tart. Host keys are pinned to each VM alias in `access/known_hosts`; do not disable host-key checking. A re-created guest needs a new verified host-key binding and a new environment receipt.

## Login material

- SSH private key: `~/dev/teslatlas-lab/access/id_ed25519` (mode `0600`). Its public key is authorized only in these two lab guests.
- SSH configuration and pinned host keys: `access/ssh_config` and `access/known_hosts` (mode `0600`).
- GUI/login credentials: `~/dev/teslatlas-lab/access/credentials.json` (mode `0600`, inside a `0700` directory). The Mac account's unique password was set and authentication verified. Debian uses its key and has password authentication disabled.
- Read credentials locally when needed; do not copy their values into plans, task messages, Git, logs or acceptance receipts. Product/Tesla credentials are not stored in this file.

For a Mac graphical console, stop its headless run and start the same guest with the lab Tart binary without `--no-graphics`. Log in as `admin` using the private credential file. Do not clone a second Mac VM merely to show its desktop.

## Source transfer, networking and development scope

The Debian VM has no host directory mounts, forwarded SSH agent or automatic application-port forwarding. The Mac VM has no host shares or clipboard integration and uses ordinary NAT. They originated as clean OS environments; Hub now coordinates each product's bounded prerequisites and source transfer against current guest state. Do not assume the host repository already exists inside a guest.

Use SSH/SCP with the lab configuration for source/artifact transfer. A local browser can reach a Debian service through an explicit SSH tunnel, for example `ssh -F "$HOME/dev/teslatlas-lab/access/ssh_config" -L 18443:127.0.0.1:8443 teslatlas-debian13-arm64` when the Hub task has configured that actual guest service. The port is an example until a working Hub handoff supplies its verified endpoint. TLS trust, CORS, pairing and service readiness remain owned bootstrap work.

## Retained and retired environments

The default `~/.lima`, `~/.colima` and `~/.tart` entries are compatibility links into the lab. Plain `limactl list` now shows the fresh Debian guest; the VM wrapper shows only the two active development targets plus the cached Mac source image.

The old 60 GiB `interop-20260905` and 35 GiB `hub-v1-arm64` Colima profiles and their data disks were removed through Colima after confirming only stopped test/build containers, zero host mounts, zero named volumes and no unique user dataset. There is no active Colima VM. Future Docker work uses the primary Debian VM. Colima fallback defaults are 4 CPUs, 4 GiB RAM and 32 GiB disk if a separate future lane is explicitly needed; creating one is not part of the primary plan.

Historical disks with distinct baseline, quarantine or unknown state are offline under `~/dev/teslatlas-lab/vms/retained/`:

- `lima/`: original Debian ARM64/x86_64 baselines and the quarantined ARM64 matrix guest.
- `tart/`: original macOS baseline and matrix guest.
- `manual-debian-arm64/`: the opaque manual Debian disk, firmware and original private launcher files formerly in `~/dev/VM`.

These are preserved historical states, not duplicate fresh development targets. They were moved on the same filesystem with file identity checks, without booting them or rewriting their disks. Their old receipt paths are historical; any reuse requires identity and quarantine reconciliation. The new goals must not start these retained guests automatically. There was no directory at `~/dev/source/teslatlas-lab`.

## Tooling and evidence

Tart 2.36.0 is now at `~/dev/teslatlas-lab/tooling/tart-2.36.0/tart.app/Contents/MacOS/tart`. The previous artifact-tree copy disappeared during this follow-up, so the official release was downloaded again and its SHA-256 verified: `c72a8ab8d78a6498a1e42688b1a1ec6c512ce46ca35a3a3be130c3de1440c7e8`. Source: [official Tart 2.36.0 release](https://github.com/openai/tart/releases/tag/2.36.0).

The Debian image and stable SSH port are specified in [debian13-arm64.yaml](../../scripts/dev/debian13-arm64.yaml). The pinned macOS source remains `ghcr.io/cirruslabs/macos-ventura-base@sha256:9fb387bb987cdda942fbc27118296ed8e16a3359a672c95c6d5ac960165ef1c5`. The wrapper is [vm.sh](../../scripts/dev/vm.sh); storage/output rules are in [ENVIRONMENT.md](ENVIRONMENT.md).

Private inventory, cleanup, file-identity and access-verification receipts are under `~/dev/teslatlas-lab/evidence/`. Only metadata and fingerprints belong in receipts; no private key or password value does.
