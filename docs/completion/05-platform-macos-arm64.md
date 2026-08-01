# Apple-silicon macOS completion

## Current position

Earlier macOS arm64 evidence recorded protected install, Keychain credentials,
TLS/pairing, manual and supervised collection, backup/fresh-root restore,
launchd restart, and preserving uninstall/reinstall. This is useful local
native evidence, not fresh release evidence. The recorded artifact was locally
ad-hoc signed, not a distribution-grade signed and notarized package.

## Target host

Apple-silicon macOS only. APFS local durable storage is required. Intel macOS,
network filesystems, FAT/exFAT/removable media, unsafe ownership/symlinks,
active swapping, inadequate free bytes/inodes, and wrong architecture must
fail admission without touching TeslaMate.

## Small completion steps

| ID | Action | Gate |
| --- | --- | --- |
| A01 | Build one provenance-recorded arm64 release candidate. | Artifact digest, source revision, dependency and SQLite provenance recorded. |
| A02 | Fresh install on clean APFS state without credentials. | Hub serves local-only truthful readiness; no unexpected network requests. |
| A03 | Configure protected Keychain credential and TLS/pairing. | Secret scan is clean; pairing and refusal paths pass. |
| A04 | Run service supervision and restart after durable collection. | Launchd restart preserves catalogue and reports readiness within 60 seconds. |
| A05 | Run migration/restore using representative corpus. | Reconciliation, pack verification, backup/restore, and timing record pass. |
| A06 | Run live owner wake proof on this host if eligible. | One-minute durable receipt plus no-wake request audit. |
| A07 | Run supported upgrade and refused unsafe downgrade. | Data intact after upgrade; downgrade refusal preserves state. |
| A08 | Uninstall then reinstall without deleting Hub-owned data. | Exact documented preservation result. |
| A09 | Repeat negative storage/resource/interrupt cases. | Every invalid admission fails closed; crash recovery is truthful. |
| A10 | Decide delivery method. | Developer ID signing/notarization or explicit local-only release boundary. |

## Required evidence bundle

Attach commands, UTC time, hardware/OS, APFS details, resource profile,
artifact digest, corpus/reference identity, request audit, expected/actual
result, and redacted logs. Existing evidence is indexed in
[platform matrix](../PLATFORM_INSTALL_MATRIX.md); it cannot substitute for this
fresh bundle.
