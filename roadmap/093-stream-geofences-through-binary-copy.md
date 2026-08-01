---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream geofences through binary COPY

Blocked by: [Stream addresses through binary COPY](092-stream-addresses-through-binary-copy.md).

## Question

Can geofence facts use their reviewed binary projection without changing
selected-car filtering or required-name rejection?

## Starting recommendation

Use the fixed two-column projection and keep the existing typed name decoder.

## Resolution

Geofences now use the fixed two-column binary COPY stream. Selected-car
filtering, required names, and the hard source-row ceiling are unchanged.
