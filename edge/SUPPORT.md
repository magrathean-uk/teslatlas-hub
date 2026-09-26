# Troubleshooting and support

Start with the guide for your installation:

- [Native services](docs/operations/native-installation.md)
- [Docker](docs/operations/docker.md)
- [Upgrade, backup and recovery](docs/operations/upgrade-backup-recovery.md)
- [Hub delivery and authentication](docs/hub-delivery-contract.md)

`doctor` validates configured files without opening listeners. `/healthz`
reports liveness; `/readyz` can fail while the process is alive because delivery
or storage is degraded. Read both in the installation's local context. Never
expose the loopback administration listener to make troubleshooting easier.

For a report through an existing maintainer contact, include the Edge version
and source revision, OS and architecture, native or container installation,
command, expected behavior and a small redacted error sample. Include aggregate
queue/health values only when relevant. Do not attach spools, credentials,
private keys, certificates with identifying details, VINs, coordinates or raw
telemetry. A public issue is not a private reporting channel.

No general support service or service-level commitment is documented in this
checkout. For suspected vulnerabilities, use the organization email route and
subject given in [SECURITY.md](SECURITY.md#reporting-a-vulnerability). That
security route does not establish a general support or conduct-reporting policy.
Do not clear a spool, delete receipts, rerun initialization or roll back across
a storage-format boundary as a reconnect fix.
