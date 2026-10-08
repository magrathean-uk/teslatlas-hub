# Developer bundle

The unpublished developer bundle is a deterministic source archive for Protocol
contracts and their local conformance tools. It is not a service, daemon, Hub
implementation, container image, release asset, or proof that another product
implements the contracts.

The bundle contains the rich schemas and OpenAPI, current-Hub, Hub-sync and Edge
profiles with their checksum maps, SSE contract, redacted examples and
fixtures, compatibility records, conformance cases and adapters, local tests,
the Apache 2.0 licence, third-party notices, top-level `docs/*.md`, the README,
`pyproject.toml`, and the exact `uv.lock`. Development-history receipts and
private runtime material are not included.

Manifest schema 1 retains its closed `contracts` keys. `hub_sync` selects the
bundle's `hub-sync-v1@1.3.0` conformance entry; both sync 1.3 and included 1.4
remain candidate profiles. Inclusion of 1.4 does not select it or prove consumer
adoption. Rich 1.2 is current with retained 1.0 and 1.1; current-Hub 1.1 and Edge
2.0 keep their separately named identities. Verification requires the included
1.4 member map, complete mapped content, independent checker and builder, and
the checker's shared sync/dependency inputs. A richer versioned machine inventory
of selected, retained and candidate roles is deferred.

## Build and verify

Python 3.11 or later is required. The examples keep the archive in a temporary
directory outside the checkout; use the same shell for the following commands.
Install `uv`, create the locked environment, run the gate, and build the
archive:

```sh
bundle_dir="$(mktemp -d)"
uv sync --locked --python 3.11
./tools/check
uv run --python 3.11 python tools/developer_bundle.py build --output "$bundle_dir"
uv run --python 3.11 python tools/developer_bundle.py verify \
  "$bundle_dir/teslatlas-protocol-2026.36.2.tar.gz"
```

Building twice from identical bytes produces an identical `.tar.gz` SHA-256.
The adjacent `.tar.gz.sha256` binds the archive. Inside it,
`BUNDLE-MANIFEST.json` closes membership and records every size and SHA-256;
`BUNDLE-SHA256SUMS` provides the same payload checksums in standard text form.
Archive verification requires exactly one complete canonical gzip member and a
byte-canonical tar stream with no raw, concatenated-member, or decompressed
post-termination bytes. It also closes the versioned root, manifest and contract
keys and values, file-entry order, paths, membership, regular-file type, exact
ordinary/executable modes, lock identity, and every file digest.

The local verifier bounds compressed input to 16 MiB, decoded tar bytes to
32 MiB, each regular member to 4 MiB and membership to 1024 files. It enforces
the input and expansion limits while reading, and member limits before reading
their contents. These utility limits do not change a bundled wire contract.

## Offline extraction

Extraction and bundle verification use only the Python standard library and do
not access the network:

```sh
python3.11 tools/developer_bundle.py extract \
  "$bundle_dir/teslatlas-protocol-2026.36.2.tar.gz" \
  --destination /tmp/teslatlas-protocol
python3.11 /tmp/teslatlas-protocol/teslatlas-protocol-2026.36.2/tools/developer_bundle.py \
  verify /tmp/teslatlas-protocol/teslatlas-protocol-2026.36.2
```

The archive carries the pinned dependency graph in `uv.lock`, not third-party
wheels or a platform-specific virtual environment. A cold-cache dependency
installation therefore needs package-index access. After an operator has
seeded an exact uv cache from this lock, the extracted tree can reinstall and
run without network access:

```sh
UV_OFFLINE=1 uv sync --locked --python 3.11
UV_OFFLINE=1 ./tools/check
```

Do not describe standard-library archive verification as a cold-cache offline
dependency install. ARM64 Docker proof, Hub catalog lifecycle, final-product
admission, and fresh real-input semantics require their separate F3/F5/F6/F7
receipts.

## Documentation scope

The archive includes the README and top-level `docs/*.md`. Repository
contribution, support, security, and agent guidance files are not included by
the current bundle builder. Links from the README to those root policies require
the repository checkout. The source bundle also excludes development-history
receipts; historical evidence references describe records kept with the
repository.
