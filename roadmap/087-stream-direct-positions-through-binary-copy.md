---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream direct positions through binary COPY

Blocked by: [Stream drives through binary COPY](086-stream-drives-through-binary-copy.md).

## Question

Can direct pack production consume position data from a reviewed typed binary
stream while retaining bounded rows, lifecycle joins, and exact projection
rules?

## Starting recommendation

Expose one fixed thirty-column position binary layout and decoder, then replace
the direct-position keyset loop with that stream under the existing row ceiling.

## Resolution

Direct pack production now reads positions through the reviewed thirty-column
binary COPY stream. It decodes and projects one row at a time under the same
hard limit, retaining existing drive relation, fragment, and skip behavior.
