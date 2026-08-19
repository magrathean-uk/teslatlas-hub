# Dependency Upgrade Ledger — 2026-08-19

## Overview

All Rust dependencies across `Cargo.toml` and `Cargo.lock` in **Teslatlas Hub** have been upgraded to their newest current releases on crates.io under **Rust 1.97.0 / Edition 2024**. All major and minor breaking changes have been refactored and verified.

## Verification & Test Evidence

* **Unit Test Suite**: 584 passed, 0 failed, 2 ignored.
* **CLI & Supervisor Test Suite**: 18 passed, 0 failed.
* **TLS End-to-End Test Suite**: 1 passed (`tests/tls_import_e2e.rs`).
* **Release Build**: `cargo build --release` completed successfully in 54.53s.
* **Total**: 603 passed, 0 failed.

## Upgraded Major & Minor Dependencies

| Crate | Previous | Upgraded | Refactoring Applied / Notes |
| :--- | :--- | :--- | :--- |
| **`aes-gcm`** | `0.10.3` | **`0.11.0`** | Migrated `teslamate_token.rs` & `teslamate_credentials.rs` to `aead 0.6` `Nonce::try_from` and system entropy. |
| **`ed25519-dalek`** | `=2.2.0` | **`=3.0.0`** | Upgraded to Dalek 3.0.0 / `curve25519-dalek 5.0.0` with strict Ed25519 signature verification in parity with App. |
| **`sha2`** | `0.10.9` | **`0.11.0`** | Migrated `hex_sha256` and digest fingerprint serialization to standard `hex::encode(digest)`. |
| **`tokio-tungstenite`** | `0.27.0` | **`0.30.0`** | Upgraded underlying `tungstenite` engine to 0.30 for vehicle telemetry WebSocket streams. |
| **`tower-http`** | `0.6.11` | **`0.7.0`** | Upgraded HTTP trace middleware layers. |
| **`zip`** | `2.4.2` | **`8.6.0`** | Upgraded terrain cache unzip engine. |
| **`base64`** | `0.22.1` | **`0.23.1`** | Updated engine decoding across server auth and manifest signing. |
| **`rusqlite`** | `0.40.1` | **`0.40.2`** | Bundled SQLite C-driver updates. |
| **`rcgen`** | `0.14.8` | **`0.14.9`** | Dynamic TLS certificate generation. |
| **`time`** | `0.3.54` | **`0.3.55`** | Timestamp formatting utilities. |
| **`clap`** | `4.6.4` | **`4.6.6`** | CLI argument parsing. |
| **`futures-util`** | `0.3.33` | **`0.3.34`** | Async task combinators. |
| **`thiserror`** | `2.0.19` | **`2.0.20`** | Error macro generation. |
| **`uuid`** | `1.24.0` | **`1.24.1`** | UUID v4/v5 generator. |
