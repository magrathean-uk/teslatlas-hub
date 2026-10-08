# Teslatlas protocol

Follow `../AGENTS.md` and `../docs/development/MASTER_PLAN.md`; the plan wins on
process, model, scope and commit policy. This product's dated `docs/development/PLAN.md`
and `STATUS.json` are historical records. Work only on the assigned scope. The Hub,
the App (as App work only) and Home Assistant consume this product. Viewer is excluded.

Run commands through `../scripts/dev/run.sh teslatlas-protocol COMMAND...` so build
output and caches stay outside the tree.

This repository owns public contracts, not Hub implementation. It moves into the
`magrathean-uk/teslatlas-hub` Cargo workspace as the `protocol/` package, keeping its
Apache-2.0 licence, after an Authorisations row in the plan.

- Use lowercase, hyphenated documentation names and lowercase `snake_case` schema fields.
- Keep schemas source-neutral and independently implementable.
- Preserve semantic versioning, capability negotiation, opaque cursors, UTC timestamps, ETags and stable errors.
- Fixtures must be deterministic and redacted.
- Do not copy AGPL Hub implementation or proprietary Teslatlas source here.
- Legal files (`LICENSE`, `NOTICE`, `docs/legal/`, contributor terms, copyright and
  attribution strings) are owner-controlled: change them only on the owner's explicit
  instruction.

## Contract discipline

- Names, fields, algorithms, limits and semantics must be implementable by an
  unaffiliated client from public files.
- RFC 2119 and RFC 8174 define `MUST`, `MUST NOT`, `REQUIRED`, `SHOULD`, `SHOULD NOT`
  and `MAY` when written in uppercase.
- Every contract change updates the affected schemas, OpenAPI, examples, fixtures,
  profiles, conformance cases, generated artifacts and tests. Do not silently change
  wire behaviour.
- Machine-readable artifacts are the wire authority; an artifact/prose conflict blocks
  until resolved and verified by the local gate.
- Compatibility follows the plan: the canonical served profile is the new `hub-http-v1`
  profile plus `hub-sync-v1` 1.3.0/1.4.0; unserved families are deleted once under
  G2-0, and additive fields are tolerated.

## Current-Hub checker

- Preserve exact wire spellings where the current-Hub profile defines them, including
  `sourceUrl`, `pairingId`, `expiresAtMs` and `tlsPin`.
- `tools/check-current-hub` is a bounded, read-only remote-wire smoke. Its private
  configuration directory and bearer header remain owner-only and never enter
  examples, fixtures, output or diagnostics.

## Workspace boundaries

- Preserve existing work, private data and exact evidence bindings.
- GitHub is source storage only: no CI, releases, tags, binary publication, signing or
  deployment automation.
- Short local commit per reviewed batch; push only on an owner request.

<!-- clean-development-policy:v2 (canonical text: ~/dev/source/dev-bootstrap/snippets/clean-development-policy.md) -->
## Clean development (mandatory)

Follow the Clean Development rules in the workspace `../AGENTS.md`: routed caches, builds and scratch outside the tree, `clean-development run --session session-only` for anything that writes output, and `dev-env-check`, `dev-projects-check` and `dev-audit` before finishing.
