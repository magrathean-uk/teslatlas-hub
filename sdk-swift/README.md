# Teslatlas Swift SDK

Public Swift client SDK for the Teslatlas Hub protocol.

## Status

Protocol-gated package foundation. The package builds, tests contract-neutral internal mechanics, and exports no speculative public Hub API.

Released v1 schemas, fixtures, and conformance rules do not yet exist in `teslatlas-protocol`. Discovery, capabilities, typed server errors, pairing, bearer rotation, queries, event payloads, signed manifests, and pack transport remain blocked. See the [exact dependency gate](docs/protocol-dependency-gate.md).

## Purpose

The proprietary Teslatlas app and independent Apple-platform clients will use this SDK for public Hub pairing, discovery, query, event, and sync contracts. Product-specific presentation and local analytics stay outside this repository.

## Read next

- [Architecture](docs/architecture.md)
- [Foundation plan](docs/plans/2026-08-30-foundation.md)
- [Protocol dependency gate](docs/protocol-dependency-gate.md)

## Package foundation

- Swift tools 6.0
- iOS 17+
- macOS 14+
- no third-party dependencies
- internal incremental SSE framing with bounded line and event data
- internal atomic Codable state persistence
- internal HTTP byte-range resume validation

```sh
swift test
swift run TeslatlasHubSDKExample
```

The example proves package import and execution only. It does not connect to a Hub while the protocol gate is active.

## Licence

Apache-2.0.
