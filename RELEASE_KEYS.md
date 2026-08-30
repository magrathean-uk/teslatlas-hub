# Release keys

Official release tags and checksum manifests are signed with the Teslatlas Hub
release OpenPGP key.

```text
Fingerprint: A43B 517A 25C5 9994 6546 39ED 9CB5 BEA1 F3D6 5EDD
Identity:    György Bolyki <contact@magrathean.uk>
GitHub UID:  György Bolyki <88115882+magrathean-uk@users.noreply.github.com>
Algorithm:   Ed25519
Created:     2026-08-29
Expires:     2028-08-28
Custodian:   György Bolyki
```

The public key is stored in `RELEASE_SIGNING_KEY.asc` so verification remains
possible with the exact source. `release-evidence.py` copies it unchanged into
the flat release output, where `SHA256SUMS` covers it. Import the downloaded
copy and verify its full fingerprint explicitly:

```sh
RELEASE=/absolute/path/to/downloaded-v1.0.0-beta.1
EXPECTED_RELEASE_FINGERPRINT=A43B517A25C59994654639ED9CB5BEA1F3D65EDD
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
the full fingerprint with the company-controlled publication at
<https://teslatlas.eu/hub/release-keys/v1.0.0-beta.1.txt> before trusting it. A
Git repository, release asset, or key server alone does not satisfy that
independent publication requirement.

The earlier pre-publication candidate key
`F59A16DD7828D75D8DAB23C52C4FD42128152ACC` was retired on 2026-08-29 before
it authenticated an official release. It remains recoverable from Git history
for audit purposes; no release-signing continuity claim is made from that key.

## Rotation

A replacement key must be announced in a signed release, identify its full
fingerprint and effective version, and preserve the retired public key for old
release verification. A lost or compromised key is never silently replaced.

Apple Developer ID and notarisation identities are separate platform trust
anchors. Their Team ID and signing authorities are recorded in each macOS
release evidence bundle.

## Debian native-attestation key

The production Ed25519 key was provisioned on 2026-08-29 under the custody of
György Bolyki. Its public-key SHA-256 is
`7186087343ae93f3d9c5d02347f467a45937339118db1a5f043cb1f6d4e15fe7`.
That digest is recorded in the private release-key record and independently
published at
<https://teslatlas.eu/hub/release-keys/v1.0.0-beta.1.txt>. The identity is
separate from both the OpenPGP tag key and the P-256 provenance key.

The archival key is held outside the repository in a dedicated AES-256
encrypted APFS release vault that remains unmounted outside authorised release
work. The native attestation generator accepts an unencrypted Ed25519 PEM key
owned by the invoking user and mode `0600` or stricter only while that vault is
mounted. Use the following process for an authorised rotation:

On the authorised macOS release host, use `scripts/release-key-vault.sh status`,
`mount`, `paths`, and `unmount` to operate the vault. The helper obtains the
vault password from macOS Keychain, never prints it, refuses unsafe vault paths,
and does not force an unmount. At-rest protection comes from the vault's own
AES-256 encryption; FileVault is not a release prerequisite.

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

The production P-256 key was provisioned on 2026-08-29 under the custody of
György Bolyki. Its public-key SHA-256 is
`a787a55c4b93266453d86805a6cda1ba5b54c76ce31750a468c1dc76a7c18901`.
That digest is recorded in the private release-key record and independently
published at
<https://teslatlas.eu/hub/release-keys/v1.0.0-beta.1.txt>. The OpenPGP tag key
above is a separate identity and must not be reused as the provenance key.

The archival key is held outside the repository in the dedicated AES-256
encrypted APFS release vault and remains unmounted outside authorised release
work. The current evidence tool accepts an unencrypted PEM private key owned by
the invoking user and mode `0600` or stricter only while that vault is mounted.
Use the following process for an authorised rotation and derive the exact
public-key trust anchor as follows:

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
inside the unmounted encrypted release vault under named-custodian access.
Mount it only for the signing operation, keep any temporary release-host copy
owner-only, then remove that copy and unmount the vault.

## Provenance key rotation

Planned rotation requires a transition statement containing the old and new
public-key digests, effective version, date, and reason. Sign the statement with
the old provenance key and the release OpenPGP key, publish the new digest
through the independent company channel before use, and retain the old public
key and digest for historical verification. If the old private key is lost or
suspected compromised, do not claim cryptographic continuity: disclose the
event, stop publication, establish the replacement through the independent
channel, and identify the first release using it.
