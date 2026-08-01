---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream addresses through binary COPY

Blocked by: [Stream direct charge sample packing](091-stream-direct-charge-sample-packing.md).

## Question

Can address facts use the reviewed binary projection while preserving the
existing selected-car relation filter and hard row ceiling?

## Starting recommendation

Use the fixed three-column address layout with the existing fixed query and
optional local presentation fields.

## Resolution

Addresses now use the fixed three-column binary COPY stream. Their selected-car
relation filter, optional presentation fields, and hard source-row ceiling are
unchanged.
