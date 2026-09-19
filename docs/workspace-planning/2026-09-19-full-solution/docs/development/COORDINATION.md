# Team coordination — 2026-09-18

One coordinator in the new task owns the outcome, dependency order and acceptance.
The required outcome is all F0–F7 and every active product complete. G0–G7 and HA's
bounded Container result are intermediate accepted evidence. HA is mandatory for
overall completion; optional deployment is not optional product work. Missing real-data
input remains a blocker to full acceptance while other ready implementation continues.
Use GPT-5.6 Sol/max for coordination; Sol/high for implementation and technical review;
Luna/max for bounded read-only exploration. No fast mode or silent model substitution.
Old product chats are archived and their queues are obsolete. Use bounded subagents,
not persistent chat-per-product loops.

## Dispatch and ownership

Read MASTER_PLAN, TASKS and the affected PLAN/STATUS once; then read only changed
files and needed evidence. Give each worker an exact deliverable, owned paths,
acceptance command/journey and stop condition. Default to at most two useful workers
in parallel, with at most one writer per repository. Add a worker only for independent
ready work. The coordinator alone edits root planning records. Hub owns shared VMs,
fixtures, installer and runtime integration; companions request exact dependencies.

Before replacing an owner, confirm the prior worker has stopped and released its
paths. A queued message is not execution: require a running acknowledgement or result.
If task delivery fails, diagnose once and use a bounded replacement after ownership
is reconciled. Never keep reporting work as active solely because it was queued.

Workers communicate directly for a concrete dependency, with the coordinator copied
only when priority, scope or acceptance changes. Use plain text, usually <=120 words:

```text
RESULT: change or finding
EVIDENCE: path + check/journey + source identity
NEED: exact input and owner, or none
NEXT: one ready action
```

No status chatter, repeated inventories, secret values, full logs or copied plans.
Use evidence links and small diffs. Continue independent owned work when one path is
blocked. Ask the user only for truly missing authority/input; make routine decisions.

## Verification and progress

Execute the next meaningful gate, fix observed defects, then verify that behavior.
Do not add tests that mirror trivial document edits, repeatedly run green suites,
or require polished installers/full-matrix receipts before ordinary use.
Reuse source-valid evidence, but revalidate expiring inputs and rebuilt environments.
Track implementation, source verification and runtime acceptance separately.
Workers update their product STATUS and send material transitions. The coordinator
alone updates TASKS and root plans. Do this on material changes, not every tool call.

Only one heavy build or VM provisioning job at once. Acquire
`/Users/bolyki/dev/teslatlas-lab/locks/heavy-build` atomically; record actual owner/PID;
release on completion. Check stale ownership before removing any old lock.

## Schedule handover

2026-09-19 reconciliation: keep the schedule paused. The next goal is an unsent
draft; preparing it does not authorize schedule adoption or new implementation.
The historical handover procedure below applies only after later explicit authority.

The old hourly automation is saved PAUSED. The new coordinator may adopt the same
hourly automation once it has the new task ID and verified worker ownership: update
its target and prompt through the supported automation tool, then enable it. Never
run both coordinators. If no supported tool is available, keep the schedule paused
and report that limitation once; active development can continue in the task.
Notify only material completion, failure or a required user decision. Stay quiet for
unchanged state. Do not use hourly wakeups as the primary development executor.
