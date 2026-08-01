---
type: wayfinder:task
status: closed
parent: 000-map
---
# Define time, units, and numeric precision

Blocked by: [Define identity, ordering, and idempotency](012-define-identity-order-idempotency.md).

## Question

Which timestamp source, timezone rule, unit normalization, decimal scale, and
rounding semantics preserve TeslaMate-equivalent results?

## Starting recommendation

Store source timestamps and canonical SI values at lossless precision; apply
display conversions only at explicit compatibility boundaries.

## Resolution

Use UTC Unix epoch milliseconds for all Hub and projection timestamps, with
source observation and Hub receipt time kept separately. The frozen transport
contract intentionally omits timezone and sub-millisecond fields. Canonical
units are km, km/h, kW, kWh, Celsius, percent SOC, and WGS84 decimal degrees;
Tesla owner mile values convert once by exact `1.609344`. Continuous telemetry
remains finite binary64 in the current projection adapter and SQLite transport,
with no display rounding in ingestion or projection.
Money needs a later explicit scaled-decimal contract.
