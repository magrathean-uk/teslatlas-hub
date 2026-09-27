# Contributing

This repository contains the browser and Node.js bindings for Teslatlas. Keep changes focused on the public client, transport behaviour, protocol validation, examples, or package tooling. Hub implementation and application UI belong in their own repositories.

## Development

Use Node 26.7.0 and npm 11.19.0, as pinned in `package.json` and `.node-version`. These are reproducibility versions, not minimum supported versions. The package is private. Work from a source checkout and use a locally packed archive for external consumer checks.

In the multi-repository workspace, follow its active instructions and command wrapper. The commands below are package-level commands. `npm ci` installs dependencies; `npm run build` replaces this checkout's `dist/` directory. The build script does not provide an output-directory override.

```sh
npm ci
npm run build
npm run typecheck
npm run test:unit
```

The initial build provides declarations and artifacts needed by package-facing checks. `test:unit` also runs the build through its pretest hook. Browser conformance requires the Playwright Chromium runtime to be available; dependency installation alone is not evidence that it is installed.

| Change | Relevant checks |
| --- | --- |
| TypeScript API or transport | `npm run build`, `npm run typecheck`, `npm run test:unit` |
| Browser or shared transport | `npm run test:conformance` |
| Protocol inputs or generated files | `npm run protocol:check`, `npm run test:protocol` |
| Export map, declarations, build, or packaged files | `npm run pack:check`, `npm run test:package` |
| Formatting and lint | `npm run format:check`, `npm run lint` |

`npm run verify` runs the combined local verification sequence, including browser conformance and package checks. Choose focused checks while developing, then run the checks needed to support the final change. A passing local suite does not establish real-Hub or installed-service acceptance.

## Protocol and client boundaries

Treat `protocol/lock.json` as the input and generated-output manifest. The current lock is a content-bound Protocol candidate, not a plain Git-commit import. See [compatibility](../docs/compatibility.md) before regeneration. Do not hand-edit generated types or validators to conceal a contract mismatch.

Keep authorization, credential storage, and event checkpoints caller-owned. Preserve opaque cursors, ETags, event replay, safe errors, and one-shot command dispatch. Browser code must remain free of Node-only dependencies. Do not replace certificate checks with bypass flags to make a test pass.

## Integration and review

Real-Hub helpers need a fresh, explicitly scoped handoff with the exact package, endpoint, trust inputs, credentials, and cleanup owner. Do not reuse closed historical fixtures. The [package lifecycle](../docs/package-lifecycle.md), [Docker](../docs/docker.md), and [acceptance](../docs/development/archive/acceptance.md) documents describe those boundaries.

Review notes should describe the behaviour change, source revision, checks actually run, and remaining gaps. Do not include tokens, pairing invitations, private certificates, vehicle identifiers, or location data. Report vulnerabilities using [SECURITY.md](SECURITY.md).

GitHub is source storage for this project. Do not add CI, release automation, tags, registry publication, or deployment as an implied contribution step. Preserve existing changes and follow the workspace's branch and publication rules.

## Licensing

Keep the [Apache 2.0 licence](../LICENSE) and existing attribution intact. Identify third-party material and its licence when introducing it. See [licensing](../docs/legal/licensing.md). Contributions need no separate assignment or contributor agreement.
