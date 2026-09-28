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
  produces stable bytes independent of drive order or page boundaries. Every
  consumer call is limited to 128 drives and an 8 MiB encoded-page equivalent.
- Callers own storage, scheduling, publication, and cancellation-token lifetime.

The Hub and App Rust core now depend on this crate for prepared and local map
computation. Product integration and ordinary-user acceptance still require
their own evidence. Callers own stable raw-row ordering by
`(date_ms, source_position_id)` and must clean each whole
eligible drive before splitting its output into bounded generator pages. A
fragment independently cleaned at a page boundary is not equivalent.
Cross-product byte parity remains an integration acceptance check.

## Determinism limits

Golden tests lock this crate's exact bytes on Rust 1.98.1 for representative
fixtures and are not an external App oracle. The
kernel uses standard-library `f64` trigonometric and logarithmic functions for
Web Mercator and distance calculations. Cross-platform libm differences may
affect a coordinate that rounds exactly at a pixel boundary; parity on each
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
