# Teslatlas Hub ecosystem product specification

> OWNER SCOPE — 2026-09-12: Viewer is excluded from all active development, goals, plans, packaging and acceptance dependencies. Preserve its existing source and historical evidence; do not schedule or resume Viewer work. Six products remain: Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge.


Owner direction: 2026-09-09. Active scope is working Debian ARM64 and Apple-silicon Mac products under [ACTIVE_SCOPE.md](ACTIVE_SCOPE.md). All x86 development and acceptance work is paused. The original start authorization and models remain in force.

## Product boundary

Hub is the primary product: a Rust replacement for TeslaMate that collects and durably retains vehicle data. Hub must install and operate without any Teslatlas companion. The five companions add optional capabilities. The iOS App in `app/` is outside this programme; preserve its code, data, independent plans, signing and pinned development tools.

| Product | Role | Installation treatment |
| --- | --- | --- |
| Hub | Collector, durable SQLite storage, API, administration and ecosystem installer owner | Core service; native macOS administration app; bootstrap, Debian package, macOS package, Docker |
| Protocol | Canonical current-Hub HTTP and Edge contracts and conformance assets | Versioned contract/data package and developer tools; no idle daemon |
| TypeScript SDK | Public typed Node and browser client | Library consumed by supported applications; optional developer bundle |
| Swift SDK | Public Swift client for supported Apple and Linux consumers | Swift package; optional developer bundle; no idle daemon |
| Home Assistant | Integration loaded by Home Assistant | Install integration into an explicitly selected HA instance; optional HA Container profile where its runtime is supported |
| Edge | Remote telemetry reception and durable encrypted forwarding | Optional service with explicit receiver/bridge credentials and configuration; native packages and Docker |

Home Assistant itself is a separate runtime. Its macOS installer option must explain whether it is installing an integration for an existing instance or configuring an explicitly selected container/VM runtime. Do not advertise an unsupported native Home Assistant daemon. Protocol and SDK install choices must be labelled as developer resources rather than services.

## Required outcomes

- **First milestone: a simple working bootstrap.** Use the primary Debian 13 ARM64 development VM to get Hub and selected helpers working together through normal public interfaces. Deliver Hub alone first, then complete the primary-environment companion paths. Reuse the existing bootstrap and correct only the contract/runtime gaps needed for this working slice. Do not put polished packages or the full platform matrix in front of it.
- First deliver working ARM64 and Mac paths. Complete minimum installation/dependency/service work needed to run them while collecting applicable recovery evidence. Missing named-source parity inputs remain an open gap, not a blocker for independent usability. Polished `.deb`, selectable macOS `.pkg`, full Docker composition and final ARM64/Mac lifecycle follow ordinary use; all x86/full cross-architecture work is deferred.
- Hub-only installation, startup, onboarding, collection, recovery, upgrade and removal work independently of all optional components.
- An installer offers compatible components together and installs all runtime dependencies required by the selected components. It does not silently select telemetry credentials, public ingress or another collector/token-refresh owner.
- Bootstrap supports interactive choices and deterministic unattended selection. Debian offers a core package, optional component packages and a bundle selection. macOS provides visible, deselectable package choices and appropriate launchd service ownership. Docker provides a core composition and explicit optional profiles, with persistent data, readiness and restart behaviour.
- Every installed component uses the actual current Hub contract; no mock, fixture, compiler result or static Compose validation is substituted for an installed runtime test.
- Data collection parity is assessed against a named TeslaMate reference: fields, null/zero semantics, units, drives, charges, sleep/awake state, gaps, reconnects, duplicate delivery, retention, migration and derived values. Record supported, absent and inapplicable source fields. Prove durable passive observations separately from synthetic regression tests.
- All user data, installation identity, TLS material, pairing state and configuration survive supported upgrades. Uninstall preserves data by default; destructive purge is explicit. A failed upgrade must have a tested recovery path.
- Product versions may move together for a tested cohort. Product version, HTTP profile, schema identity and storage format remain separate compatibility axes. Do not weaken strict validators to admit unfinished implementations.

## Technology and support

- Active platforms are Debian 13 ARM64 and macOS on Apple silicon. Preserve Hub's macOS 13+ floor and each companion's declared supported runtime. The existing ARM64 guests and appropriate local Mac are the development targets. All x86/amd64/Intel Mac work and final Azure/full cross-architecture acceptance are paused until the owner resumes them; preserve their code and deferred requirements without blocking the active aim.
- Preserve each companion's declared language/runtime and platform floor. Record a new dependency's minimum OS, runtime, compiler, libc and service-manager requirements before upgrading it.
- Prefer current stable technology and supported security updates. Pin the accepted compiler/tool versions and package/image digests per candidate. Test both the declared floor and a current supported environment. A new toolchain does not authorize raising the supported deployment target.
- Retain Rust, SQLite, AppKit, the current web stack and the existing HA integration unless evidence shows a specific replacement is necessary. Avoid speculative rewrites and new infrastructure merely to use a newer technology.
- GitHub is source storage only: no CI, releases, tags, registries or artifact uploads. Do not commit or push without a current explicit instruction. Build packages and retain necessary verification evidence locally.

## Execution and authority

The master plan is `docs/development/MASTER_PLAN.md`. Each product has exactly one active plan at `<product>/docs/development/PLAN.md`. Old plans are superseded by redirects; original documents remain in a clearly marked historical archive. Existing evidence and wire specifications remain inputs, subject to fresh source verification.

Work stays in the existing independent `main` checkouts. One writer owns each repository. Hub owns Hub source, common packaging and the installed acceptance runner. A companion requests a Hub change through a bounded handoff; it must not edit Hub concurrently.

This planning run may prepare local development environments and remove verified disposable files as explicitly authorized. Product implementation and meaningful test execution are now authorized. After the owner deleted the old native goals on 2026-09-09, all six in-scope existing tasks are authorized to verify native state, create one fresh ARM64/Mac working-product goal when none remains unfinished, and verify the actual objective/status under ACTIVE_SCOPE.md. Keep broader acceptance backlog separate and create no duplicate tasks. The hourly coordinator continues to route ready ARM64/Mac work and concrete shared-runtime handoffs. Production/live vehicle changes remain separately controlled.

Future completion means a working integrated product with accepted delivery routes and documented limits. Completion of this planning run means the plans, task goals, environment preparation and cleanup are accurately delivered; it is not product acceptance.

## Shared VM access

Use [VM_ACCESS.md](VM_ACCESS.md) for the prepared `debian13-arm64` and `macos13-arm64` guests, verified SSH/login instructions and private credential locations. Use the shared wrapper and lab directories; do not create per-product duplicate VMs or reuse retained baseline/quarantine guests as fresh development environments. Docker work belongs in the primary 32 GiB Debian guest when that development step is started.
