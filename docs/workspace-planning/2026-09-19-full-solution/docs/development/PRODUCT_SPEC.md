# Product specification — full working ecosystem

The required outcome is a fully working Hub and every active companion/dependency,
with complete supported behavior and usable source-built distributions on in-scope
targets. App adoption readiness is an accepted intermediate milestone. All F0–F7 in
[MASTER_PLAN.md](MASTER_PLAN.md) are mandatory; the full goal remains unsent.

Hub is a self-hosted collector, durable store and public API that works independently
of companions. Product specifications, existing supported features and operator guides
control the acceptance ledger. Do not reduce the product to the current-Hub SDK API
surface or silently drop existing behavior to achieve completion.

| Product | Required role |
| --- | --- |
| Hub | Standalone setup/service, durable SQLite data, pairing/trust, public current/history and recovery |
| TypeScript SDK | Public typed packed Node/browser consumers; caller-owned credentials; ordinary API errors/recovery |
| Edge | Optional mTLS receiver and bounded encrypted spool, durable forwarding and Hub deduplication; no Tesla account credentials |
| Swift SDK | Public SwiftPM consumer on declared supported platforms, strict contract/trust boundary and useful external examples |
| Protocol | Source-neutral versioned wire/profile/schema/conformance authority used by all consumers |
| Home Assistant | Required complete integration in its documented supported HA installation/runtime paths using the supported polling profile, config/reauth/lifecycle |

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
No production or vehicle actions. Installed native/container/source distribution, complete active-product lifecycle,
named-source parity/passive data evidence and final combined acceptance are required
for the full goal. Missing external input is a blocker, not a completion exemption.
Source-only publication remains the distribution policy; no binary release is implied.
