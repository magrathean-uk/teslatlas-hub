# Source distribution

The repository provides a deterministic source-only handoff for the four
public SwiftPM libraries. It is not a release archive, tag, binary package, or
runtime acceptance artifact.

Run the complete temporary exercise from the repository root:

```sh
python3 tools/source_handoff.py exercise
```

The command stages a package below a private temporary directory using the
required basename `teslatlas-sdk-swift`, verifies every staged path, byte size,
and SHA-256 digest, asks SwiftPM to validate the manifest, and builds a separate
source-only consumer that imports these products:

- `TeslatlasHubSDK`
- `TeslatlasCommands`
- `TeslatlasHubV1Compatibility`
- `TeslatlasCurrentHub`

The temporary package, consumer, and build scratch directory are removed when
the command exits, including after a failed check. No executable is run.

To retain a handoff for another reviewer, choose a new absent output path:

```sh
python3 tools/source_handoff.py prepare --output /private/review/swift-handoff
python3 tools/source_handoff.py verify --handoff /private/review/swift-handoff
python3 tools/source_handoff.py smoke --handoff /private/review/swift-handoff
```

`prepare` refuses to merge with or replace an existing path. The handoff
contains `source-handoff.json`, the canonical `teslatlas-sdk-swift` package
root, and a sibling `external-four-library-consumer`. The manifest is
timestamp-free and path-independent. Its `source_package.identity_sha256`
binds the ordered relative paths, byte sizes, and file hashes. The validator
rejects duplicate JSON keys, schema drift, missing or extra files and
directories, symlinks, special filesystem entries, escaped paths, wrong root
names, checksum changes, and a consumer that does not declare all four
products. Source selection applies the same symlink and repository-containment
rules before copying any input.

Filesystem metadata is also deterministic. Every staged directory has mode
`0755`, every regular file has mode `0644`, and every modification timestamp is
`2000-01-01T00:00:00Z` (`946684800` Unix seconds). These values are independent
of the caller's umask and are enforced again during verification. SwiftPM may
create local metadata while parsing or building; `smoke` removes only those
known generated paths, restores the fixed metadata, and performs a final exact
readback.

The selected source payload contains the Swift manifest, version, licence,
README, all package sources and tests, both source examples, and public
documentation. Local build output, private inputs, development receipts,
archived plans, Git metadata, IDE state, and platform harness projects are not
source-package inputs.

Successful `smoke` output proves that SwiftPM can parse the staged source and
compile an external consumer of all four libraries on the recorded host. It
does not prove macOS 14, iOS 17, Linux ARM64, live Hub behavior, installation,
upgrade, rollback, removal, real-data semantics, or final F3/F6/F7 acceptance.
