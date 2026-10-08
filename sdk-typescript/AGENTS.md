# Teslatlas TypeScript client

Node.js client for the Hub read API. Follow `../AGENTS.md` and
`../docs/development/MASTER_PLAN.md`; the plan wins on process, model, scope and commit
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
- Keep credential and checkpoint persistence caller-owned. Preserve typed errors,
  ETags and cursor binding.
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
then `npm run typecheck`, `npm run test:unit`, `npm run test:conformance`, or
`npm run verify` for the full local suite. `npm run build` writes `dist/` in this
checkout. Run everything through `../scripts/dev/run.sh teslatlas-sdk-typescript ...`.

<!-- clean-development-policy:v2 (canonical text: ~/dev/source/dev-bootstrap/snippets/clean-development-policy.md) -->
## Clean development (mandatory)

Follow the Clean Development rules in the workspace `../AGENTS.md`: routed caches, builds and scratch outside the tree, `clean-development run --session session-only` for anything that writes output, and `dev-env-check`, `dev-projects-check` and `dev-audit` before finishing.
