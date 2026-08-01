---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream drives through binary COPY

Blocked by: [Stream cars through binary COPY](085-stream-cars-through-binary-copy.md).

## Question

Can complete drive facts decode from the reviewed binary projection without
changing their lifecycle fields, numeric conversion, timestamp contract, or
row ceiling?

## Starting recommendation

Use the exact reviewed twenty-five-column drive type layout. Convert only the
already-projected fields and retain all existing range and timestamp failure
rules.

## Resolution

Complete drive facts now use PostgreSQL binary `COPY TO STDOUT` with the
reviewed twenty-five-column type layout. They retain the existing source-row
ceiling and timestamp, decimal, and nullable-relation failure behavior.
