# Owned synthetic Hub fixture

Build the current native Hub and harness example in `hub/`:

```sh
cargo build --bin teslatlas-hub --example interop_fixture
```

Create a new owner-only (0600) JSON config inside a private directory. Paths must be absolute. `output_dir` must not exist. Example fields (replace paths and the digest with the locally exported profile):

```json
{
  "binary": "/absolute/hub/target/debug/teslatlas-hub",
  "seed_binary": "/absolute/hub/target/debug/examples/interop_fixture",
  "output_dir": "/private/new-fixture",
  "lifetime_seconds": 3600,
  "profile_id": "hub-http-v1@1.0.0",
  "profile_path": "/absolute/teslatlas-protocol/profiles/hub-http-v1/1.0.0",
  "profile_sha256": "SHA256 of the exact SHA256SUMS file bytes"
}
```

An optional `port` fixes the loopback port; otherwise the launcher reserves an available port during seeding. It copies both executables into unique private staging files, hashes those inputs before execution, launches the seed, creates a supported `pair --json` invitation using the staged production binary, and launches the real TLS server. Collection, terrain and geocoding are disabled; provider-shaped credentials are explicitly nonfunctional synthetic strings.

```sh
python3 tools/interop/fixture.py --config /private/config.json
```

Wait for its redacted ready message. The private `ready.json` descriptor contains endpoint, trusted certificate path, profile ID/path/hash, scenario path/hash, invitation file path, staged executable paths/hashes, child PID, launcher PID, both process start identities, the retained readiness path, and the owned update marker/receipt paths. `connection.json` is the seed-stage descriptor; consumers must use `ready.json` for bound native acceptance.

From `teslatlas-protocol/`, run:

```sh
./conformance/run --profile hub-http-v1@1.0.0 \
  --adapter /absolute/teslatlas-protocol/conformance/adapters/actual-hub \
  --config /private/new-fixture/ready.json --json
```

Alternatively, from `hub/`, save the redacted receipt using:

```sh
python3 tools/interop/smoke.py --descriptor /private/new-fixture/ready.json \
  --output /private/acceptance.json
```

One complete acceptance run consumes the invitation, rotates its bearer and applies the scenario's one later observation. Use a fresh fixture for another complete run. SDK-focused sessions may create their own invitations with the staged supported CLI, saving secret stdout directly to a 0600 file. Keep `pairingUri`, claim secrets, bearer values and opaque cursors out of diagnostics.

The later observation is requested by creating the private `advance.request` marker named in the descriptor. The launcher alone invokes the staged harness's `--advance` command. Its independent scenario checks reopen the store and compare the preserved charge before writing `advance.json`; no production HTTP mutation route is added.

SIGINT/SIGTERM or the lifetime limit stops only the launcher's owned Hub child. Evidence, including staged binaries and `stopped.json`, is retained. Passing this synthetic native run does not establish import/migration preservation, packaged client behavior, sync ingestion correctness, Linux execution or the full platform matrix.

Before reading the invitation, the acceptance adapter requires private retained
staged files with strict matching SHA256 digests and checks the OS-reported
native Hub executable, UID, direct parent, start identities and exact serve
command. It compares the descriptor with the launcher-owned ready record and
binds the listener/config/data directory to this fixture. Stopped or stale
fixtures fail. It repeats identity checks before claim and before issuing a
successful receipt. macOS uses libproc and Linux uses procfs for native
executable identity; both require `ps`. This is run provenance evidence, not
code signing or protection from another process deliberately acting as the
same OS owner.
