---
type: wayfinder:task
status: closed
parent: 000-map
---
# Write operator runbooks

Blocked by: [Design release and supply-chain trust](077-design-release-supply-chain.md).

## Question

Which installation, migration, health, repair, backup, restore, credential,
cutover, rollback, upgrade, and incident procedures must an operator follow?

## Starting recommendation

Make every normal path one command, every destructive path explicit, and every
runbook state its prerequisites, evidence, rollback, and secret-handling rules.

## Resolution

The runbooks define gated Hub-only normal paths and explicit recovery paths for
installation through incidents. Every path records evidence, protects secrets,
and forbids TeslaMate mutation; cutover uses only an operator-executed signed
plan and rollback seals Hub-only history first.
