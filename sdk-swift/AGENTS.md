# Teslatlas Swift SDK

Retired from active scope (owner, 2026-10-08). Nothing in Teslatlas consumes this
package; App v7 keeps its in-tree Hub client as the only Swift transport, and
independent Apple developers use the published OpenAPI contract with a generator.
Source and history are preserved; the repository is archived read-only after the
owner-requested push. Do not develop, build or publish it. A public Swift package is
added only when a non-App Apple consumer exists.

Legal files (`LICENSE`, `NOTICE`, `docs/legal/`, contributor terms, copyright and
attribution strings) are owner-controlled. Preserve existing changes; no branches,
worktrees or stashes; push only on an owner request.

<!-- clean-development-policy:v4 (clean-development 0.6.0; canonical: bolyki-dev/clean-development/docs/policy-snippet.md) -->
## Clean development (mandatory)

- Storage is routed for you: every zsh loads `~/dev/machine/env.zsh`; hooks pin the mode per session (`$CLEAN_DEV_MODE` is `external` when the 4TB is attached, else `internal`). Do not wrap commands, override or unset the routed variables.
- Temp files go to `$TMPDIR` (session scratch, deleted at session end), throwaway build output to `$CLEAN_DEV_BUILDS`, caches wherever their variables point. Never write caches, builds or temp files into `~`, `~/dev` or this repo outside its git-ignored build directories.
- Internal mode: before any download or build likely over 5 GB, stop and ask G (suggest waiting for the 4TB).
- This repo's `.clean-development.json` (schema v2) lists its git-ignored `buildDirs`/`depDirs`; keep it accurate when the build layout changes.
- No dot-folders in `~` or `~/dev`. `.env` files stay git-ignored; read them only when the task needs them; never commit them.
