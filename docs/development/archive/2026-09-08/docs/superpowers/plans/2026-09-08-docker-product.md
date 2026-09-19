# Hub Docker Product Implementation Plan

> **Execution record:** The user authorized the bounded Hub implementation on 2026-09-08. Checkboxes track the remaining Docker product work; completed core packaging is recorded in the working-product plan's execution update.

**Goal:** Let users download verified compatible sources, build and operate the actual Teslatlas Hub ecosystem with Docker Compose, including Legacy collection, Fleet Telemetry, client pairing, persistent storage, recovery, and all six sibling companion projects, including their currently developing capabilities.

**Architecture:** Build the existing Rust CLI into a non-root Linux image. Compose runs Hub and optional Fleet companions as separate processes sharing a private network namespace, preserving their loopback contracts. Public Hub HTTPS and Fleet receiver TLS are separate from private ingestion. Reuse Hub's verified-source companion bootstrap for ecosystem acquisition and builds; give Viewer, Edge and Home Assistant their appropriate separate runtime or installation targets, and export protocol/SDK products through optional build tooling.

**Tech stack:** Existing Rust crate and lockfile, Debian 13 runtime, Dockerfile, Docker Compose, existing pinned Go companion sources and evidence scripts.

**Spec:** The scope and design below originated as the proposed specification for the user's planning request on 2026-09-08. It now guides the authorized implementation, while publication, registry pushes, releases and live deployment remain separately unauthorized.

## Constraints and inspected state

- Work in the existing `hub/` main checkout. Preserve unrelated changes; do not modify the App.
- Amendment, 2026-09-08: sibling ecosystem projects are required scope even while under development. The current Hub execution implements only the core image/Compose/static packaging boundary; downloading sources, companion containers and sibling edits remain future work. Read each sibling's `AGENTS.md` before any such edits and coordinate with its active development owner.
- Source distribution only. No registry pushes, GitHub Actions, Releases, binary uploads, commits, or tags without separate authorization. Docker support means a documented local source build.
- Do not relax TLS, device authentication, secret ownership, instance locks, or Fleet proxy loopback validation to make containers work.
- Keep product versions separate from protocol/schema versions. Derive image version metadata from `Cargo.toml`, currently `2026.36.2`; require Rust at least `1.98` and build with `--locked`.
- Current inspection: Hub HEAD `7fe8cb2`; existing modifications in compatibility execution state and installed-host preparation/tests are outside this plan.
- Actual GitHub landing README is `.github/README.md`, declared by `Cargo.toml`. Do not create a competing root README.
- Existing Docker interop tooling is test infrastructure, not product packaging.
- Live source inspection confirms six sibling roots: `../teslatlas-protocol`, `../teslatlas-sdk-typescript`, `../teslatlas-sdk-swift`, `../teslatlas-viewer`, `../teslatlas-home-assistant`, and `../teslatlas-edge`. Their READMEs describe implementations beyond the original core-only Docker scope.
- Reuse `scripts/bootstrap-companions.py` and `tools/companions/` via the existing `teslatlas-hub companions` route. `tools/companions/catalog-current.json` currently has no admitted cohorts; public download/install is not yet available. `docs/compatibility/execution-state.json` still records pending installed-matrix work and no publication. Reinspect those facts at execution time rather than freezing today's development status as permanent exclusions.
- `src/application/main/dispatch.rs` already runs `serve` in the foreground and handles SIGTERM. Service-management commands invoke host supervisors and must not be used as container lifecycle commands.
- `src/runtime/config.rs` currently requires Fleet Telemetry to use `127.0.0.1:8080`; the patched receiver hardcodes that HTTP destination. `src/api/server.rs` merges public and internal routes. A network namespace alone does not solve simultaneous public TLS and private Fleet ingestion.

## Design decisions

Choose source-built Compose with separate optional Fleet services. An all-in-one container would require another process supervisor; ordinary bridge networking for companions would require changing their loopback security contracts. Neither is needed.

Support targets are Linux `amd64` and `arm64`. Claim each only after its acceptance run. Docker Desktop on Apple silicon is an additional separately tested host experience; Windows and NAS products are not initially claimed. Docker runs the CLI service, not the AppKit management application or a newly invented web UI.

Use repository-root `Dockerfile`, `.dockerignore`, and `compose.yaml`, with container examples under `packaging/docker/`. The core image build context is this Hub repository only. Companion builds consume separately verified source snapshots and retained outputs; never widen the context to the entire workspace or copy arbitrary sibling directories. Exclude `.git`, `target`, unrelated build outputs, credentials, databases, and operator configuration from source inputs. Explicitly include source/legal inputs needed by the build and verified runtime outputs needed by each final image.

Run as a fixed non-root UID/GID (proposed `10001:10001`), use a read-only root filesystem, drop capabilities, set no-new-privileges, use a small `/tmp` tmpfs, and mount persistent state at `/var/lib/teslatlas-hub`. Configuration and TLS identity are read-only mounts under `/etc/teslatlas-hub`. Provide deliberate one-time volume ownership preparation; never recursively change ownership on every startup. Preserve strict secret modes rather than relaxing admission for Docker mounts.

Expose Hub TLS at container port `8443`, initially published on host loopback. Document an explicit LAN/VPN binding change with a matching trusted certificate and public URL. Never publish `8080` or command proxy `4445`. Keep TLS end to end; do not teach an HTTP reverse proxy to bypass Hub's authentication model.

Fleet services use `network_mode: service:hub`. Run the receiver on unprivileged port `8444`, with an optional Fleet Compose override publishing host `443` to `8444` on the namespace-owning Hub service. The command proxy stays at `https://127.0.0.1:4445/`; the receiver still forwards to private `http://127.0.0.1:8080/v1/internal/fleet-telemetry`. Mount only each companion's required keys/configuration and its own cache; do not give companions the Hub database or full secrets directory. Recreate the Fleet stack together when the namespace-owning Hub container is replaced.

## Task 1: Separate private Fleet ingress from public HTTPS

**Files:** Modify `src/runtime/config.rs`, `src/runtime/config/tests.rs`, `src/api/server.rs` and existing server tests; update `docs/guides/configuration.md` and `docs/architecture/security-model.md` with the resulting listener contract.

**Interface:** Preserve the receiver's fixed loopback HTTP endpoint. Allow Fleet with a public TLS Hub listener without admitting non-loopback plaintext or a remote command proxy.

- [ ] Add regression cases for existing plaintext Fleet configuration, Fleet with `0.0.0.0:8443` plus valid TLS, invalid public plaintext, listener collision, and a non-loopback proxy.
- [ ] Retain existing combined loopback behavior for current installations. In Fleet + public TLS mode, create a separate fixed `127.0.0.1:8080` ingress listener containing only the internal route; omit that route entirely from the public router.
- [ ] Start and stop both listeners under existing supervision. A bind failure or unexpected termination must fail the service and stop its sibling, not leave half a working installation.
- [ ] Prove public ingestion requests cannot reach the handler even with the correct ingestion token, private requests require the token, pairing/sync require the existing TLS authentication flow, and SIGTERM releases both ports and the data lock.
- [ ] Keep the current receiver patch unchanged; make no wire-profile or schema changes. Run required Rust gates and relevant existing server tests.

## Task 2: Build and operate the standalone Hub image

**Create:** `Dockerfile`, `.dockerignore`, `compose.yaml`, `packaging/docker/config.toml.example`, `packaging/docker/prepare-volumes.sh`, `scripts/test-docker-packaging.sh`.

**Interface:** Entrypoint is the Hub binary; default arguments are `--config /etc/teslatlas-hub/config.toml serve`. Compose supplies the data volume and configuration mounts. No service installation runs inside the image.

- [ ] Verify available builder/runtime image tags and record immutable base digests during implementation. Use a multi-stage locked release build; include CA certificates, the required runtime libraries, licence/notices and source identity. Keep compilers and source credentials out of the runtime layer.
- [ ] Make the documented config select TLS on `0.0.0.0:8443`, the persistent data directory, and disabled geocoder/terrain defaults. Supply operator-generated certificates rather than baking an identity into the image.
- [ ] Make first-run volume preparation explicit, scoped to newly created named volumes, and safe to repeat without changing existing files. Verify Linux ownership and secret admission; document how bind mounts differ.
- [ ] Document and exercise this command sequence after copying/configuring the examples:

  ```sh
  docker compose build hub
  docker compose run --rm hub --config /etc/teslatlas-hub/config.toml bootstrap
  # Run setup using the exact CLI's bounded stdin/private-file options.
  docker compose up -d hub
  docker compose exec hub teslatlas-hub --config /etc/teslatlas-hub/config.toml status
  docker compose logs --tail 100 hub
  docker compose stop hub
  ```

- [ ] Supply the exact `setup --help`-verified token-input command in the final guide. Never use tokens as environment variables, build arguments, command-line values, or checked-in `.env` content. Stop the resident process before offline credential/import/restore commands.
- [ ] Add a bounded HTTP health probe against `/healthz` using certificate validation and a runtime HTTP client; no `-k`. Treat `/readyz` and recent observations as separate readiness/collection evidence. Do not use `doctor` or `preflight` as periodic health probes. A sleeping car must not cause a restart loop.
- [ ] Configure log rotation, a finite stop grace period (initially 30 seconds), and restart policy. Validate clean SIGTERM exit within that period, read-only runtime operation, restart persistence, wrong-owner errors, missing TLS errors, and concurrent-writer rejection.

## Task 3: Add the complete optional Fleet stack

**Create:** `packaging/docker/Dockerfile.command-proxy`, `packaging/docker/Dockerfile.fleet-telemetry`, `packaging/docker/compose.fleet.yaml`, `packaging/docker/config.fleet.toml.example`, `packaging/docker/fleet-telemetry.json.example`, `scripts/build-docker-fleet.sh`.

**Reuse:** `packaging/tesla-command-proxy/`, `packaging/fleet-telemetry-bridge/`, `scripts/go-proxy-evidence.py`, `scripts/fleet-telemetry-evidence.py`, and `scripts/legal-bundle.py`.

- [ ] Build the existing reviewed/patched companions from their pinned sources. Validate architecture-specific evidence and retain the matching source/legal corpus; do not substitute unpatched upstream `latest` images or silently omit failed companions.
- [ ] Implement the namespace, port and least-access mount design above. Pass the receiver bearer through its existing file-path setting. Configure public receiver certificates, proxy certificate and key, command-signing key, and independent proxy session-cache persistence.
- [ ] Provide the exact Compose base-plus-override command throughout Fleet documentation. Start the proxy for Fleet preflight/configuration while resident Hub collection is stopped, then start Hub and receiver. Verify one-off setup containers can reach the proxy in that namespace; do not assume `compose run` has the resident service's loopback.
- [ ] Document developer registration, virtual-key pairing, certificate requirements, renewal and ingestion-token rotation by linking existing Fleet setup. Initial vehicle configuration is an explicit operator action, never an automatic container startup script.
- [ ] Verify telemetry receipt and acknowledgement with synthetic input, rejection of missing/invalid bearer, no external ingress/proxy access, and absence of paid polling fallback. Test receiver/proxy failures and whole-stack recreation without losing state.

## Task 3A: Acquire and build the complete developing ecosystem

**Create:** `packaging/docker/Dockerfile.companion-builder`, `scripts/build-docker-companions.py`, `scripts/test-docker-companions.py`, `docs/guides/docker-companions.md`.

**Reuse/extend only where necessary:** `tools/companions/{core,recipes,targets,cli}.py`, their existing schemas/tests, and `docs/guides/companion-setup.md`. Keep fixed recipes and one compatibility catalog authority; do not introduce a second Docker-specific source resolver.

| Sibling project | Required Docker deliverable | Development boundary to preserve |
| --- | --- | --- |
| `teslatlas-protocol` | Verified contracts, conformance tooling and exportable source in the optional builder | Contract versions are distinct from Hub product versions; this is not a resident service |
| `teslatlas-sdk-typescript` | Locked package build/export and exact artifact supplied to Viewer | Preserve the current-Hub adapter independently of the richer protocol API; no npm publication required |
| `teslatlas-sdk-swift` | Linux SwiftPM build/test and source/product export | Apple runtime validation stays on Apple hosts; unresolved installed compatibility remains visible |
| `teslatlas-viewer` | Optional production static Viewer service built with its cohort's exact SDK package | Live mode must exercise actual Hub routes; fixture mode and unsupported views remain explicitly labelled |
| `teslatlas-home-assistant` | Verified custom-integration installation for an existing HA container, plus disposable HA container acceptance | Candidate is not production-admitted or HACS-published merely because it builds |
| `teslatlas-edge` | Optional independently deployable Edge and receiver containers with persistent encrypted spool | Preserve mTLS, bearer rotation and acknowledgement semantics; current README marks v2 spool upgrade forward-only and live vehicle proof separate |

- [ ] Implement a one-shot builder that runs the existing bootstrap as an unprivileged user, with the complete selected recipe toolchains and a private persistent companion prefix. Keep Python/build tools and downloaded dependency caches outside the resident Hub image; include the packaged helper/runtime if the resident CLI advertises companion commands, and direct build operations to the builder service.
- [ ] Published mode uses allowlisted origins, complete commit IDs, source digests, locked dependencies, profile bindings and accepted artifact hashes from an admitted Hub-compatible cohort. Preserve `install` versus explicit catalog-refreshing `update`. Fetch sources during the explicit build/update action, never whenever Hub restarts; no fallback to moving `main`, `latest`, or public package substitutes.
- [ ] Local development mode uses the existing explicit `local-candidate` catalog and byte-complete local-source manifest for the six sibling roots. Snapshot their current content without changing refs or cleaning dirty files; reject inputs changed during capture. Bind output receipts to the snapshots, not merely Git HEAD, and label outputs `local-unpublished`.
- [ ] Allow all six components to be selected now in development mode. A component failing its build or compatibility checks stays in scope with a precise blocked result; do not silently omit it or advertise it as supported. Maintain separate per-component build, runtime, installed-matrix and public-availability results.
- [ ] Reuse the bootstrap's dependency order: the SDK artifact must be verified before insertion into the Viewer snapshot. Export protocol and Swift outputs for consumers without adding idle protocol/SDK daemon containers. Use each recipe's actual pinned toolchain, including Node/npm, Swift and Edge's Rust plus receiver Go toolchain.
- [ ] Preserve transactional installation, verified output manifests, operation locks, offline identical-input reuse and rollback. Build new images from completed verified outputs, then explicitly recreate selected runtime services; changing a builder's active symlink must not mutate an already running image.
- [ ] Test empty public catalog, unavailable immutable commit, digest mismatch, incompatible Hub version/profile, dependency failure, changed local snapshot, interrupted install, output tampering, offline reuse and rollback. Each failure must leave prior working images/installation intact and expose a redacted actionable error.

## Task 3B: Integrate Viewer, Home Assistant and Edge runtimes

**Create in Hub:** `packaging/docker/Dockerfile.viewer`, `packaging/docker/compose.viewer.yaml`, `packaging/docker/compose.home-assistant-test.yaml`, `packaging/docker/compose.edge.yaml`, `packaging/docker/Dockerfile.edge`, `packaging/docker/Dockerfile.edge-receiver`, `scripts/test-docker-ecosystem.sh`.

**Sibling implementation seams to inspect before editing:** Viewer `package.json`, `bin/teslatlas-viewer.mjs` and live browser tests; Home Assistant `custom_components/teslatlas_hub` and live-Hub test entrypoint; Edge native packaging/build scripts and delivery integration tests; both SDKs' current-Hub tests; protocol profile conformance cases. Container orchestration belongs in Hub unless a missing product interface requires a focused sibling change.

- [ ] Viewer: run its existing packaged Node static server, not Vite dev/preview. Provide an explicit optional Compose service, browser-reachable Hub URL, trusted Hub TLS, configured allowed origin and clear live-mode entry. Browser requests originate on the user's machine: never use a Docker-only service hostname as the browser Hub URL. Keep paired credentials in memory as the current Viewer does and out of bundled JS/build arguments. Default port publication to loopback; document TLS hosting for remote Viewer access.
- [ ] Viewer acceptance: open the actual container-served bundle in a browser, pair to the containerized Hub, read vehicles/current state/drives, exercise session expiry and unsupported resources, and verify no fixture data is substituted after a live failure. Keep it described as the reference Viewer, not the native Hub administration app.
- [ ] Home Assistant: install only `custom_components/teslatlas_hub` using the verified bootstrap's target semantics. Its immutable-release symlink destination must be mounted at the same absolute path inside HA, or add a reviewed container-aware target recipe with transactional ownership checks; do not produce a host-valid but container-broken link. Preserve existing HA configuration, registry data and unrelated integrations. Require an explicit HA reload/restart after code activation; do not bundle or start a replacement production HA instance.
- [ ] Home Assistant acceptance: use an isolated HA configuration volume and pinned supported HA image; exercise config flow, trusted TLS pairing, sensor updates, unavailable state, reauthentication and unload against Docker Hub. No Tesla credentials enter HA. Keep candidate/publication restrictions visible until the integration's own release gates are satisfied.
- [ ] Edge: package its current Rust service and its own reviewed receiver bridge, which is distinct from the direct Hub Fleet bridge. Support deployment on a different Linux Docker host using a documented Hub-reachable TLS endpoint, private loopback ingestion and only the intended Edge delivery port published. Persist encrypted spool, keys and sequence state separately from immutable images. Do not place Tesla account tokens or vehicle-command keys in Edge.
- [ ] Edge acceptance: drive synthetic receiver envelopes through durable spool, Hub pull/commit/ack, retries, deduplication, restart and reconnect, gap/loss notices, full spool, bearer rotation and invalid mTLS identity. Tie acceptance to the exact admitted Edge delivery profile and Hub implementation. Do not silently downgrade v2 or bypass a pending Hub v2 gate. A downgrade after spool migration requires the documented compatible restore route.
- [ ] Make direct Fleet ingestion and Edge delivery explicit alternative collection configurations; the current Hub config rejects enabling both. Test rejection and document migration/cutover without duplicate collectors. Viewer and HA use public authenticated HTTPS and do not share the private Fleet namespace or mount Hub storage.
- [ ] Feed Docker ecosystem receipts into the existing compatibility review process without overwriting unrelated execution state. SDK Linux smoke tests and protocol conformance supplement actual Viewer, HA and Edge runtime tests; they do not replace them.

## Task 4: Verify persistence, upgrades and supported hosts

**Create:** `scripts/test-docker-runtime.sh`; extend the packaging script from Task 2. Store redacted acceptance results separately from product support claims.

- [ ] Run `docker compose config` for base and Fleet variants. Build and run on native Linux amd64 and arm64; emulation is build evidence only. Verify Docker Desktop separately before listing it as supported.
- [ ] Exercise fresh bootstrap/setup, trusted TLS pairing, authenticated sync, persistent restart/recreation, both collection modes with mocks, failed configuration, and graceful shutdown. Test from outside the container network namespace as well as inside it.
- [ ] Use existing backup, verify-backup and encrypted credential recovery commands with explicitly mounted backup destinations. Prove restoration into a new volume, fresh client pairing, and synthetic observation continuity. Never recommend copying a running SQLite file alone.
- [ ] Rehearse upgrade using old and new locally built image identities against disposable data. Back up first; rollback after a storage migration means restoring a compatible backup, not blindly selecting an older image.
- [ ] Run `cargo check`, `cargo test`, and `cargo clippy --all-targets` for runtime changes, plus Docker scripts and affected existing packaging gates. Do not run real vehicle actions in automated tests.
- [ ] Record exact commit, image/base digests, architecture, Docker/Compose versions, passed cases and gaps. Live Fleet acceptance requires separately authorized deployment and a fresh durable receipt; container health is not evidence of live collection.
- [ ] Extend the matrix to all six ecosystem components, including build/export checks for protocol and SDKs, browser-level Viewer acceptance, installed HA acceptance and Edge delivery acceptance. Run the combined supported stack as well as component checks. Rehearse cohort upgrade and rollback while keeping HA configuration, Edge spool and Hub storage compatible; use coordinated restore when data migrations preclude image-only rollback.

## Task 5: Make Docker discoverable and usable

**Create:** `docs/guides/install-docker.md`.

**Modify:** `.github/README.md`, `docs/index.md`, `docs/guides/getting-started.md`, `docs/guides/build-from-source.md`, `docs/guides/configuration.md`, `docs/guides/cli.md`, `docs/guides/fleet-setup.md`, `docs/guides/troubleshooting.md`, `docs/operations/backup-and-recovery.md`, `docs/releases/upgrade.md`, and `.github/SUPPORT.md`.

- [ ] Write one complete install guide: prerequisites, locally building images, volume ownership, TLS, Legacy setup, optional Fleet setup, status/logs, pairing, stop/start, upgrades, backup/restore, and uninstall preserving data. Identify volume deletion as destructive and keep it out of ordinary uninstall commands.
- [ ] Add Docker beside macOS/Debian in the README introduction, install links, first-setup steps and guide table. Proposed copy after validation: “Run Teslatlas Hub on macOS, Debian, or with Docker Compose. Docker images are built locally from this repository; see the Docker installation guide.”
- [ ] State that Docker uses CLI administration and that the Fleet stack requires additional companions and Tesla setup. Do not imply a bundled GUI, one-click Fleet account authorization, published image registry, or universal NAS compatibility.
- [ ] Add Docker invocation examples to CLI and operations docs. Explain `exec` for live-safe commands versus `run --rm` with the resident process stopped for commands taking the exclusive store lock. Verify every example against the exact built binary.
- [ ] Cover certificate trust, host binding, file ownership, unhealthy versus unconfigured service, Fleet namespace replacement, port conflicts, storage permissions, and redacted diagnostic collection in troubleshooting/support.
- [ ] Check relative links from `.github/README.md` and run the install guide from a clean disposable checkout. Advertise only architectures and modes that pass Task 4; report any remaining gap explicitly.
- [ ] Add an ecosystem selection table and development-status section to the Docker guide and Hub README: Hub core, direct Fleet, Viewer, HA integration, Edge alternative and protocol/SDK tools. Document exact source download/build/activate/update/rollback commands and the distinction between published and local-candidate modes. Include planned components even before public admission, labelled under development rather than omitted.
- [ ] Update `docs/guides/companion-setup.md` and `docs/guides/docker-companions.md` together. Add reciprocal Docker-guide links and accurate component-specific instructions to the six sibling `README.md` files during implementation, with their native build/use paths retained. Do not claim registry images, HACS publication or Apple SDK validation based on Docker evidence.

## Completion boundary

The planned implementation encompasses Hub and all six sibling projects. It is complete when a new operator can follow the checked-in guides to acquire an admitted source cohort, build selected products, configure/run/pair services, and upgrade/restore them with Docker. Unpublished development cohorts must have an explicit tested local-candidate route. Core Docker support may be reported separately, but pending companion acceptance or an empty public catalog leaves the overall ecosystem objective incomplete and must remain visible. Documentation and source changes can then be reviewed for a separately authorized GitHub source update. No registry publication is needed for this design.

The original planning task changed only this document. The subsequent authorized execution added the bounded core Docker packaging and documentation files listed in the working-product execution update; companion acceptance and publication remain pending.
