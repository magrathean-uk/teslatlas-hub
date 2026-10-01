# Teslatlas Hub

Rust telemetry and sync service, with an AppKit control app. See `Cargo.toml` and
[Source layout](docs/architecture/source-layout.md) for ownership boundaries.
The crate forbids unsafe Rust.

## Scope

- In the Teslatlas workspace, follow `../AGENTS.md` and the current programme
  plan at `../docs/development/MASTER_PLAN.md`. Keep status there; old plans and
  receipts are background, not current acceptance.
- Native Mac Dev acceptance N1–N4 is complete. Current I0–I6 work reviews and
  improves that accepted App/Hub system and required dependency boundaries; physical
  v7 phone and production-onboarding acceptance remain separate obligations. Packaging/distribution, other platforms, Viewer and Home Assistant
  are later work and do not gate this goal.
- Work in the requested checkout. Preserve unrelated edits and private data.
  Do not create branches, worktrees or stashes in the owner's workspace.
- Develop locally on the existing `main` and leave routine changes uncommitted.
  Commit and push only for an owner-requested source checkpoint or push. Identify
  tested edits by source base, changed paths and exact artifacts; skip routine SHA
  bookkeeping under the owner's 2026-09-30 instruction.
- GitHub is source storage. No new CI, automated security workflows, tags,
  releases, binary publication, signing or deployment without an explicit request.
  Pushes also require an explicit request.
- No vehicle commands, public ingress or accessibility work. Never commit
  credentials, private logs, location rows or real telemetry fixtures.
- Wire-contract changes start in the sibling Protocol repository. App changes
  belong to the App. Do not edit either as an incidental Hub change.
- Use `rg` and targeted reads. Use codebase-memory-mcp if structural navigation
  is needed; do not recreate CodeGraph. Do not search vendor, upstream,
  repository-roots, dependency or archived evidence trees.

Complete authorised local work and its necessary checks without repeated
permission requests. Use a small team for independent bounded work when useful,
with fresh context and one writer per file; follow the workspace model choices.

## Validate the changed behaviour

In the owner's workspace, use its existing runner and heavy-build coordination.
The workspace pins rustup 1.98.1. From this Hub directory, choose the relevant check:

```sh
../scripts/dev/run.sh hub cargo check --locked
../scripts/dev/run.sh hub cargo test --locked --lib TEST_FILTER
../scripts/dev/run.sh hub cargo clippy --locked --all-targets -- -D warnings
```

Replace `TEST_FILTER` with the affected test name. Broaden tests for shared
behaviour or regressions. Use the workspace heavy-build lock for builds and
platform tests; do not run competing heavy jobs.

For standalone source work, see [Build from source](docs/guides/build-from-source.md).
For documentation, check links and factual claims; do not build the app merely
to validate prose. Layout and provenance scripts inspect tracked paths:

```sh
python3 scripts/verify-repository-layout.py
python3 scripts/verify-provenance.py
```

Root AGENTS.md and CLAUDE.md are local guidance in this checkout. The layout gate
forbids tracking them. Do not change the gate or force-add them as part of a docs edit.

Legal files are owner-controlled (owner, 2026-09-27). These are `NOTICE`, `LICENSE`,
`.github/CONTRIBUTING.md`, `docs/legal/additional-terms.md`, `docs/legal/overview.md`,
`docs/legal/trademarks.md` and everything in `docs/governance/`. Change them, or their
checksum lock `docs/legal/owner-controlled-files.sha256`, only on the owner's
explicit instruction; otherwise the layout gate fails. Keep the copyright and
attribution lines in `src/lib.rs` and the macOS `AppDelegate.swift` exactly as they are.

AppKit changes use `scripts/test-macos-appkit-focused.sh TestClass/testMethod`
with an actual XCTest selector. The script selects Xcode-beta when installed,
then Xcode, unless `DEVELOPER_DIR` is explicitly set.

## Runtime and evidence

Unsigned source runs need `TESLATLAS_HUB_DEVELOPMENT=1` and an explicit
`fixture`, `standalone` or `edge` mode. Follow
[Source-run control app](macos/TeslatlasHubApp/DEVELOPMENT.md) for owned absolute
paths and TLS. Bind loopback by default; the current phone goal permits an explicitly
selected private LAN address with TLS and existing authentication. No public ingress;
leave the comparison Hub on port 21443 alone.
`serve` holds a lifetime lock; stop it before `pair`, `backup` or `repair`.
While serving, use `/healthz` and `/readyz` for health checks.

Complete the task's actual acceptance path. Record the source base, changed paths, exact artifact, command, host, result and
remaining gap. Keep product signature/content validation enabled. A build, fixture or unit test does
not prove the current goal's live collection and physical-phone pairing/sync path.

## Pending URL migration

The next release must apply [NEXT-RELEASE-URLS.md](NEXT-RELEASE-URLS.md): product sites moved to `https://magrathean.uk/solutions/<slug>/` and support addresses to `contact+<slug>@magrathean.uk`. Remove this section with that file once released.
