<p align="center">
  <img src="https://raw.githubusercontent.com/magrathean-uk/magrathean-uk/main/assets/icons/teslatlas.png" width="96" height="96" alt="">
</p>

<h1 align="center">Teslatlas Compute</h1>

<p align="center">The dependency-free shared Rust kernel for deterministic Teslatlas map computation.</p>

<p align="center">
  <a href="LICENSE">Licence</a>
</p>

## Overview

`teslatlas-compute` is the dependency-free shared Rust kernel for deterministic
Teslatlas map computation. Its first public API converts typed drives or bounded
canonical position-page v1 bytes into canonical tile-publication v1 bytes.

The initial implementation is extracted from the Teslatlas App CPU fallback at
App commit `569ac74867947616632ecb64bb65fb97ab565cb5`. It preserves the App's
z2-z13 LOD parameters, Web Mercator projection, discontinuity handling,
antimeridian split, tile crossing, weighted 200,000-segment tile budget, signed
segment ordering, and little-endian payload formats.

The exported tile geometry identity is `raster-v9-rounded-tile-px`, matching
the prepared-artefact contract in Teslatlas Protocol commit
`4b5d3c1ee71ae5edb734416baeb836dba1c485f4`.

`ALGORITHM_VERSION` is the explicit semantic identity `0.1.1`, independent of
the Cargo package version `0.1.0`. It distinguishes corrected cleaner/generator
results from earlier `0.1.0` results while retaining geometry v9 and the v1 wire
formats. Callers must check exact current algorithm identity and full artifact
bindings before cached producer reuse or current receipt/pack delivery; a valid
historical signature alone does not establish current eligibility. Regenerate
selected stale results from authoritative history rather than relabeling old
bytes. Prepared maps remain optional when regeneration fails. The RDP allowance
bounds new computation, not reuse of a verified current result.

## API boundary

- `parse_position_page_v1` validates and decodes one bounded input page.
- `prepare_positions_for_tile_rendering_v1` converts a drive's ordered raw
  `(date_ms, latitude, longitude, speed_kmh)` positions to cleaned f32
  coordinates using the App's validity, bridgeable-outlier and stationary-drift
  rules. It never smooths or interpolates. A drive over 65,536 raw positions
  returns `CleanMapError::OversizedDrive`: no Hub prepared map should be
  published for that span, and the App retains its existing local route path.
- `generate_tile_payload_v1` computes from an in-memory drive slice.
- `generate_tile_payload_v1_from_pages` consumes each bounded page once and
  produces stable bytes when the same complete `Drive` objects are reordered
  or regrouped between pages. Every consumer call is limited to 128 drives and
  an 8 MiB encoded-page equivalent. Each object must fit one page; an oversized
  object is rejected rather than continued on another page.
- `generate_tile_payload_v1_with_rdp_limit` and
  `generate_tile_payload_v1_from_pages_with_rdp_limit` add a caller-selected
  finite `u64` limit on RDP interior-point distance evaluations. One budget
  spans every tier, drive and page in a call. Exactly the selected number of
  evaluations is allowed; the next returns `Error::InvalidData` without
  partial output. Zero permits no RDP evaluations. Successful geometry and
  bytes use the same path as the existing generators. This limits RDP work,
  not total CPU time, guard rescans, rastering or packing. Existing generator
  APIs have no RDP evaluation ceiling.
- Callers own storage, scheduling, publication, and cancellation-token lifetime.

The Hub and App Rust core now depend on this crate for prepared and local map
computation. Product integration and ordinary-user acceptance still require
their own evidence. Callers own stable raw-row ordering by
`(date_ms, source_position_id)` and must clean each whole
eligible drive before grouping complete `Drive` objects into bounded generator
pages. Neither independently cleaning raw fragments nor splitting a cleaned
drive's points into separate objects is equivalent. `drive_id` is metadata,
not a continuation key: separate objects with the same ID are counted and
simplified separately, and their endpoints are not reconnected.
Cross-product byte parity remains an integration acceptance check.

Invalid raw rows are removed by the cleaner; they do not survive as route-gap
markers. Surviving coordinates may connect under the generator's segment rules.
Invalid coordinates supplied directly to a generator instead end a valid run
before simplification. Changing raw-gap semantics requires a separate consumer
contract and parity decision.

Generators accumulate repeated tile segments directly into bounded unique
weights. The first segment for a new tile charges a shared 4,096-tile publication
limit across all tiers; a call fails during admission of tile 4,097, before
packing or processing further input. Coordinates retain signed offsets outside a tile's `0..512` interior
when representable as i16. Any unrepresentable endpoint fails the whole call
with `Error::InvalidData`; it is not wrapped, clipped or subdivided. Mixed exact
`-180`/`+180` meridian aliases retain vertical motion on the starting alias's
edge, consistently with that alias used for both endpoints. A page consumer's
first error is retained even if a storage visitor ignores it; retries return
that error and cannot publish a partial result.

## Determinism limits

Golden tests lock this crate's exact bytes on Rust 1.98.1 for representative
fixtures and are not an external App oracle. The
kernel uses standard-library `f64` trigonometric and logarithmic functions for
Web Mercator and distance calculations. Cross-platform libm differences may
affect cleaner speed, acceleration and drift decisions, RDP and guard membership,
the renderability gate, or coordinates near pixel-rounding boundaries. These
threshold effects can change retained points and `ReadyEmpty`/`ReadyData`
classification before raster rounding. No cross-platform divergence has been
demonstrated by the same-host tests; raw-cleaner-to-payload parity on each
shipping architecture must therefore be checked before product adoption.

## Development

From the workspace root:

```sh
scripts/dev/with-heavy-build-lock.sh \
  scripts/dev/run.sh teslatlas-compute cargo test --all-targets
```

Build output is routed outside the checkout by clean-development.

## Licence

Teslatlas Compute is licensed under the Apache License 2.0. See
[LICENSE](LICENSE) and [NOTICE](NOTICE).

<sub>© 2026 MAGRATHEAN UK LTD · [Legal](https://github.com/magrathean-uk/.github/blob/main/LEGAL.md)</sub>
