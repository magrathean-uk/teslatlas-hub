# Hub release compliance gate

Release is blocked unless all items pass.

## Licence/source

- `LICENSE` is verbatim GNU AGPL v3.
- package, CLI, GUI and docs use `AGPL-3.0-only`.
- section 7 notices apply only to authorised material.
- upstream modifications carry source/revision/date notices.
- object code has exact Corresponding Source.
- remote network interaction has a prominent source offer.
- build/install/interface material is included.
- no proprietary secret is required to build covered code.

## Ownership/provenance

- an authorised Company maintainer has attested that private founder,
  employment, contractor, assignment and contributor records are current;
- historical commits without DCO trailers have been reviewed under
  `HISTORICAL_CONTRIBUTIONS.md` without rewriting published history;
- every file has a resolved provenance class;
- similarity scan inputs/results are retained;
- empty scans are not treated as clearance;
- `UNKNOWN` material is removed or resolved;
- public/proprietary movement is approved.

## Dependencies/security

- lockfile final;
- SBOM/notices generated;
- vulnerabilities adjudicated;
- backup/restore and rollback tested;
- no debug/fake endpoint enabled;
- artefacts signed and checksummed.

## Publication

Publish all of the following together:

- macOS app ZIP and the byte-identical separately downloadable service-only
  `TeslatlasHubService.pkg` embedded by that app;
- native Debian 13 amd64 and ARM64 packages;
- exact tagged workspace source archive, paired on the release page with the
  detailed archive below as one complete Corresponding Source offer;
- one deterministic detailed evidence archive containing
  `rust-vendored-sources.tar.gz`, `rust-source-inventory.json`,
  `rust-source-evidence-manifest.json`, complete Go and Fleet evidence
  (including `tesla-http-proxy-go-sources.tar.gz` and
  `fleet-telemetry-upstream-source.tar.gz` plus
  `fleet-telemetry-go-module-sources.tar.gz` with all 45 locked module source
  ZIPs and `go.mod` files), the exact dependency legal bundle, SBOMs,
  inventories, notices, and both native Debian receipts/signatures;
- flat, independently checksummed release asset
  `TeslatlasHubDebianAttestationPublicKey.pem`, whose SHA-256 is independently
  published and whose bytes match the copy inside the detailed evidence archive;
- flat, independently checksummed `RELEASE_SIGNING_KEY.asc`, whose full
  fingerprint is authenticated outside the release;
- provenance and notarisation receipts inside the detailed evidence archive;
- top-level `SHA256SUMS` and detached `SHA256SUMS.asc`;
- migration notes, release notes, and legal changelog.

The full OpenPGP release fingerprint and both production public-key SHA-256
digests must be published through a separately authenticated,
company-controlled channel. Repository, release-asset, and key-server copies
are not independent trust anchors. The current missing external publication
blocks v1.0.0-beta.1.

Immediately before the signed tag, commit the final status flip that removes
candidate, draft, preparation, and unreleased wording from this version's
public status paragraphs, sets the actual release date, and changes the
release-note title to the final beta title. The tag must point to that commit; a
post-tag status/date edit or a tag that still identifies itself as only a
candidate fails this gate.

The finalization review includes `README.md`, `CHANGELOG.md`,
`LEGAL_CHANGELOG.md`, `RELEASE_KEYS.md`, `SOURCE_AVAILABILITY.md`,
`PRIVACY.md`, `SECURITY.md`, `RELEASE_VERIFICATION.md`, this file,
`docs/README.md`, both installation guides,
`docs/FLEET_SETUP.md`, `docs/RELEASING.md`, and the beta release notes. Remove
version-specific key, trust-anchor, external-publication, native-execution, or
other blocker statements only after their underlying evidence exists; leaving a
resolved v1.0.0-beta.1 blocker statement in the signed tag also fails this gate.

The publication directory is flat. It contains `Teslatlas Hub.zip`,
`TeslatlasHubService.pkg`, both architecture-named Debian packages,
`RELEASE_SIGNING_KEY.asc`, the Debian attestation public key, the exact tagged
source tarball, one detailed evidence tarball, `SHA256SUMS`, and
`SHA256SUMS.asc`. Intermediate evidence trees are not separate release assets.

The dependency legal bundle is platform-invariant. It excludes the
architecture-bound `go-component-manifest.json` and
`fleet-telemetry-component-manifest.json`; those remain in their complete
sidecar evidence directories. The Fleet Go source/legal corpus is also
platform-invariant; neither fact is a native Linux reproducibility claim.
Debian native proof must bind tag-locked Linux sidecar bytes admitted by
`packaging/linux/sidecar-sha256.lock` to signed, architecture-specific native
attestations; each package must embed those architecture values as
`SIDECAR_SHA256SUMS`.

The detailed evidence must contain verified `linux-amd64` and `linux-arm64`
Go-proxy and Fleet-receiver evidence whose manifest subjects exactly match the
sidecars in each Debian package. Go v2 generation on the locked Apple-silicon
macOS host performs the target-specific clean rebuild from its locked 20-module,
21-source corpus; `--verify-dir` validates the resulting record and evidence but
does not rebuild it. Fleet evidence binds its subject and source/legal corpus
without asserting a clean receiver rebuild. Darwin evidence must
never be presented as Linux-native build evidence. The separate signed native
attestation remains mandatory proof that the package and its sidecars executed
on Debian 13 for the named architecture.

The Fleet notice embedded in platform packages points to
`fleet-telemetry-go-module-sources.tar.gz`. That exact source archive must be in
the detailed evidence release asset even though it is intentionally not copied
into the platform-invariant dependency legal bundle.

Both `git verify-tag --raw` and detached-checksum verification must produce one
`VALIDSIG` whose fingerprint exactly matches the full approved signed-tag
OpenPGP fingerprint. Merely accepting any valid signature or a short key ID
fails this gate. Platform-specific version encodings must map deterministically
from the exact Hub SemVer; for beta.1, Apple uses `1.0.0b1` while Hub remains
`1.0.0-beta.1`.

## Human attestations

Automation cannot prove private chain of title, contributor instruments,
trade-mark clearance, authority to use third-party services, or acceptance of
current platform terms. The authorised release signer must record those factual
decisions before publication. Missing evidence blocks an official release; a
generated document must never be treated as the attestation itself.
