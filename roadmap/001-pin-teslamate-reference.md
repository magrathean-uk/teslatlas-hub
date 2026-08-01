---
type: wayfinder:research
status: closed
parent: 000-map
assignee: Godel
---
# Pin the TeslaMate parity reference

Blocked by: none.

## Question

Which exact TeslaMate version and commit define parity, and what backend modules,
database migrations, state-machine behaviors, settings, and outputs belong to
that reference?

## Starting recommendation

Pin one released upstream commit, inventory it mechanically, and require every
later parity claim to name the matching reference behavior or an explicit
intentional deviation.

## Resolution

Use the TeslaMate `4.1.0-dev` development line from clean `origin/main`. The
research snapshot was commit `7054517c10475f39f480edeae8f90c6f717985a3`
(`v4.0.1-60-g7054517c`). Re-pin the clean upstream head only between Hub
development stages. Never use a dirty feature checkout as the parity reference.
