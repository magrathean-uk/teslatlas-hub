---
type: wayfinder:task
status: closed
parent: 000-map
---
# Set reliability objectives

Blocked by: [Define compatibility surfaces](004-define-compatibility-surfaces.md).

## Question

What data-loss, duplication, recovery-time, availability, and stale-data limits
must define “more robust and reliable than TeslaMate”?

## Starting recommendation

Set zero acknowledged-observation loss, deterministic replay, bounded recovery,
and explicit freshness objectives rather than a vague superiority claim.

## Locked baseline

- Driving freshness: at most 5 seconds.
- Charging freshness: at most 10 seconds.
- Ordinary online freshness: at most 75 seconds.
- Asleep or offline: no freshness promise; expose last-observation age and
  never wake.
- Crash to truthful readiness: at most 60 seconds on a supported baseline host.
- Durably acknowledged observations: zero loss.
- Retries and replay: zero duplicate projected facts.

Derive remaining targets from pinned TeslaMate behavior, Hub measurements, and
supported-host benchmarks. Do not ask the user for arbitrary numbers.

## Resolution

Hub reliability is measured at the durable Hub boundary, not by an unqualified
availability percentage on a single owner-operated host.

- A successful durable acknowledgement means the observation and its idempotency
  identity are committed locally. Such an acknowledgement permits zero loss.
- Retrying an acknowledged source observation, or replaying it after restart,
  produces zero duplicate projected facts. A failed or interrupted operation
  has no acknowledgement and may be retried from its stable source identity.
- After a clean crash, Hub must reach either truthful readiness or explicit
  not-ready within 60 seconds on a supported baseline host. It must never serve
  a ready response while local integrity fails or recovery is incomplete.
- Freshness is an operational target only while an authorized source is
  reachable and collection is enabled: driving <=5 seconds, charging <=10
  seconds, ordinary online <=75 seconds. These preserve margin over the pinned
  TeslaMate cadence (2.5, 5, and 60 seconds respectively).
- Asleep or offline vehicles have no freshness promise and must expose the age
  of the last durable observation. Hub never wakes a vehicle to satisfy a
  freshness target.
- Transport and source outages are observable failures: retain the last good
  history, back off without duplicate facts, and recover automatically when the
  source returns. They are not silently represented as current data.
- A percentage availability SLO is deliberately not claimed until ticket 006
  defines the supported host envelope and ticket 008 supplies measured evidence.

Proof requires crash/replay, corruption/readiness, idempotency, and cadence
tests on the later defined baseline host; an HTTP health response alone is not
reliability evidence.
