# Security and privacy model v1

## Assets and trust boundaries

Protected assets are Tesla owner/Fleet credentials, read-only PostgreSQL
passwords, cursor/signing material, TLS private keys, pairing/device bearers,
raw vehicle telemetry and location history, Hub SQLite/stages/packs/backups,
and redacted audit evidence. TeslaMate, its PostgreSQL database, services,
containers, configuration, schedules, and credentials are external read-only
sources. The Hub service account, systemd credential delivery, protected local
filesystem, direct TLS listener, paired phone, and operator are distinct trust
boundaries; no boundary is assumed trustworthy merely because it is local.

| Threat | Required control |
| --- | --- |
| Secret disclosure in configuration, URL, logs, reports, or errors | credentials only via private systemd credential files; reject embedded/query secrets; typed redacted debug/error paths; report/fixture secret scan before sealing |
| Unprivileged local process or path attack | dedicated service user, restrictive state/credential/TLS permissions, symlink-safe path checks, systemd sandbox/no capabilities, bounded file and request parsing |
| Network attacker or unpaired client | TLS-only remote listener, paired bearer authentication, short-lived single-use invitation, leaf pin in pairing, no redirects for owner API, bounded bodies/timeouts, no arbitrary SQL/history route |
| Malicious source payload or migration endpoint | explicit HTTPS/TLS or PostgreSQL TLS, fixed allowlisted reads, no wake/command route, bounded typed decode, schema/read-only/repeatable-read checks, staging/quarantine and integrity gates |
| Lost/replayed phone credential | store token digests, one-use invitation claim, device-scoped revocation, stale cursor/signature rejection, and no access to raw telemetry or Tesla credentials |
| Package/supply-chain or plugin extension | signed/reproducible package evidence is required before release; Hub has no plugin host, script execution, browser, MQTT, or web-UI extension surface; an added extension needs a separate threat model |
| Backup, report, or corpus disclosure | redacted report references only, protected artifact paths/digests, synthetic/default fixture data, reviewed sanitizer provenance, and operator-controlled backup storage/access |
| Physical disk or privileged-host attacker | document exposure and require operator filesystem/full-disk protection; Hub does not claim application-level encryption for telemetry, packs, or backups, and host root can inspect a running service |

## Credential lifecycle and privacy

Legacy, Fleet, PostgreSQL, cursor/signing, TLS, and paired-device material have
separate custody. Handoff is optional and inactive; it never rotates, revokes,
or refreshes TeslaMate material. Hub revocation removes only Hub credential or
device authority and records a non-secret audit result. Credential/TLS/cursor
replacement requires a verified Hub-only candidate, explicit operator action,
atomic activation, and compatibility/rollback evidence; it never silently
invalidates a phone or modifies TeslaMate.

Hub collects no analytics and exposes to a paired phone only the typed selected
vehicle mirror. Raw owner responses, credentials, operation journals, source
database handles, and arbitrary historical SQL remain local. Location and
telemetry are sensitive even without a name, so support/report exports default
to redacted identifiers, hashes, counts, and classifications. Retention follows
the integrity/backup policy; automatic deletion is not a privacy substitute.

## Required adversarial proof

Tests must cover credential-bearing URL/config/response/log rejection,
permission and symlink attacks, expired/replayed pairing, invalid bearer and
TLS/redirect failures, malformed/oversized source input, no-wake route audit,
read-only migration attempts, corrupt/tampered pack/manifest/backup/report,
private crash recovery, corpus sanitizer checks, and negative absence of plugin,
MQTT, and web control surfaces. A passing test does not claim protection from a
compromised host root, physical disk theft without host encryption, or a
malicious authorized operator; those are explicit non-goals requiring operator
controls.
