---
type: wayfinder:task
status: closed
parent: 000-map
---
# Stream direct charge summary pass

Blocked by: [Decode charge samples from binary COPY](089-decode-charge-samples-from-binary-copy.md).

## Question

Can the first direct charge pass build process facts and sample counts from
typed binary rows under the existing hard source-row limit?

## Starting recommendation

Replace only the summary pass; preserve its facts/count map and leave sample
pack emission as the next independently testable stream.

## Resolution

Direct pack production now builds charge facts and sample counts from the
reviewed binary COPY stream under the existing hard source-row ceiling. Sample
pack emission remains a separate stream conversion.
