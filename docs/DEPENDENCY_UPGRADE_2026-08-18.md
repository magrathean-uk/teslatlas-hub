# Dependency Upgrade Ledger — 2026-08-18

## Overview

All Rust dependencies in `Cargo.lock` have been refreshed and locked to their latest stable, compatible versions under the pinned **Rust 1.97.0 / Edition 2024** toolchain.

## Verification & Test Evidence

* **Unit Test Suite**: 584 passed, 0 failed, 2 ignored.
* **CLI & Supervisor Test Suite**: 18 passed, 0 failed.
* **TLS End-to-End Test Suite**: 1 passed (`tests/tls_import_e2e.rs`).
* **Total**: 603 passed, 0 failed.

## Upgraded Dependencies (Patch & Minor)

The following packages were updated in `Cargo.lock`:

| Crate | Previous | Upgraded | Purpose / Notes |
| :--- | :--- | :--- | :--- |
| `rusqlite` | `0.40.1` | `0.40.2` | Bundled SQLite engine and backup enhancements |
| `thiserror` / `thiserror-impl` | `2.0.19` | `2.0.20` | Error macro backend |
| `futures-util` / `futures-*` | `0.3.33` | `0.3.34` | Async task and stream combinators |
| `clap` / `clap_builder` | `4.6.4` / `4.6.2` | `4.6.6` | CLI option parsing |
| `rcgen` | `0.14.8` | `0.14.9` | Dynamic self-signed TLS cert generation |
| `time` | `0.3.54` | `0.3.55` | RFC 3339 timestamp formatting |
| `uuid` | `1.24.0` | `1.24.1` | UUID generation for pairing tokens |
| `syn` / `proc-macro2` | `2.0.119` / `1.0.106` | `3.0.3` / `1.0.107` | Procedural macro compilation engine |
| `zerocopy` / `zerovec` | `0.8.55` / `0.11.6` | `0.8.56` / `0.11.7` | Zero-copy deserialization |
| `icu_*` family | `2.2.0` | `2.3.0` | Unicode and locale data providers |

## Developer Notes for Future Major Upgrades

When planning future development or major version bumps, be aware of the following protocol and ecosystem couplings:

1. **Cryptographic & Wire Parity with iOS App (`ed25519-dalek` / `zstd` / `sha2`)**:
   - `ed25519-dalek` is pinned to `=2.2.0` across both Hub and App (`teslatlas-core`). Do not bump to `3.0.0` unless both codebases are upgraded simultaneously and wire verification is re-audited.
   - `zstd` is pinned to `=0.13.3` to guarantee deterministic frame-level decompression on iOS devices.
   - `sha2` is on `0.10.x` (`digest 0.10` trait). Bumping to `0.11.x` requires all downstream crypto crates to migrate to `digest 0.11`.

2. **Web Framework & Middleware Alignment**:
   - `axum` is on `0.8.9`. Keep `tower-http` on `0.6.x` and `tower` on `0.5.x` (bumping to `tower-http 0.7` requires unreleased `axum 0.9`).
   - `tokio-tungstenite` is on `0.27.x`. Bumping to `0.30.x` requires refactoring the native-roots TLS connector in `collector.rs`.
   - `aes-gcm` is on `0.10.x`. Bumping to `0.11.x` requires migrating `teslamate_token.rs` from `generic-array` to `hybrid-array`.
