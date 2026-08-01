---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream history positions through binary COPY

Blocked by: [Stream updates through binary COPY](094-stream-updates-through-binary-copy.md).

## Question

Can the history-reader position path share the reviewed thirty-column binary
decoder and retain its maximum-row protection?

## Starting recommendation

Use the existing fixed binary position stream and decoder, keeping its vector
only at the public history-reader boundary.

## Resolution

The history-reader position path now shares the reviewed binary COPY decoder
and hard source-row ceiling. Only the explicit public history result retains a
position vector.
