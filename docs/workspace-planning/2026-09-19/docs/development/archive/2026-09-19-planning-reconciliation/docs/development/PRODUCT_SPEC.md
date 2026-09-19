# Product specification — adoption milestone

Hub is a self-hosted collector, durable store and public API that works independently
of companions. The immediate deliverable is a working Debian ARM64 and Apple-silicon
Hub ecosystem suitable for the separate App owner's v7 adoption work.

| Product | Required role |
| --- | --- |
| Hub | Standalone setup/service, durable SQLite data, pairing/trust, public current/history and recovery |
| TypeScript SDK | Public typed packed Node/browser consumers; caller-owned credentials; ordinary API errors/recovery |
| Edge | Optional mTLS receiver and bounded encrypted spool, durable forwarding and Hub deduplication; no Tesla account credentials |
| Swift SDK | Public SwiftPM consumer on declared supported platforms, strict contract/trust boundary and useful external examples |
| Protocol | Source-neutral versioned wire/profile/schema/conformance authority used by all consumers |
| Home Assistant | Secondary integration in a supported HA runtime using the supported polling profile, config/reauth/lifecycle |

Use actual published/public contract behavior; do not invent App requirements or routes.
Preserve identity, TLS/pairing material, configuration and data across supported restart
and upgrade. Uninstall preserves data by default. Synthetic observations must be labelled;
no synthetic or source-only test establishes real collection parity.

Developer resources (Protocol/SDKs) are packages, not services. Home Assistant runs in
its supported selected runtime, not an invented native Mac daemon. Optional components
must not become mandatory for Hub setup. Complete minimum dependency and launch support
before polishing aggregate installers. Preserve supported OS/runtime floors and exact
profile bindings; product versions, HTTP profile and storage/schema versions are separate.

`app/` and Viewer are outside active implementation. Viewer is a later final product,
never a prerequisite for any gate. x86/Intel/Azure and release automation remain deferred.
No production or vehicle actions. Full parity/installed distribution obligations remain
in MASTER_PLAN as later work, without inflating the App-adoption milestone.
