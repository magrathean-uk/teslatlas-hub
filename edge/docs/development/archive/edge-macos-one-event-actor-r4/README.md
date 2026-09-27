# Edge macOS one-event actor r4

This directory is a stable, source-only review package that supersedes the
r3 contract after Hub rejected r3 as not independently reviewable. It contains
fresh review candidates for the producer helper and the one-shot guard.

The historical helper source under `/private/tmp/edge-fleet-src.current.x9MQxE`
is gone. The `producer/main.go` and `producer/main_test.go` files here are
therefore new stable source candidates, not byte-identical claims about the
historical `synthetic-vehicle-darwin-arm64` binary. The historical binary is
provenance only and must not be copied or invoked.

The producer candidate accepts all cohort-specific values as arguments,
constructs one protobuf `VehicleName` datum in one topic-`V` FlatBuffers
envelope, performs one WebSocket write, reads one binary ACK, and has no retry
or replay path. Hub must stage it into the pinned Fleet Telemetry source tree,
build it for the fresh future cohort, and independently bind the resulting
hash before any runtime decision.

The guard candidate verifies exact authorization, binding, helper and
owner-only TLS input paths; reads one Unix-millisecond timestamp only after
those checks; claims an `O_CREAT|O_EXCL` guard; spawns the helper once without a
shell; and records a final result. Existing guards, timeouts, nonzero exits and
ACK failures are final. The wrapper does not create credentials, start a
runtime, open a network connection itself, or submit an event.

Run the focused local-only guard tests with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover \
  -s guard -p 'test_*.py' -v
```

The Go files are source-review inputs. Their imports are the pinned upstream
Fleet Telemetry `messages` and `protos` packages; Hub owns the future source
overlay/module build and any live receiver test. `gofmt -d` is the local syntax
and format check for this source-only package.

See the sibling r4 JSON proposal for exact file hashes, modes, supersession
evidence, fresh placeholders, guard order, and the no-runtime boundary.
