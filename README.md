# Teslatlas Hub

Native Rust collector and local data hub for one Tesla vehicle. It can import a
selected car, history, and legacy owner tokens from a TeslaMate PostgreSQL
database, then continue polling and streaming into its own SQLite store.

This is an alpha. It is an independent, unofficial compatibility project, not
affiliated with Tesla or TeslaMate. See [PROGRESS.md](PROGRESS.md),
[PROVENANCE.md](PROVENANCE.md), and [LEGAL.md](LEGAL.md).

## Alpha scope

- macOS 12 or later, Apple silicon only.
- One car.
- Legacy Owner API token authentication only; no Fleet API.
- PostgreSQL history import, encrypted token/key copy, token refresh, Owner API
  polling, Tesla streaming, lifecycle persistence, backup, and repair.
- Native AppKit control app plus full CLI access.
- Per-user LaunchAgent. A login-independent LaunchDaemon is not implemented.
- No Grafana, MQTT, multi-car collection, Debian package, or App Store build.

## Build

Requirements: Rust 1.97, Xcode 27, and XcodeGen.

```sh
cargo build --locked --release
scripts/build-macos-app.sh
```

The app is written to `dist/Teslatlas Hub.app`. Alpha builds are ad-hoc signed,
not notarized.

## CLI setup

Create a private config file:

```toml
data_dir = "/Users/me/Library/Application Support/Teslatlas Hub/data"
bind = "127.0.0.1:8080"
```

Import one car. The PostgreSQL URL must not contain a password.

```sh
teslatlas-hub --config /absolute/path/config.toml migrate \
  --source postgresql://reader@127.0.0.1/teslamate \
  --car-id 1 \
  --postgres-password-file /absolute/path/postgres-password \
  --encryption-key-file /absolute/path/teslamate-encryption-key
```

The first copy runs while TeslaMate may remain active. At the prompt, stop
TeslaMate before answering `y`; Hub then takes one final snapshot and starts.
Do not let TeslaMate and Hub refresh the same legacy token concurrently.

If the original encryption key is unavailable, provide fresh legacy access and
refresh token files instead:

```sh
teslatlas-hub --config /absolute/path/config.toml migrate \
  --source postgresql://reader@127.0.0.1/teslamate \
  --car-id 1 \
  --postgres-password-file /absolute/path/postgres-password \
  --access-token-file /absolute/path/access-token \
  --refresh-token-file /absolute/path/refresh-token
```

Run without installation:

```sh
teslatlas-hub --config /absolute/path/config.toml preflight
teslatlas-hub --config /absolute/path/config.toml serve
```

Or install the current per-user service:

```sh
teslatlas-hub --config /absolute/path/config.toml install
```

Useful checks:

```sh
teslatlas-hub --config /absolute/path/config.toml status
teslatlas-hub --config /absolute/path/config.toml doctor
teslatlas-hub legal
```

## Security and privacy

Tokens are never accepted in command arguments. Imported TeslaMate ciphertext
is copied as ciphertext; the matching key is stored in a mode-0600 file and
tokens are decrypted only in process memory. Protect the Hub data directory,
logs, backups, and secret files. See [SECURITY.md](SECURITY.md) and
[PRIVACY.md](PRIVACY.md).

## Licence

GNU AGPL-3.0-or-later. See [LICENSE](LICENSE), [NOTICE](NOTICE), and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
