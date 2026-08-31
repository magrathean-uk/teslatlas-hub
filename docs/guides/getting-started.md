# Getting started

Teslatlas Hub runs on one operator-controlled host, keeps provider credentials
resident there, stores telemetry locally, and synchronises history to paired
Teslatlas clients.

## 1. Choose the host

| Host | Supported beta deployment |
|---|---|
| Apple-silicon Mac, macOS 13+ | Native **Teslatlas Hub.app** plus per-user LaunchAgent |
| Debian 13, `amd64` or `arm64` | Native package plus hardened systemd units |

Follow [Install on macOS](install-macos.md) or
[Install on Debian](install-debian.md). Other systems are outside the beta
support scope.

## 2. Choose one credential path

| Legacy | Fleet |
|---|---|
| Uses an existing Owner API access/refresh-token pair. | Uses a Tesla developer application and Fleet credentials. |
| Collects by polling and the legacy driving stream. | Collects through Fleet API and selected Fleet Telemetry push. |
| Only one service may own refresh of the token pair. | Hub remains the resident credential owner and supervises companion services. |
| Suits an existing legacy or TeslaMate deployment. | Suits a new provider-supported setup. |

Do not configure both paths casually. For Fleet, follow the complete
[Fleet setup](fleet-setup.md). For legacy setup, use the guided macOS flow or
the packaged Debian command shown in the installation guide.

## 3. Decide the network boundary

The safe default is:

```toml
data_dir = "/absolute/private/path/teslatlas-hub"
bind = "127.0.0.1:8080"

[geocoder]
enabled = false

[terrain]
enabled = false
```

Keep plaintext HTTP on loopback. A non-loopback listener requires Hub TLS and
paired-device bearer authentication. Never publish the internal Fleet
Telemetry ingestion route. See [Configuration](configuration.md).

## 4. Initialise and verify

Use the exact platform invocation from the [CLI reference](cli.md). On a source
checkout, the sequence is:

```sh
cargo build --locked --release --bin teslatlas-hub
./target/release/teslatlas-hub --config /absolute/path/config.toml init
./target/release/teslatlas-hub --config /absolute/path/config.toml doctor
./target/release/teslatlas-hub --config /absolute/path/config.toml status
```

After credentials are configured, run `doctor` again, start the service, and
confirm that every intended vehicle is admitted and collecting. Redact all
diagnostics before sharing them.

## 5. Pair a client

Create a short-lived, single-use invitation with `pair`, then complete pairing
from the Teslatlas client. Revoke unknown or retired devices immediately.
Pairing does not make an unsafe network listener safe; TLS and host-level
controls still apply.

## 6. Establish recovery

Before relying on Hub:

1. create and verify a data-only backup;
2. export credentials into a separately encrypted recovery file;
3. store the two materials separately; and
4. test restoration into a new directory.

Follow [Backup and recovery](../operations/backup-and-recovery.md). A data
backup alone cannot restore provider credentials.

## Migrating from TeslaMate

Migration is optional and read-only against the source database. Back up
TeslaMate, update it to 4.2.0 or newer, start it once, and wait for its database
migrations to finish. Run the compatibility check and explicitly acknowledge
that database evidence cannot prove the running app version. At final cutover,
stop TeslaMate before Hub receives ownership of the same legacy token pair. See
[TeslaMate migration](../releases/migration.md).
