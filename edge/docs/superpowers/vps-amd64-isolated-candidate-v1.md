# VPS AMD64 isolated Edge candidate v1

This is a reviewed candidate and run contract only. It performs no installation
by itself. It is for the pending synthetic functional, persistence, and fault
gates on the existing Debian 13 AMD64 VPS, before the final Azure gates. It
does not authorize a production Edge deployment, a vehicle connection, a
firewall change, a release, or an Azure request.

## Observed host boundary

Read-only observation at 2026-09-08T19:48:29Z found `bolykihu` on Debian 13
AMD64 with two CPUs, about 2.1 GiB available memory, 17.4 GB free root-disk
space, and existing Docker projects for TeslaMate, Pi-hole, and Alice Studio.
The VPS has no Edge binary, unit, state root, image, or container. Existing
listener and container inventories are evidence inputs, not targets to modify.

The candidate must never start, stop, restart, reload, inspect secrets from, or
remove any existing service or project. In particular it must not act on
`root-teslamate-*`, `pihole`, `alice-studio-*`, nginx, Cloudflare Tunnel,
mail, databases, WireGuard, or firewall services.

## Admission gate

Hub may propose one run only after attaching all of these to the run request:

1. the exact Edge source/package/image/bridge/contract identity manifest,
   including the current coordinator SHA
   `926a0b876a9d06a28474ef4d43ec04d3f7414f0d16c9e25b0e66ff8a86cc19be`;
2. the reviewed synthetic Hub fixture, its normal and fault actor identities,
   and a private input bundle with no production credential or vehicle data;
3. current read-only host baseline: disk, `MemAvailable`, CPU count, failed
   units, existing container/project inventory, and listener inventory; and
4. a single run suffix of eight lower-case hexadecimal characters, called
   `<id>` below.

The reviewer rejects a request if `<id>` collides with an existing path,
service, container, project, or listener. It also rejects a request if less
than 10 GiB root-disk is free, less than 2 GiB memory is available, the host is
not Debian 13 AMD64, a failed unit exists, or the selected loopback ports are
already listening. These thresholds reserve room for the shared host; they do
not promise that a run will be safe under a changing workload.

The only remaining owner action, once this proposal and immutable inputs are
reviewed, is narrowly to authorize creation, execution, and removal of the
names listed below for one synthetic run. No broader VPS deployment authority
is implied.

## Fixed namespace

For a requested suffix `<id>`, the run owns exactly these names:

| Resource | Exact name or path |
| --- | --- |
| Run ID | `vps-edge-it-<id>` |
| Test Unix account | `teslatlas-edge-it-<id>` |
| Extracted package root | `/opt/teslatlas-edge-it-<id>` |
| Private config and test PKI | `/etc/teslatlas-edge-it-<id>` |
| Native state root | `/var/lib/teslatlas-edge-it-<id>` |
| Native service units | `teslatlas-edge-it-<id>.service`, `teslatlas-fleet-telemetry-it-<id>.service` |
| Docker project | `tlae-it-<id>` |
| Docker compose root | `/opt/docker-stacks/tlae-it-<id>` |
| Docker state root | `/srv/tlae-it-<id>` |
| Expected containers | `tlae-it-<id>-edge-1`, `tlae-it-<id>-receiver-1` |
| Expected Docker network | `tlae-it-<id>_default` |
| Hub test listener | `127.0.0.1:19443` mapped to container 8443 |
| Synthetic receiver listener | `127.0.0.1:19444` mapped to container 8444 |
| Owner-only evidence root | `/var/lib/teslatlas-edge-it-<id>/evidence` |

The native and Docker variants use separate state roots and never run at the
same time. The container port override is mandatory:

```yaml
services:
  edge:
    ports:
      - "127.0.0.1:19443:8443"
      - "127.0.0.1:19444:8444"
    mem_limit: 768m
    cpus: "0.75"
  receiver:
    mem_limit: 384m
    cpus: "0.25"
```

No test port binds `0.0.0.0`, a WireGuard address, or public TCP 443. The
receiver accepts only fixture traffic from loopback. No nftables, Docker
global network, reverse proxy, DNS, VPN, or Cloudflare configuration changes
are permitted.

## Immutable inputs and private material

The owner supplies an AMD64 `.deb` or OCI image that has already been built and
reviewed elsewhere. The VPS must verify its SHA-256, declared product/storage
version, architecture, source identity, contract identities, and bridge
identity before loading or extracting it. Do not compile, pull an unpinned
image, or use a mutable tag on this shared host.

For the native candidate, extract the reviewed `.deb` beneath the test package
root; do not use `dpkg -i` or alter the system package database. Generate
test-only configuration, mTLS material, bearer, and Hub credentials under the
test config/state roots with directory mode 0700 and secret-file mode 0600.
The exact service templates must be copied into the two named test units with
only their user, paths, and loopback ports substituted. Do not reuse the
production `teslatlas-edge` account, `/etc/teslatlas-edge`, or
`/var/lib/teslatlas-edge` names.

For the Docker candidate, the compose root contains only the reviewed compose
inputs plus the mandatory loopback/resource override. `EDGE_RUNTIME_DIR` is
`/srv/tlae-it-<id>` and `--project-name tlae-it-<id>` is mandatory. The state,
receiver-secret, config, Hub TLS, and vehicle TLS mounts remain separate as in
the reviewed Docker design. The fixture's test certificates and bearer have no
production scope and are destroyed during cleanup.

Before first start, record only hashes, modes, owners, device/inode pairs, and
file counts for all inputs. Never retain key, bearer, certificate, config, or
synthetic payload bytes in task output.

## Planned commands and evidence

The following are the exact command categories Hub must bind to a reviewed
argv manifest before the owner action. Values under `<...>` are fixed by that
manifest; no command accepts unreviewed job JSON as an executable, path, port,
or fault selector.

| Step | Bounded operation | Required evidence |
| --- | --- | --- |
| Baseline | Read host capacity, failed units, listener sockets, Docker projects/containers, and named-path absence. | Canonical JSON plus hashes of command output; all candidate names absent. |
| Stage | Verify supplied package/image SHA and metadata; create only named roots with stated ownership/modes; write redacted identity manifest. | Input SHA table, architecture/version output, mode/owner/inode inventory. |
| Bootstrap | For native, extract the package and enable only the two named test units. For Docker, `docker compose --project-name tlae-it-<id> up` only from the named compose root. | Specific process/container IDs, loopback listeners 19443/19444, health/readiness, mount/access proof. |
| Functional | Run the reviewed Hub normal fixture only through 127.0.0.1:19443/19444. | Contract case receipts, Edge durable admission, Hub application/disposition/receipt/frontier, aggregate spool counts. |
| Persistence | Separately restart Edge, receiver, Hub test actor, and whole named test project; then perform isolated image/binary replacement with the same named state root. | Before/after state hashes and counts, shutdown duration, redelivery/no-deletion proof, final frontier. |
| Fault | Run the reviewed fault actor in a fresh named fixture root. | Bound witnesses for transaction, lost-ACK, offline recovery, auth, v1, identity, gap, duplicate, and unsupported-event cases. |
| Upgrade | Use new disposable state roots for receipt-free format-2 migration, receipt-bearing format-2 refusal, format-3 older-binary refusal, and complete backup rollback. | Marker/version declaration, state inventory hashes, expected error/outcome, backup manifest, readiness result. |
| Cleanup | Stop receiver before Edge; stop/disable only the named units or compose project; verify no candidate processes, containers, listeners, mounts, or temporary fixture roots remain. | Final state and listener diff against baseline, preserved encrypted test-backup manifest, named removal inventory. |

Every command result is recorded as argv/stdout/stderr SHA-256 values, exit
status, timestamps, and an owner-only path. Expected functional evidence is
the same 16-case Edge contract used by Hub, including the ARM64-safe coordinator
binding where the current VPS AMD64 cell must report `amd64`. Fault evidence
must prove the expected redelivery or fail-closed outcome; process exit alone
is insufficient.

During execution, halt the run if memory available drops below 1.5 GiB, root
disk free drops below 8 GiB, a non-candidate container/unit changes state, a
new non-candidate listener appears, or cleanup cannot complete. Preserve the
test evidence and leave the run **failed** or **blocked**. Never cure pressure
by pruning Docker globally, deleting a shared volume, or stopping a production
service.

## Rollback and removal

Before any destructive test step, copy the isolated test state to an encrypted,
owner-only backup and verify its manifest. A failed test restores only into a
new named candidate root; it never overlays production or an earlier test run.

The final cleanup sequence is constrained to the chosen suffix:

1. stop the named receiver then the named Edge unit or compose project;
2. verify their PIDs/containers and ports 19443/19444 are absent;
3. preserve the test backup and evidence manifest outside the removal set;
4. remove only the named units, test user, containers/network, compose root,
   package root, config root, and state root; and
5. repeat the baseline inventory and prove every pre-existing service,
   container, project, listener, and non-candidate path is unchanged.

`docker compose down -v`, `docker system prune`, `systemctl reset-failed`,
package-manager removal, firewall changes, and any wildcard path are forbidden.
One `systemctl daemon-reload` is permitted only to load the two named test-unit
files; it must not restart, reload, or alter an unrelated unit.

## Decision boundary

This candidate is suitable only after an owner explicitly authorizes the
single, named synthetic VPS run and Hub supplies its reviewed fixture plus
immutable inputs. Until then, its current result is **planned; not installed**.
The successful candidate would be one AMD64 VPS evidence row. It would not
replace macOS verification, the future native Azure ARM64/AMD64 rows, passive
vehicle provenance, source publication, or release approval.
