# Owned synthetic Hub fixture

This directory provides a deterministic, private synthetic fixture for native
Hub interoperability checks. It does not collect real telemetry or use Tesla
credentials. Passing the fixture does not establish migration preservation,
packaged client behaviour, sync ingestion correctness, Linux execution, or the
full platform matrix.

The launcher needs the sibling `teslatlas-protocol` checkout because it imports
its native-evidence helper. Run Hub commands from the Hub root. Build the binary
and harness using the existing workspace runner, or the standalone command
below outside that workspace:


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
  "profile_id": "hub-http-v1@1.0.0",
  "profile_path": "/absolute/teslatlas-protocol/profiles/hub-http-v1/1.0.0",
  "profile_sha256": "SHA256 of the exact SHA256SUMS file bytes"
}
```

```sh
python3 tools/interop/fixture.py --config /private/config.json
```

The config binds the Hub binary, seed harness, fresh output directory, profile
ID and path, profile checksum, and optional loopback port. The launcher stages
and hashes both executables, creates a supported one-use pairing invitation,
starts the TLS server, and writes a private `ready.json` descriptor. Use that
descriptor for acceptance. Run this command from the sibling Protocol root:

```sh
./conformance/run --profile hub-http-v1@1.0.0 \
  --adapter /absolute/teslatlas-protocol/conformance/adapters/actual-hub \
  --config /private/new-fixture/ready.json --json
```

Or, from the Hub root, save a redacted smoke receipt:

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

Keep invitations, pairing URIs, bearer values, claim secrets, and opaque cursors
out of logs and diagnostics. The adapter checks staged-file hashes, executable
identity, process ownership, configuration binding, and fresh readiness before
issuing a successful receipt. Retain private evidence and stopped records for
review.
