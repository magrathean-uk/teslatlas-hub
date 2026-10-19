# Owned synthetic Hub fixture

This directory provides a deterministic, private synthetic fixture for native
Hub interoperability checks. It does not collect real telemetry or use Tesla
credentials. Passing the fixture does not establish migration preservation,
packaged client behaviour, sync ingestion correctness, Linux execution, or the
full platform matrix.

The launcher imports the native-evidence helper from
`protocol/conformance/hub_native_evidence.py` in this repository. Run Hub
commands from the Hub root. Build the binary and harness using the existing
workspace runner, or the standalone command below outside that workspace:

```sh
cargo build --locked --bin teslatlas-hub --example interop_fixture --features interop-fixture
```

Create an owner-only (`0600`) JSON config in a private directory. Replace these
paths and the digest with your actual binaries and exported profile;
`output_dir` must not exist:

```json
{
  "binary": "/absolute/build/teslatlas-hub",
  "seed_binary": "/absolute/build/examples/interop_fixture",
  "output_dir": "/private/new-fixture",
  "lifetime_seconds": 3600,
  "profile_id": "hub-http-v1@1.2.0",
  "profile_path": "/absolute/hub/protocol/profiles/hub-http-v1/1.2.0",
  "profile_sha256": "SHA256 of the exact SHA256SUMS file bytes"
}
```

```sh
python3 tools/interop/fixture.py --config /private/config.json
```

The config binds the Hub binary, seed harness, fresh output directory, profile
ID and path, profile checksum, and optional loopback port. The launcher stages
and hashes both executables, creates a supported one-use pairing invitation,
starts the TLS server, and writes a private `ready.json` evidence record. The
`hub-http-v1@1.2.0` runner does not accept that file as its descriptor.

The `1.2.0` descriptor is documented in `protocol/docs/guides/conformance.md`.
This repository's `fixture.py` does not yet write it, so `fixture.py` and
`smoke.py` cannot pass unaided. Native acceptance uses an externally produced
lab descriptor until `fixture.py` is ported to write it. Run this command from
the `protocol/` directory of this repository:

```sh
./conformance/run --profile hub-http-v1@1.2.0 \
  --config /private/<run>/descriptor.json --timeout-seconds 600 --json
```

Or, from the Hub root, save the redacted `hub-http-v1@1.2.0` summary of a
complete native run:

```sh
python3 tools/interop/smoke.py \
  --descriptor /private/new-fixture/ready.json \
  --output /private/acceptance.json
```

Each complete run consumes the invitation and changes the scenario. Start a
fresh fixture for another complete run. The launcher stays attached until its
lifetime limit, SIGINT or SIGTERM, then stops only its own child and retains
evidence.

The descriptor's later-observation marker drives the synthetic scenario. The
launcher alone invokes the staged harness. Dynamic-entity checks are available
only with the `interop-fixture` feature and use the exact private fixture root:

```sh
/absolute/build/examples/interop_fixture --expose-dynamic /private/new-fixture
/absolute/build/examples/interop_fixture --retire-dynamic /private/new-fixture
/absolute/build/examples/interop_fixture --restore-dynamic /private/new-fixture
```

A device is granted only the vehicles that are active when its invitation is
claimed. A client that paired before `--expose-dynamic`, or while the dynamic
vehicle was retired, is never granted it and sees a 404 for it, even after
`--restore-dynamic`. Pair the client while the dynamic vehicle is exposed.

Keep invitations, pairing URIs, bearer values, claim secrets, and opaque cursors
out of logs and diagnostics. The `1.2.0` runner checks the descriptor's profile
binding, certificate and optional TLS pin, and `smoke.py` issues a receipt only
for a complete run in which every check passed. Binary identity and process
ownership come from the external lab descriptor writer until `fixture.py` is
ported. Retain private evidence and stopped records for review.
