---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the read-only verification window

Blocked by: [Design dual-run safety](063-design-dual-run-safety.md).

## Question

Which checks can prove Hub readiness for roughly a day without changing,
scheduling, stopping, or reconfiguring TeslaMate?

## Starting recommendation

Run Hub-owned health, database-growth, parity, credential, and rollback checks.
Produce a readiness report only. Never install a timer or service action against
TeslaMate; cutover remains an explicit operator action outside migration.

## Resolution

Use an approximately twenty-four-hour, Hub-owned manual verification window.
It repeatedly records Hub health and sealed migration parity, treats source
drift separately, validates Hub-only credential and rollback readiness, and
emits an advisory signed report. TeslaMate remains untouched and unscheduled;
cutover stays manual.
