---
type: wayfinder:task
status: closed
parent: 000-map
---
# Set the host and resource envelope

Blocked by: [Set reliability objectives](005-set-reliability-objectives.md).

## Question

Within the locked Apple-silicon macOS, Debian amd64, and Debian arm64 platform
set, which memory sizes, CPU classes, filesystems, database sizes, and network
conditions define the supported baseline?

## Starting recommendation

Use macOS as a native first-class host rather than only a development machine.
Define conservative Debian baselines while allowing larger buffers, caches, and
validation work when measured host capacity permits.

## Resolution

The supported baseline host for the representative roughly 10-million-row
migration is an Apple-silicon Mac or Debian amd64/arm64 machine with at least
four CPU cores, 8 GiB RAM, 50 GiB free local SSD storage, and a direct wired
or equivalent LAN PostgreSQL path of at least 1 Gbit/s with normal RTT no more
than 10 ms. The 10-minute target applies only to this baseline or better.

Hub data, staging, temporary packs, and backups require a local APFS, ext4, or
XFS filesystem with durable file sync and atomic rename semantics. Network
filesystems, removable FAT/exFAT volumes, spinning disks, and an actively
swapping host are unsupported for migration or the reliability objectives.
The preflight must reject work unless free space can cover the measured source
projection, unpublished destination stage, verified pack output, and an 8 GiB
recovery reserve; 50 GiB is the minimum admission floor, not a claim that every
future source fits it.

The collector's normal steady-state budget is one Hub process, bounded pages,
and no whole-history retention. Migration may use bounded worker buffers only;
it must not turn the 8 GiB baseline into an in-memory source copy. Larger hosts
may raise concurrency only after ticket 067 measures the same correctness and
reconciliation result. WAN, VPN, low-bandwidth, or latent source paths remain
supported for correctness but have no 10-minute performance claim.

This is a host contract, not current performance proof. Tickets 021, 055, 067,
and 072 must turn it into admission checks, direct binary-COPY limits, corpus
measurements, and published baseline evidence.
