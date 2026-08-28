# Release keys

Official release tags and checksum manifests are signed with the Teslatlas Hub
release OpenPGP key.

```text
Fingerprint: F59A 16DD 7828 D75D 8DAB 23C5 2C4F D421 2815 2ACC
Identity:    Teslatlas Hub Release <release@teslatlas.local>
Algorithm:   Ed25519
```

The public key is stored in `RELEASE_SIGNING_KEY.asc` so verification remains
possible with the exact source. `release-evidence.py` copies it unchanged into
the flat release output, where `SHA256SUMS` covers it. Import the downloaded
copy and verify its full fingerprint explicitly:

```sh
RELEASE=/absolute/path/to/downloaded-v1.0.0-beta.1
EXPECTED_RELEASE_FINGERPRINT=F59A16DD7828D75D8DAB23C52C4FD42128152ACC
VERIFY_TMP=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-key-verify.XXXXXX")
trap 'find "$VERIFY_TMP" -depth -delete 2>/dev/null || true' EXIT HUP INT TERM
SOURCE_CHECKOUT="$VERIFY_TMP/source"
VERIFY_GNUPGHOME="$VERIFY_TMP/gnupg"
install -d -m 0700 "$VERIFY_GNUPGHOME"
git clone https://github.com/magrathean-uk/teslatlas-hub.git "$SOURCE_CHECKOUT"
git -C "$SOURCE_CHECKOUT" fetch --force --tags origin
git -C "$SOURCE_CHECKOUT" checkout --detach v1.0.0-beta.1
test -z "$(git -C "$SOURCE_CHECKOUT" status --porcelain=v1 --untracked-files=all)"
test "$(git -C "$SOURCE_CHECKOUT" cat-file -t v1.0.0-beta.1)" = tag
cd "$SOURCE_CHECKOUT"
gpg --homedir "$VERIFY_GNUPGHOME" --batch \
  --import "$RELEASE/RELEASE_SIGNING_KEY.asc"
gpg --homedir "$VERIFY_GNUPGHOME" --batch \
  --fingerprint "$EXPECTED_RELEASE_FINGERPRINT"
TAG_STATUS=$(GNUPGHOME="$VERIFY_GNUPGHOME" git verify-tag --raw v1.0.0-beta.1 2>&1)
TAG_SIGNER=$(
  printf '%s\n' "$TAG_STATUS" |
    awk '$1 == "[GNUPG:]" && $2 == "VALIDSIG" {print $3}'
)
test "$TAG_SIGNER" = "$EXPECTED_RELEASE_FINGERPRINT"
```

The copy in the same repository is not an independent trust anchor. Compare
the full fingerprint with the official release notes and a separately
authenticated Magrathean publication before trusting it.

**The fingerprint above has not yet been published through the required
separately authenticated, company-controlled channel. This blocks an official
v1.0.0-beta.1 release.** A Git repository, release asset, or key server alone
does not satisfy that independent publication requirement.

## Rotation

A replacement key must be announced in a signed release, identify its full
fingerprint and effective version, and preserve the retired public key for old
release verification. A lost or compromised key is never silently replaced.

Apple Developer ID and notarisation identities are separate platform trust
anchors. Their Team ID and signing authorities are recorded in each macOS
release evidence bundle.

## Debian native-attestation key

**No production Debian native-attestation private key or independently
published public-key digest currently exists. This blocks publication of
v1.0.0-beta.1.** This Ed25519 identity is separate from both the OpenPGP tag
key and the P-256 provenance key.

An authorised custodian must create the key outside the repository on an
encrypted volume. The native attestation generator accepts an unencrypted
Ed25519 PEM key owned by the invoking user and mode `0600` or stricter:

```sh
: "${DEBIAN_ATTESTATION_SIGNING_KEY:?absolute path outside the repository required}"
umask 077
openssl genpkey -algorithm ED25519 \
  -out "$DEBIAN_ATTESTATION_SIGNING_KEY"
chmod 0600 "$DEBIAN_ATTESTATION_SIGNING_KEY"
mkdir -p dist/release
openssl pkey -in "$DEBIAN_ATTESTATION_SIGNING_KEY" -pubout \
  -out dist/release/TeslatlasHubDebianAttestationPublicKey.pem
shasum -a 256 dist/release/TeslatlasHubDebianAttestationPublicKey.pem
```

The public key is published as the separately downloadable flat release asset
`TeslatlasHubDebianAttestationPublicKey.pem`. Before use, the authorised company
officer acting as release-key custodian must record its full 64-hex SHA-256
digest in the private release record and publish the same digest through a
separately authenticated, company-controlled channel outside GitHub. The public
key asset is not its own trust anchor. A second-person check is recommended
when another authorised maintainer exists, but is not falsely represented as a
control in this single-maintainer repository.

`release-evidence.py` reads the public key and its independently pinned digest,
verifies each native receipt, and stages byte-identical copies in the flat
release output and detailed evidence archive. The flat copy is covered by
`SHA256SUMS`; neither copy is an independent trust anchor. Verify both against
the independently published digest. Never put the private key in Git, a release
asset, the source archive, the Hub host, or the evidence output.

Rotation requires a statement naming the old and new full public-key digests,
effective version, date, and reason. Sign it with the OpenPGP release key and
retain the old public key for historical receipt verification. A lost or
suspected-compromised key breaks continuity and requires explicit disclosure.

## Provenance evidence key

**No production provenance private key or independently published provenance
trust anchor currently exists. This blocks publication of v1.0.0-beta.1.** The
OpenPGP tag key above is a separate identity and must not be reused as the
provenance key.

An authorised release-key custodian must create the production provenance key
on an encrypted offline volume. The current evidence tool accepts an
unencrypted PEM private key owned by the invoking user and mode `0600` or
stricter. It must be an ephemeral release-host copy, not the archival master.
With `PROVENANCE_SIGNING_KEY` set to that copy's absolute path, create a P-256
key and derive the exact public-key trust anchor as follows:

```sh
: "${PROVENANCE_SIGNING_KEY:?set an absolute private-key path on the encrypted volume}"
umask 077
openssl genpkey -algorithm EC \
  -pkeyopt ec_paramgen_curve:P-256 \
  -pkeyopt ec_param_enc:named_curve \
  -out "$PROVENANCE_SIGNING_KEY"
chmod 0600 "$PROVENANCE_SIGNING_KEY"
openssl pkey -in "$PROVENANCE_SIGNING_KEY" -pubout \
  -out provenance-public-key.pem
shasum -a 256 provenance-public-key.pem
```

Before the key signs any candidate, the authorised company officer acting as
release-key custodian must record the full 64-hex SHA-256 digest in the private
release record and publish the same digest through a separately authenticated,
company-controlled channel outside GitHub. The release bundle may contain
`provenance-public-key.pem`, but that copy is not the independent trust anchor.
Publication remains blocked until the private record and external publication
match. A second-person check is recommended when another authorised maintainer
exists, but is not claimed as a current control.

The private key must never enter Git, GitHub Actions, a release asset, the
source archive, the Hub host, or the artifact backup set. Keep the archival key
on the encrypted offline volume under named-custodian access. Mount or copy it
to the isolated release host only for the signing operation, keep the temporary
file owner-only, then remove that copy and revoke host access to the volume.

## Provenance key rotation

Planned rotation requires a transition statement containing the old and new
public-key digests, effective version, date, and reason. Sign the statement with
the old provenance key and the release OpenPGP key, publish the new digest
through the independent company channel before use, and retain the old public
key and digest for historical verification. If the old private key is lost or
suspected compromised, do not claim cryptographic continuity: disclose the
event, stop publication, establish the replacement through the independent
channel, and identify the first release using it.
