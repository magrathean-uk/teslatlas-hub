---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the collection state machine

Blocked by: [Freeze the no-wake and sleep policy](029-freeze-no-wake-policy.md).

## Question

What states, timers, guards, transitions, restart semantics, and source events
must match TeslaMate vehicle logging behavior?

## Starting recommendation

Specify a pure deterministic state machine first, then wrap all network,
clock, and persistence effects behind replayable commands.

## Resolution

The per-vehicle pure machine has start, offline, asleep, online, driving,
charging, updating, suspended, unknown, and quarantined states. Durable
discovery/data observations, timer expiry, classified failure, credential
action, and restart are inputs. It emits only replayable commands for schedule,
discovery, guarded data read, fact append, projection, retry update, and safe
quarantine; network and clock effects stay outside the machine.

Asleep, offline, suspended, and unknown schedule discovery only. An online
data observation may derive driving, charging, or updating. Open drive and
charge sessions close only on deterministic terminal evidence or safe timeout.
Restart resumes from the durable cursor; earlier or duplicate facts are no-ops,
and malformed facts quarantine without erasing the journal.
