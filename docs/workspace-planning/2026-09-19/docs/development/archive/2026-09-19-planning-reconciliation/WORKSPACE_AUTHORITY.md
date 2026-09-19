# Workspace authority — 2026-09-18 reset

The owner's latest reset replaces older pause/restart, task, model and VM directions.
Historical bytes are preserved in [the reset archive](docs/development/archive/2026-09-18-reset/manifest.json).

## Scope and outcome

Deliver the Hub adoption readiness gates in [MASTER_PLAN.md](docs/development/MASTER_PLAN.md).
Active repositories: `hub/`, `teslatlas-sdk-typescript/`, `teslatlas-edge/`,
`teslatlas-sdk-swift/`, `teslatlas-protocol/`, and secondary `teslatlas-home-assistant/`.
The App has independent authority and is entirely outside this work. Do not touch
its source, plans, data, tasks, automation, build output or pinned toolchains.
Viewer is a final future product after the others work; preserve its source/evidence,
exclude it from every active gate and worker, and require a later explicit start.
x86/amd64/Intel/Azure work remains deferred.

## Execution authority

The owner authorizes reassessment, replacement of active planning documentation,
archival of old product tasks, verified disposable artifact cleanup, lean replacement
ARM64 VMs, and subsequent development by the new coordinator. No renewed approval is
needed for those bounded actions. The handoff preparation does not itself launch VM
installation or a new product implementation campaign.

Use the existing independent dirty `main` trees. No reset, clean, stash, branch,
worktree, clone, commit, push, publication or CI. `repository-roots/` backs live Git
storage and must be preserved. Never blanket-delete source, receipts, private runtime
roots, data stores, unique baselines or shared/App tooling.

The coordinator owns root plans and task/gate records. A Sol/high worker owns each
product's implementation and status while assigned. Hub owns shared environment,
fixture, installer and integration changes. One writer per repository; one heavy
build/VM installation at a time. Models and communication are defined in
[COORDINATION.md](docs/development/COORDINATION.md).

Use fresh runtime identities, credentials and handoffs after rebuilding VMs.
Historical closed cohorts remain closed. Owner authorization for isolated development
is not authority for production, Tesla account/vehicle actions, public ingress,
real collection or secrets disclosure. Missing named-source data does not block
independent development; record its acceptance limit honestly.
