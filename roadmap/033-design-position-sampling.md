---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design position sampling

Blocked by: [Design drive lifecycle parity](032-design-drive-lifecycle.md).

## Question

Which positions are retained, deduplicated, rejected, interpolated, or attached
to drives, and how are precision and sampling gaps represented?

## Starting recommendation

Retain source facts losslessly, reject impossible coordinates explicitly, and
make any downsampling a separate reversible projection.

## Resolution

Retain every bounded source position payload in the immutable journal. Attach a
projected position only to a validated open drive and only when finite WGS84
latitude is within `[-90, 90]` and longitude within `[-180, 180]`. Invalid
coordinates reject projection while preserving source evidence. Ordered
observation identity, not coordinate equality, controls deduplication, so an
exact repeat remains receipt evidence.

Hub never interpolates, snaps, or invents a path. Consecutive accepted samples
define path/distance and their timestamps make gaps explicit. Full precision
stays in source and typed projection; privacy/downsample views are separate,
versioned, rebuildable outputs.
