# Release and supply-chain trust v1

Release trust starts from a reviewed immutable Hub source commit, exact
`Cargo.lock`, reviewed build toolchain/target, and the committed vendored
SQLite source. Normal builds and installs never download SQLite. A SQLite
refresh is a release-engineering action: it fetches only the official pinned
archive over HTTPS, verifies declared SHA3-256 archive/source digests,
regenerates bindings from those headers, and records the updater/toolchain
version and resulting vendor-tree digest for review.

Dependencies are resolved through the lockfile with locked builds; any lock,
patch, vendored tree, build script, feature, compiler, linker, or target change
creates new provenance. The release record includes a dependency inventory/SBOM,
source commit/tree, Rust/toolchain versions, target triple, build flags,
vendored SQLite version/digests, reproducibility environment, test/evidence
index, and artifact hashes. Build hosts retain raw logs as protected evidence
without credentials.

Each release contains native artifacts for every supported target, an
offline-verifiable canonical manifest naming each artifact, size, SHA-256,
source/provenance digests, and compatibility versions, plus a detached Minisign
signature. The Minisign public key is an independently distributed pinned trust
root; GitHub or any artifact host is storage only, never the authority for a
key, tag, release, or latest-version claim. Installers receive the public key
explicitly, verify manifest signature and artifact hash before installation,
and reject missing, substituted, extra, wrong-architecture, or unsigned files.

Release production requires reproducible or independently repeatable native
build evidence for every artifact. If bytes cannot be reproduced, a documented
diff must be limited to understood build metadata and both independent builds
must pass artifact/content, package, corpus, platform, and security gates. A
release cannot be promoted from a local test result or GitHub automation; CI is
not a release dependency.

Emergency revocation publishes a separately signed, versioned revocation record
under the pinned root, with affected artifact digest/version, reason class,
time, replacement, and minimum safe version. Install/upgrade checks that record
before accepting an artifact and fail closed when unavailable or invalid in a
security-sensitive update. Revocation never performs remote deletion, silently
rotates Tesla credentials, disables TeslaMate, or destroys Hub data; operators
receive a signed replacement/rollback instruction. Trust-root rotation needs a
cross-signed old/new record and an offline recovery path; compromise without a
valid predecessor requires explicit operator re-pinning.
