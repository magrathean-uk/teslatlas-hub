# Native packaging and supervision v1

Debian amd64 and arm64 ship one native `teslatlas-hub` package with the static
Hub binary, default non-secret configuration, systemd unit files, setup and
verification tools, and documentation. It contains no credential, token, TLS
identity, telemetry, migration data, or generated pack. The package creates a
non-login `teslatlas` service account and only Hub-owned directories: protected
configuration/TLS/credential roots, `/var/lib/teslatlas` state, and
`/run/teslatlas` runtime state. Ownership and modes deny other local users;
setup rejects unsafe identity paths and never uses TeslaMate paths.

## Units and activation

`teslatlas-hub.service` is the serving unit. It starts after network-online,
runs as `teslatlas`, restarts only on failure, and receives only its cursor key
and optional owner credential through systemd encrypted credential drop-ins.
`teslatlas-hub-collect.service` is a manual oneshot, never a timer or
dependency-triggered collector. `teslatlas-hub-import@.service` is a manual
oneshot with a selected car ID and optional read-only PostgreSQL credential
drop-in. No package unit has a TeslaMate unit, container, compose file, port,
credential path, database, or schedule as a dependency or mutation target.

Package installation never enables, starts, collects, imports, pairs, exposes a
remote listener, or changes TeslaMate. Explicit Hub setup validates its own
storage, identity, listener, and readiness before enabling/starting only the
Hub serving unit. Collection, import, credential handoff, and source authority
remain separate explicit actions. Default configuration remains loopback-only;
remote use needs the direct TLS/pairing path.

## Service hardening and limits

All Hub units run with a private temporary/runtime/state directory, no ambient
or bounding capabilities, no new privileges, protected system/home/kernel
surfaces, restricted address families, native syscall architecture, restricted
namespaces/realtime/SUID, and bounded tasks/files. They retain ordinary IP/Unix
sockets only because Hub must serve paired TLS and perform explicitly authorized
outbound collection/import. Credential material is never passed by environment,
argv, configuration, or plaintext staging file.

Upgrades preserve state, credentials, TLS identity, packs, reports, and the
previous verified backup until the separate compatibility/rollback gates pass.
Package removal and purge stop only package-managed Hub behavior and preserve
all user telemetry and credentials; data destruction is a separately authorized
Hub-only operation. A package may fail closed on unsafe files, incompatible
architecture, missing credentials, or unhealthy Hub state rather than repairing
or changing an external source.

Apple-silicon macOS is a supported native runtime but has no systemd contract.
It must use a separately verified native delivery/supervision path with the same
Hub-only ownership, protected credential handling, manual collection/import,
no timer, and data-preserving removal guarantees. Intel macOS is not a first
platform.

## Apple-silicon macOS path

`scripts/mac-install.sh` builds the native arm64 binary, applies a local code
signature, installs protected user-owned state under Application Support, stores
cursor and optional owner credentials in the login Keychain through a
Security.framework helper, generates a direct TLS identity, and installs one
user LaunchAgent. The LaunchAgent starts only the serving process. Collection,
pairing, import, and backup remain explicit wrapper commands; no polling timer
is installed.

The wrapper materializes credentials only in a mode-0700 temporary runtime
directory for the child lifetime and removes that directory on exit. Secrets
never enter configuration, argv, or environment values. `mac-verify.sh` checks
launchd ownership, code signature, catalogue integrity, and TLS readiness.
`mac-uninstall.sh` removes only the LaunchAgent and executables while preserving
state, configuration, backups, logs, and Keychain credentials.

The 2026-07-31 local proof passed install, TLS readiness, one-time pairing,
fixture publication, online backup, fresh-root restore, one-second launchd
restart, removal/reinstall with unchanged catalogue hash, and live owner
collection with one snapshot persisted and zero failures. Developer ID signing,
notarization, and a distributable release package remain release gates.

Supervised collection is a separate opt-in service on both native platforms.
Debian installs `teslatlas-hub-supervised.service` disabled; macOS installs the
`teslatlas-hub-supervised enable|disable` owner action. Both refuse an interval
below 15 seconds and require the protected owner credential. Neither uses a
timer or wake/command route. The 2026-07-31 proof completed three consecutive
15-second live collections on each platform, persisted a new observation each
cycle, and left supervision disabled afterward.
