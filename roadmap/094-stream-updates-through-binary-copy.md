---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream updates through binary COPY

Blocked by: [Stream geofences through binary COPY](093-stream-geofences-through-binary-copy.md).

## Question

Can firmware update facts use the reviewed binary projection without changing
latest-firmware ordering or nullable update handling?

## Starting recommendation

Use the fixed five-column layout and retain current timestamp and version
decoding rules.

## Resolution

Updates now use the fixed five-column binary COPY stream. Latest-firmware
ordering, nullable end/version fields, and timestamp failure behavior remain
unchanged.
