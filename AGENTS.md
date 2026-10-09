# Teslatlas Hub

Rust telemetry and sync service, with an AppKit control app. See `Cargo.toml` and
[Source layout](docs/architecture/source-layout.md) for ownership boundaries.
The crate forbids unsafe Rust.

## Scope

- In the Teslatlas workspace, follow `../AGENTS.md` and the programme plan at
  `../docs/development/MASTER_PLAN.md`; the plan wins on process, model, scope and
  commit policy. Keep status there; old plans and receipts are background.
- Goal 2 (see the plan): complete the Mac Hub. Ships as the GUI app with its per-user
  LaunchAgent only; the `.pkg` installer, its pre/postinstall scripts and the Rust
  `install`/`service` verbs are retired. In scope: macOS 13 runtime verification, local
  Mac notifications, keep-awake, optional encrypted backups, per-device car scope,
  the JSON read API profile and the physical live sync wire. Linux, Debian, Docker and
  Pi are specified now and built after the Mac Hub is complete. No browser client in v7.
  Edge is retired (source preserved). Viewer is excluded. Home Assistant is the
  reference third-party consumer and must keep working at every API change.
- This repository becomes the OSS Cargo workspace holding Hub, Protocol, Compute and
  the TypeScript Hub client, with per-package licences, after an Authorisations row.
- Work in the requested checkout. Preserve unrelated edits and private data. Do not
  create branches, worktrees or stashes.
- Short local commit per reviewed batch; push only on an owner request. GitHub is
  source storage: no CI, automated security workflows, tags, releases, binary
  publication, signing or deployment.
- No vehicle commands; no public ingress operated by Magrathean. Never commit
  credentials, private logs, location rows or real telemetry fixtures.
- Wire-contract changes start in the Protocol package (the sibling checkout until the
  authorised move). App changes belong to the App writer.
- One writer per repository; helpers are read-only.

## Validate the changed behaviour

In the owner's workspace, use its runner and heavy-build lock. The workspace pins
rustup 1.98.1. From this directory, choose the relevant check:

```sh
../scripts/dev/run.sh hub cargo check --locked
../scripts/dev/run.sh hub cargo test --locked --lib TEST_FILTER
../scripts/dev/run.sh hub cargo clippy --locked --all-targets -- -D warnings
```

Replace `TEST_FILTER` with the affected test name. Broaden tests for shared behaviour
or regressions. Use the heavy-build lock for builds and platform tests.

For standalone source work, see [Build from source](docs/guides/build-from-source.md).
For documentation, check links and factual claims; do not build the app merely to
validate prose. Layout and provenance scripts inspect tracked paths:

```sh
python3 scripts/verify-repository-layout.py
python3 scripts/verify-provenance.py
```

Both gates must pass on the current tree. They read the tracked-file list
(`git ls-files`), so run them again after staging added or deleted paths, and classify
every new tracked file in `provenance-manifest.json`. Do not force-add or remove files
to satisfy them as part of a docs edit.

Legal files are owner-controlled (owner, 2026-09-27). These are `NOTICE`, `LICENSE`,
`.github/CONTRIBUTING.md`, `docs/legal/additional-terms.md`, `docs/legal/overview.md`,
`docs/legal/trademarks.md` and everything in `docs/governance/`. Change them, or their
checksum lock `docs/legal/owner-controlled-files.sha256`, only on the owner's
explicit instruction. Keep the copyright and attribution lines in `src/lib.rs` and the
macOS `AppDelegate.swift` exactly as they are.

AppKit changes use `scripts/test-macos-appkit-focused.sh TestClass/testMethod`
with an actual XCTest selector. The script selects Xcode-beta when installed,
then Xcode, unless `DEVELOPER_DIR` is explicitly set.

## Runtime and evidence

Unsigned source runs need `TESLATLAS_HUB_DEVELOPMENT=1` and an explicit
`TESLATLAS_HUB_DEVELOPMENT_MODE` of `fixture`, `standalone` or `private-lan`. Follow
[Source-run control app](macos/TeslatlasHubApp/DEVELOPMENT.md) for owned absolute
paths and TLS. Bind loopback by default; `private-lan` needs an exact RFC1918 IPv4
bind, a matching HTTPS URL and port, TLS and existing authentication. Use an identified
binary outside the product tree, never a `target/` symlink. `serve` holds a lifetime
lock; stop it before `pair`, `backup` or `repair`. While serving, use `/healthz` and
`/readyz`. Large-history measurements use the release CLI built through the managed
wrapper (`cargo build --locked --release --bin teslatlas-hub`); the history refresh
procedure is in that DEVELOPMENT.md. Runtime identities and ports of the lab Hubs are
in the plan's Goal 2 "Reference".

Complete the task's actual acceptance path. One short receipt: repo@sha, command,
host, result. A build, fixture or unit test does not prove live collection or the
App pairing and sync path.

## Pending URL migration

The next release must apply [NEXT-RELEASE-URLS.md](NEXT-RELEASE-URLS.md): product sites moved to `https://magrathean.uk/solutions/<slug>/` and support addresses to `contact+<slug>@magrathean.uk`. Remove this section with that file once released.

<!-- clean-development-policy:v2 (canonical text: ~/dev/source/dev-bootstrap/snippets/clean-development-policy.md) -->
## Clean development (mandatory)

Follow the Clean Development rules in the workspace `../AGENTS.md` (canonical text in the snippet above): routed caches, builds and scratch outside the tree, `clean-development run --session session-only` for anything that writes output, and `dev-env-check`, `dev-projects-check` and `dev-audit` before finishing.
