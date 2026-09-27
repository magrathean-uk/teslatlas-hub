# Teslatlas TypeScript SDK

This repository owns the public browser and Node.js transport bindings for
Teslatlas. When working in the multi-repository workspace, follow its `AGENTS.md` and active programme plan first. Keep work within the SDK:
do not add viewer UI, Hub implementation, server-held credentials, CI, release
automation, tags, or publication without an explicit owner instruction.

## Boundaries

- Derive public types and protocol behavior from the locked protocol artifacts.
- Use `camelCase` TypeScript names and lowercase-hyphenated documentation names.
- Keep credential and replay-checkpoint persistence caller-owned and browser-safe.
- Preserve typed errors, ETags, cursor binding, SSE replay handling, and the
  one-shot behavior of non-idempotent commands.
- Keep the current-Hub pairing boundary intact: Node claim transport verifies
  normal TLS trust, hostname, and the invitation leaf pin on one connection;
  browser pairing fails closed unless the embedding supplies a pin-capable
  claim transport.
- Do not turn fixture, unit, or package checks into a claim about a live Hub or
  ordinary browser acceptance.
- Legal files (`LICENSE`, `NOTICE`, `docs/legal/`, contributor terms, copyright and attribution strings) are owner-controlled: change them only on the owner's explicit instruction.

Preserve existing changes and private data. In the current workspace, keep the existing `main` checkout; do not create branches, worktrees, or stashes. Pushes and vehicle commands need an explicit owner instruction. Read the [contribution guide](.github/CONTRIBUTING.md) for command prerequisites and [development record](docs/development/PLAN.md) for the distinction between current authority and historical evidence.

## Working locally

Use the commands defined in `package.json` and run the smallest relevant check:
`npm run build` followed by `npm run typecheck`, `npm run test:unit`, `npm run test:conformance`,
`npm run protocol:check`, or `npm run verify` for the full local suite.
`npm run build` writes `dist/` in this checkout, so do not claim that cache
routing alone keeps every generated output outside the repository.

For optional cache and output management, see
[Clean Development](https://github.com/magrathean-uk/clean-development).
