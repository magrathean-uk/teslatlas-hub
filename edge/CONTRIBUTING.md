# Contributing to Teslatlas Edge

Keep changes focused on optional telemetry ingress. Read the relevant
[architecture](docs/architecture.md) and
[delivery contract](docs/hub-delivery-contract.md) before changing behavior.
For a security concern, follow [SECURITY.md](SECURITY.md).

## Build requirements

The Rust manifest declares edition 2024 and a minimum Rust version of 1.98.
The receiver builder requires Go 1.27.0 exactly and verifies the pinned Fleet
Telemetry archive and local patch. It also uses Python 3, curl, patch, tar,
file and a SHA-256 tool. Package recipes consume already-built target binaries;
they do not cross-compile Rust for you.

In the maintained multi-repository workspace, follow the parent execution
wrapper and toolchain policy. Keep build output outside the checkout. In an
independent clone, configure Cargo's external target directory before these
commands. Run from the repository root:

```sh
cargo build --locked --release
cargo run --locked -- --help
cargo fmt --check
cargo test --locked
```

Consider [Clean Development](https://github.com/magrathean-uk/clean-development)
for managing development caches and build output.

## Choose validation for the change

| Change | Focused command or check |
| --- | --- |
| Admission, strict envelopes | `cargo test --locked --test admission_contract --test protocol_contract` |
| Spool, expiry, recovery, ACK identity | `cargo test --locked --test spool_contract --test delivery_contract --test fault_matrix` |
| Credentials and TLS | `cargo test --locked --test credential_contract --test mtls_contract` |
| Configuration and listener lifecycle | `cargo test --locked --test config_contract --test runtime_contract` |
| Package recipes and service files | `cargo test --locked --test packaging_contract` plus a disposable installed lifecycle check |
| Receiver overlay or pin | `scripts/test-fleet-telemetry-bridge.sh` |
| Installed matrix adapter | Read [the interop guide](tools/interop/client_lanes/README.md) and run the affected Python tests |

The receiver test downloads and builds pinned upstream source and checks the
result. It needs network access and the exact Go toolchain. A Cargo test pass
cannot replace this bridge check. Installed matrix execution needs the shared
runner and private session inputs; never point it at a live installation as a
quick test.

Use an isolated spool and synthetic identities. Check rejection paths as well
as success, including replay, storage exhaustion and restart when relevant.
Do not weaken strict decoding, certificate checks, quotas or receipt ordering
to make a fixture pass.

## Prepare a review

Describe the problem, the resulting behavior and the checks actually run.
Record the exact revision, target, toolchain and artifact for runtime claims.
Keep source checks, package creation, installed lifecycle and real-data
acceptance separate. Include remaining gaps without presenting old receipts
as current proof.

Preserve existing changes and legal notices. Third-party updates need exact
source/version and notice review; see [licensing](docs/licensing.md). Do not
add contributor assignment, commercial terms or a new license through a docs
change.

GitHub is used for source storage. Do not add CI, hosted tests, security
automation, release publishing or build badges. Use the owner's existing
review channel; this guide does not establish a support service or public
contribution intake process.
