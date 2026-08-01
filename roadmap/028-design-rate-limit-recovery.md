---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design rate-limit and transport recovery

Blocked by: [Design the Tesla API client](027-design-tesla-api-client.md).

## Question

How should per-account and per-vehicle backoff, jitter, circuit breaking,
retry budgets, and retry-after handling behave?

## Starting recommendation

Use persistent bounded backoff with classified failures and isolate each
vehicle so one fault cannot create account-wide request storms.

## Resolution

Persist retry state per account and source vehicle. A `429` honors a valid
delta `Retry-After`; invalid or missing values use TeslaMate's five-minute
fallback. The disabled-account rate-limit signal uses TeslaMate's fifteen-
minute hold. These account gates prevent all vehicle requests, while a
vehicle-unavailable result delays only its vehicle and never wakes it.

Transient faults use bounded exponential backoff with deterministic full
jitter. Match TeslaMate's per-vehicle fuse baseline: three non-timeout,
non-auth API failures in ten minutes opens a five-minute circuit. Auth or
scope failure disables collection pending explicit credential action. Restart
reloads gates, attempts, and budgets before issuing a request; success clears
only the affected vehicle's transient state.
