# Teslatlas TypeScript client

Node.js client for the Hub read API. Follow `../../AGENTS.md` and
`../../docs/development/MASTER_PLAN.md`; the plan wins on process, model, scope and commit
policy. Only the Hub client (`src/hub`) is retained; the rich client, SSE, vehicle
commands, the vendored Protocol copy, the lock and sync scripts and the two
bearer-serving browser helpers are deleted under plan group G2-2. Types are then
generated from the in-repo contract. No browser client in v7. The package is not
published in v7. The retained code moves into the `magrathean-uk/teslatlas-hub`
workspace as the `sdk-typescript/` package, keeping its Apache-2.0 licence, after an
Authorisations row.

## Boundaries

- Derive public types from the canonical contract; use `camelCase` TypeScript names and
  lowercase-hyphenated documentation names.
- Keep credential persistence caller-owned. Preserve typed errors, ETags and cursor
  binding.
- Keep the pairing boundary intact: the Node claim transport verifies normal TLS trust,
  hostname and the invitation leaf pin on one connection.
- Do not turn fixture, unit or package checks into a claim about a live Hub.
- Legal files (`LICENSE`, `NOTICE`, `docs/legal/`, contributor terms, copyright and
  attribution strings) are owner-controlled: change them only on the owner's explicit
  instruction.
- No vehicle commands. Short local commit per reviewed batch; push only on an owner
  request. No CI, release automation, tags or publication.

## Working locally

Use the commands in `package.json` and run the smallest relevant check: `npm run build`
then `npm run typecheck`, `npm run test:unit`, `npm run protocol:check`, or
`npm run verify` for the full local suite. `npm run build` writes `dist/` in this
checkout. From the workspace root, run everything through
`scripts/dev/run.sh hub-sdk-typescript ...`.

<!-- clean-development-policy:v4 (clean-development 0.6.0; inherited from ../AGENTS.md) -->
## Clean development (mandatory)

Follow the [Hub clean-development policy](../AGENTS.md#clean-development-mandatory).
The Hub root `../.clean-development.json` (schema v2) governs this package, including
the git-ignored `dist/` and `node_modules/` directories. Cargo output stays in the
Hub's git-ignored `target/`; caches and scratch follow `~/dev/machine/env.zsh`.
Use the Hub runner shown above; the Hub's macOS app goes under `$TESLATLAS_LAB/build/hub/dist` and its Xcode
staging under `$TESLATLAS_DERIVED_DATA`.
