# Operator runbooks v1

All commands below own Hub only. Never put a secret in a command argument,
environment, configuration, log, or support bundle. Every completed procedure
keeps its redacted command result and report/manifest digest. If a named gate
fails, stop at that gate; do not repair, restart, reconfigure, or cut over
TeslaMate.

| Procedure | Prerequisites and normal action | Evidence and rollback |
| --- | --- | --- |
| Install | verified signed native artifact and pinned public key; Debian uses `install.sh`, then explicit `teslatlas-hub-setup` | package/manifest digest, TLS/readiness result; remove only Hub package, preserving state |
| Health | Hub service installed; run `teslatlas-hub-verify` or `teslatlas-hub doctor` | readiness/integrity result; no rollback needed because this is read-only |
| Credential | protected regular input or password agent; use the installer credential path only | non-secret custody/permission result; revoke/remove only the Hub candidate/drop-in, never TeslaMate copy |
| Collection | explicit Hub authority, valid owner credential, explicit HTTPS base, and no active gate; run manual `teslatlas-hub-collect.service` once | request/no-wake audit and durable receipt; failed run leaves source and prior Hub facts unchanged |
| Migration | signed preflight, read-only PostgreSQL identity, selected car, capacity, backup/restore gate; run manual import unit once | capture/stage/reconciliation/manifest report; failed capture discards only incomplete Hub stage |
| Repair | Hub not-ready or local quarantine diagnosis; run `teslatlas-hub repair` only after recording `doctor` | repair report and retained quarantine; corrupted catalogues/referenced packs require offline restore, not repair guesses |
| Backup/restore | only the released, verified Hub backup/restore helper and a fresh private target | complete backup manifest/hash and clean-host restore proof; never copy live SQLite/WAL files or TeslaMate credentials manually |
| Upgrade/downgrade | verified backup, compatibility plan, and Hub quiescence | upgrade report/readiness; unsafe downgrade refuses, compatible failure restores matching Hub backup/binary |
| Cutover/rollback | all verification-window, reconciliation, live-probe, and backup gates pass; run `sudo teslatlas-hub-cutover --car-id ID` | the redacted Hub-only gate report proves import and post-wake collection; Hub never executes TeslaMate actions; rollback seals Hub-only interval before authority returns |
| Incident | record current `doctor`, readiness, unit state, report IDs, and non-secret logs; if containment is needed, stop only the Hub unit | preserve evidence and prior data; resume only after named recovery/restore gate passes |

Normal operations are single explicit Hub commands, never timers. Migration,
collection, and cutover do not follow package installation automatically. The
only physical action is the owner-authorized wake for the separate live probe;
the script asks, records acknowledgement, waits one minute, and never sends a
wake command.

Destructive Hub data removal, manual database edits, live WAL copying, secret
recovery by log inspection, source-side service control, and unreviewed
configuration edits are prohibited runbooks. They need a separate explicit
operator authority and a destructive-scope confirmation. For macOS, use the
same release-verified native helper semantics once its native supervision path
is released; do not substitute systemd commands.
