---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream charging processes through binary COPY

Blocked by: [Stream direct positions through binary COPY](087-stream-direct-positions-through-binary-copy.md).

## Question

Can charge-session facts use the reviewed binary projection while preserving
nullable relation, decimal, timestamp, and lifecycle behavior?

## Starting recommendation

Decode the exact reviewed eighteen-column process layout under the existing
row ceiling before the larger charge-sample stream is moved.

## Resolution

Charge-session facts now use the reviewed eighteen-column binary COPY stream.
The existing row ceiling and every nullable relation, decimal, timestamp, and
lifecycle projection rule remain enforced.
