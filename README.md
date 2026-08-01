# Teslatlas Hub

Native Tesla telemetry backend for Teslatlas. It is being built to replace the
Docker-shaped TeslaMate deployment path with a small Rust service, SQLite
storage, systemd, and a binary sync path made for iPhone transfer.

> Development status: the bridge foundation exists, but full TeslaMate backend
> parity is not complete. Follow the ordered [Wayfinder map](roadmap/000-map.md).

This is a ground-up, AGPL-compatible fork in product direction from the
TeslaMate ecosystem. It credits TeslaMate for the source-migration contract
and behaviour it can read, while keeping this implementation and its transport
code independent; no TeslaMate application code is copied here.

The Hub phone format is a typed, immutable SQLite pack: `cars`, `drives`,
`positions`, `charges`, and `charge_samples`, plus binding metadata for one
installation, account, vehicle, generation, and selected local car ID. Raw
collector observations never leave the Hub. The first typed format emits full
snapshots only. The iOS bridge verifies a signed manifest, streams immutable
packs into a private cache, stages every pack in a fresh sibling SQLite file,
and atomically activates only the complete receipt-bound snapshot. The isolated
Teslatlas integration presents Hub as its third source: a person pastes the one-use pairing URI,
chooses one published Hub vehicle, and stores the paired profile only in the
existing device Keychain after a complete signed import. Foreground refresh is
wired; overnight refresh is still blocked and remains roadmap work. Deltas also
remain future work.

TeslaMate migration is explicit and read-only. It uses a TLS PostgreSQL
connection, a reviewed schema probe, and one repeatable-read transaction. A
full snapshot is published only after the typed pack passes local integrity
and cursor-binding checks. Each fixed query is scoped to the selected car before
keyset pagination. Large position and charge tables stream directly into
bounded, independently verified, parent-complete typed fragments; no whole
history vector or JSON staging database is created. Only a fully verified
fragment set receives a cursor-bound manifest. iPhone authenticity starts with
paired TLS and the certificate fingerprint carried in the one-use pairing URI.
TLS Hub responses also expose a paired manifest public key and sign the exact
manifest response bytes; client verification is required before a manifest can
enter a staging import.

## First local run

```sh
cargo run -- --config ./config.toml init
cargo run -- --config ./config.toml doctor
cargo run -- --config ./config.toml serve
```

Example `config.toml`:

```toml
data_dir = ".teslatlas-hub"
bind = "127.0.0.1:8088"
```

For a TeslaMate import, add a credential-free source endpoint and a durable
source label. Do not use the hostname as the label, and never put a password
in the URL:

```toml
[teslamate]
source_url = "postgresql://teslamate_reader@db.internal/teslamate"
source_key = "home-teslamate"
```

Health: `GET /healthz`.
Readiness: `GET /readyz`.
Capabilities: `GET /.well-known/teslatlas-hub`.

## Native Linux setup

After installing the Debian package, run one owner command on the Hub machine:

```sh
sudo teslatlas-hub-setup
```

It creates or reuses the protected local TLS identity, starts the service on
the detected LAN address, verifies readiness over that exact certificate, and
displays a short-lived pairing QR. Use `--lan-address ADDRESS` only when the
machine has several network routes and automatic selection is not the desired
one.

Credentials do not belong in `config.toml`, command arguments, environment
variables, logs, or this repository.

The installer stores its binary cursor-signing key as a host-encrypted systemd
credential on every native installation, and can store an owner token the same
way. Neither belongs in configuration, argv, environment, logs, or the Hub
database. This protects normal process inspection and accidental configuration
leaks. It does not protect an unencrypted host disk from a root or offline-disk
attacker; use encrypted storage or a host key backed by your infrastructure
when that threat model applies.

## Native baseline

Hub ships its SQLite engine. It does not use the Debian package version.
SQLite `3.53.4` is vendored from the official amalgamation, hash-checked during
release engineering, and compiled statically into the Hub binary. Normal
builds and installs never download it.

The technical boundary and data-flow design is in
[Architecture](docs/ARCHITECTURE.md). Remaining decisions and proof gates are
tracked in the ordered [Wayfinder map](roadmap/000-map.md).
