---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design the Tesla API client

Blocked by: [Design credential lifecycle](026-design-token-lifecycle.md).

## Question

Which regional endpoints, redirects, TLS rules, response limits, schema drift,
timeouts, and retry classifications must the client handle?

## Starting recommendation

Pin official contracts, reject unsafe redirects and oversized responses, and
preserve unknown fields in the observation journal without trusting them.

## Resolution

The legacy compatibility client uses only the explicit HTTPS Owner API base and
GET-only `/api/1/products` plus online-vehicle `vehicle_data`; redirects are
rejected before a bearer can be replayed. Fleet source configuration chooses a
documented regional base explicitly and never changes region through a
redirect. TLS, credential-free bases, bounded four-MiB response bodies, and
content-free errors are mandatory.

Known envelope controls validate before projection while unknown vehicle-data
fields remain bounded observation data unless credential-shaped. The client has
one explicit request timeout and no internal retry. Its retry consumer must
separate auth or scope failure, vehicle-unavailable, rate-limit hints,
transient transport/server failure, and terminal protocol failure; subsequent
collection policy owns the retry decision.
