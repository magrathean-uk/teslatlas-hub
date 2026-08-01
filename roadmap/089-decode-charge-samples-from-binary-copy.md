---
type: wayfinder:task
status: closed
parent: 000-map
---
# Decode charge samples from binary COPY

Blocked by: [Stream charging processes through binary COPY](088-stream-charging-processes-through-binary-copy.md).

## Question

Can the reviewed twenty-two-column charge projection decode from binary COPY
with the exact existing energy, current, voltage, and nullable field contract?

## Starting recommendation

Add the fixed column layout and binary decoder first. Direct two-pass pack
folding follows as a separate bounded behavior.

## Resolution

Charge samples now have the fixed reviewed twenty-two-column binary COPY
decoder with identical nullable and numeric conversion rules. Direct pack
folding remains the next bounded step.
