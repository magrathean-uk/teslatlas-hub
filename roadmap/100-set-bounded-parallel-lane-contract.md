---
type: wayfinder:task
status: closed
parent: 000-map
---
# Set bounded parallel lane contract

Blocked by: [Prove native snapshot lane copy](099-prove-native-snapshot-lane-copy.md).

## Question

What bounded source-copy lane range protects TeslaMate while permitting useful
parallel typed COPY on supported hosts?

## Starting recommendation

Default to four lanes, permit one through eight only, and reject all other
values before a source connection is opened.

## Resolution

Hub now defaults to four parallel COPY lanes and rejects values below one or
above eight during import configuration validation, before source use.
