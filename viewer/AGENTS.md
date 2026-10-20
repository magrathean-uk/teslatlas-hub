# Teslatlas Viewer guidance

Teslatlas Viewer is a small public reference client for the Teslatlas Hub. It
demonstrates health, vehicles, current state, recent sessions, data quality,
freshness, and paired devices. It is not a protocol authority or Teslatlas
feature parity.

## Current direction

The Viewer is deferred under the current workspace plan. Do not build, test,
serve, pair, start Viewer or shared runtimes, consume Hub handoffs, or create
new Viewer evidence unless the owner explicitly defines a future scope.
The authorized documentation refresh is an exception to that hold. Do not turn
that exception into product work. The workspace also excludes accessibility work unless explicitly requested.

In the service workspace, the parent `docs/development/MASTER_PLAN.md` remains
the plan and status authority. Historical local plans and receipts do not
supersede it.

- Preserve unrelated edits, private data, and original receipts. Work in the
  existing checkout; do not create branches, worktrees, stashes, or candidate roots.
- GitHub is source storage. Do not add or enable CI, security automation,
  releases, tags, binary publication, signing, or deployment. Pushes need an
  explicit owner instruction; follow the parent plan's commit rules.
- Do not add vehicle commands or public ingress. Hub and shared-runtime changes
  belong to their owners and require a concrete authorized handoff.
- Carry authorized local work through its relevant acceptance checks. Delegate
  independent bounded work with clear file ownership when useful; do not ask
  again for routine steps already covered by the task.
- Use targeted source reads. If a structural code tool is needed, use
  `codebase-memory-mcp`; do not recreate CodeGraph tooling.

## Product boundaries

- Use public `@teslatlas/sdk` browser APIs only. `ViewerDataSource` is an
  application display seam, not a replacement transport API.
- Keep the SDK adapter as the network boundary. Views must not construct Hub
  URLs, call transport primitives, retain raw pairing material, or calculate
  private analytics.
- Keep live credentials and pairing material in memory only. Do not put them
  in URLs, browser storage, logs, copied errors, screenshots, fixtures, or Git.
  Clearing a local session must not be presented as remote device revocation.
- Preserve the distinction between absent, stale, inferred, degraded, offline,
  error, and complete data. Do not copy proprietary Teslatlas UI or analytics.

## When work resumes

Read [README.md](README.md), [the architecture](docs/architecture/overview.md), and
[privacy guidance](docs/legal/privacy.md) before changing the client boundary. Check
the relevant `package.json` scripts before running them. The relevant commands are `npm run sdk:verify`, `npm run typecheck`,
`npm test -- --run`, `npm run test:cli`, and `npm run build`. The SDK verifier
runs before type checking, unit tests, and builds. See [verification](docs/reference/verification.md)
for prerequisites, side effects, and narrower checks. Historical test results
do not prove today's source or an ordinary installed user path.

Choose browser specifications deliberately. The default Playwright selection is
not fixture-only because it also collects installed recovery and data-state
specifications. Use named fixture specifications such as
`e2e/viewer.spec.ts` or `e2e/screenshots.spec.ts` only when their execution is
authorized and their prerequisites are available.

For future development, consider
[Clean Development](https://github.com/magrathean-uk/clean-development). It is
optional and is not configured by this repository guidance.

## Documentation and policy

Keep public documentation free of credentials, private infrastructure details,
location data, internal account information, and unredacted receipts. The
[security policy](.github/SECURITY.md) describes the currently documented reporting
position. The root [LICENSE](LICENSE) controls the legal grant; see
[licensing notes](docs/legal/licensing.md) for repository-specific context.

Legal files (`LICENSE`, `NOTICE`, `docs/legal/`, contributor terms, copyright and
attribution strings) are owner-controlled: change them only on the owner's explicit
instruction.

<!-- clean-development-policy:v2 (canonical text: ~/dev/source/dev-bootstrap/snippets/clean-development-policy.md) -->
## Clean development (mandatory)

This project follows [Clean Development](https://github.com/magrathean-uk/clean-development) and the machine rule that nothing creates tool state under `~` (only the allow-listed agent homes).

- Load `~/dev/machine/env.zsh`; `~/.zshenv` does this for every zsh. Machine storage preferences live in `~/dev/machine/storage.conf`; workspace navigation is in `~/dev/START-HERE.md`.
- New sessions select `/Volumes/4TB/machine/cache` when the configured volume is actually mounted and writable, otherwise `~/dev/machine/cache` for ordinary smaller work. Use routed `DEVCACHE`, `CLEAN_DEVELOPMENT_{ROOT,CACHE_ROOT,BUILD_ROOT,SCRATCH_ROOT}`, `BOLYKI_DEV_LIBS` and `TMPDIR`; builds and scratch are beneath the selected cache. Keep active-session destinations fixed; stop if required storage disappears. Other users configure their own volume and fallback.
- SpecieX, large video/model jobs and VMs require external storage; never use internal fallback. Persistent labs, credentials, installed toolchains, source and final deliverables are not disposable caches.
- All active Git entry points belong under `~/dev/source/`, third-party repositories in `source/third-party/`, websites in `source/magrathean/`, and Teslatlas products/docs in `source/teslatlas-service/`. Do not read, scan, move or edit `~/dev/porn`.
- Preserve routed tool homes/caches, caller overrides and SDK/toolchain choices. Never unset or bypass routing. Scrubbed environments must restore the reviewed routing before cache-writing tools run; preserve intentional hermetic Git/Cargo isolation.
- Run builds, tests, installs and anything else that writes caches or build output through Clean Development: `clean-development run --session session-only -- <command>`. Follow its docs and keep its receipts.
- Do not add installers or scripts that default into `~` (`~/.cargo`, `~/.rustup`, `~/.cache`, `~/.npm`, `~/.swiftpm`, `~/.gradle`, ...) and do not hardcode `$HOME` paths for caches; use the routed variables.
- Before finishing, run `dev-env-check`, `dev-projects-check` and `dev-audit`; require passing environment/project checks and no new home-audit entries compared with the task baseline. Fix violations your work caused.
