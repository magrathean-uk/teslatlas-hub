# Upgrade, downgrade, and rollback v1

Every Hub upgrade begins with a Hub-only plan that records old/new binary,
catalogue, protocol, pack, configuration, credential, and adapter versions.
Before package activation it validates artifact identity, configuration syntax
and ownership, filesystem capacity, current readiness, active operation state,
and a freshly verified Hub backup generation. It refuses an unknown version,
unsafe path, incompatible schema/pack/protocol, missing backup, unresolved
quarantine, or insufficient recovery space. It never changes TeslaMate.

Hub first quiesces only its own work at a durable transaction boundary. A
collected fact either committed before quiescence or remains unacknowledged;
an open migration capture is discarded and a sealed stage remains evidence.
The upgrade applies only ordered transactional Hub migrations. It validates the
catalogue, manifests, packs, operation journal, credential availability, and
truthful readiness before re-enabling the Hub serving process. Collection,
migration, pairing, and cutover do not become enabled merely because a binary
was upgraded.

Configuration is staged as a new private, non-secret candidate and becomes
active only after exact parsing and compatibility validation. A migration
source, credential mode, listener identity, or collection authority change is
a separate explicit operation, not an upgrade side effect. Existing encrypted
Hub credentials, cursor/signing material, TLS identity, device records, and
TeslaMate custody remain unchanged unless the operator separately performs a
verified Hub-only rotation.

Catalogue migrations are forward-only. A binary-only downgrade is permitted
only when the old binary explicitly supports every retained catalogue,
protocol, pack, and configuration version. Otherwise downgrade is refused.
Rollback after any incompatible migration restores the verified pre-upgrade Hub
backup into a fresh private location with its matching binary, verifies hashes,
integrity, manifest/pack signatures and `doctor`, then atomically selects that
complete generation. It never attempts a lossy reverse migration, overwrites a
known-good backup, or asks TeslaMate to repair a Hub change.

Compatible protocol/pack extensions retain the previous reader contract for
the declared transition. An incompatible protocol, pack schema, source identity,
or projection revision creates a new full-snapshot generation; old referenced
packs remain until the backup/retention floor permits removal. An upgrade or
rollback report seals plan/backup/config digests, version matrix, quiescence
boundary, migrations, validation results, final readiness, and any refusal
reason without secrets.
