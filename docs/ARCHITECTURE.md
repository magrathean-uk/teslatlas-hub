# Architecture

This is the target architecture. Implemented and proven boundaries are tracked
separately in [Current status](STATUS.md).

Teslatlas Hub is a native service, not a container bundle.

| Layer | Choice | Boundary |
| --- | --- | --- |
| Host | Debian 12+ amd64 or arm64, systemd | No Docker daemon or database service |
| Hub | Rust, bundled SQLite, systemd credentials | Host owns tokens and pack catalog |
| Transport | Rustls TLS, HTTP/2, zstd SQLite packs | iPhone gets typed mirror data only |
| Phone | Swift networking, Rust-owned SQLite mirror | One selected Hub vehicle per source profile |

## Data lanes

`owner-token` is the compatibility lane. It performs explicit GET-only vehicle
discovery, never calls wake, stores bounded raw observations, and publishes a
signed typed car snapshot. This establishes an immediate iPhone source without
pretending that a single current-state response is completed trip history.

TeslaMate PostgreSQL is the history lane. It performs a TLS, read-only,
repeatable-read capture for one selected car, then emits parent-complete typed
history fragments.

Fleet is the future ongoing-data lane. It needs an owner-registered Tesla
application, its own credentials, callback and telemetry setup. It is not a
fallback that silently changes token behaviour.

## iPhone transfer

1. The owner creates a short-lived pairing invitation after configuring TLS.
2. Teslatlas pins the leaf certificate, claims the invitation once, and keeps
   only the paired bearer in its Keychain-backed source profile.
3. The Hub signs the exact manifest bytes with an Ed25519 key derived from the
   protected Hub cursor key.
4. The phone verifies the raw manifest signature, downloads content-addressed
   zstd SQLite packs over same-origin HTTP/2, and resumes an interrupted tail
   only when `ETag`, `Content-Range`, size, and final SHA-256 all agree.
5. Rust stages every pack into a fresh private SQLite file. The live local
   mirror swaps only after the full signed receipt set seals.

The phone never receives Hub credentials, raw owner responses, PostgreSQL
credentials, or a remote SQLite database handle.

## Host deployment shape

The package starts loopback-only. Remote phone use is an opt-in direct TLS
listener with a public HTTPS origin. The TLS certificate private key remains
host-local; the pairing URI contains only endpoint, one-use pairing secret,
pairing ID and leaf-certificate fingerprint.

The release path is detached-manifest signed with an independently pinned
Minisign public key. Development bootstrap pins a reviewed Git object. Both
paths build or install native Debian packages and use systemd encrypted
credentials; neither puts a Tesla token in configuration, argv, environment,
or the Hub database.

## Intentional limits

- No implicit production API endpoint or background legacy polling.
- No vehicle wake, command, or charging-control capability in the token path.
- No partial phone mirror activation.
- No data deletion during package removal or upgrade.
- No performance claim without a measured Pi/VPS-class benchmark.
