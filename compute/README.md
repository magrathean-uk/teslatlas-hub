# Teslatlas Compute

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
- `generate_tile_payload_v1` computes from an in-memory drive slice.
- `generate_tile_payload_v1_from_pages` consumes each bounded page once and
  produces stable bytes independent of drive order or page boundaries. Every
  consumer call is limited to 128 drives and an 8 MiB encoded-page equivalent.
- Callers own storage, scheduling, publication, and cancellation-token lifetime.

The crate does not yet mean the App or Hub consumes it. Product integration and
ordinary-user acceptance remain separate Phase 4 work. The App still owns raw
position cleaning in `prepare_positions_for_tile_rendering`; callers must supply
an equivalently cleaned `(latitude, longitude)` sequence. Cross-product byte
parity remains an integration acceptance check.

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
