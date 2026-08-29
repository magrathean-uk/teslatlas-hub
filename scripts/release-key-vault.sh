#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu

vault_path=${TESLATLAS_RELEASE_VAULT_PATH:-"${HOME}/Library/Application Support/Teslatlas/Release Vault.sparsebundle"}
mount_path='/Volumes/Teslatlas Release Vault'
keychain_service='uk.magrathean.teslatlas-hub.release-vault'
keychain_account='György Bolyki'

usage() {
    echo "usage: $0 status|mount|paths|unmount" >&2
    exit 64
}

is_mounted() {
    /sbin/mount | /usr/bin/grep -F " on $mount_path (" >/dev/null 2>&1
}

case "${1:-}" in
    status)
        if is_mounted; then
            echo 'Teslatlas release vault: mounted'
        else
            echo 'Teslatlas release vault: locked'
        fi
        ;;
    mount)
        [ -d "$vault_path" ] && [ ! -L "$vault_path" ] || {
            echo "release vault is missing or unsafe: $vault_path" >&2
            exit 1
        }
        if is_mounted; then
            echo 'Teslatlas release vault: already mounted'
            exit 0
        fi
        vault_password=$(/usr/bin/security find-generic-password \
            -a "$keychain_account" -s "$keychain_service" -w)
        /usr/bin/printf '%s' "$vault_password" |
            /usr/sbin/diskutil image --stdinpass attach --nobrowse \
                --mountOptions owners "$vault_path" >/dev/null
        unset vault_password
        is_mounted || {
            echo 'release vault did not mount' >&2
            exit 1
        }
        echo 'Teslatlas release vault: mounted'
        ;;
    paths)
        is_mounted || {
            echo 'Teslatlas release vault is locked' >&2
            exit 1
        }
        printf '%s\n' \
            "PROVENANCE_SIGNING_KEY=$mount_path/private/teslatlas-hub-provenance-p256.pem" \
            "DEBIAN_ATTESTATION_SIGNING_KEY=$mount_path/private/teslatlas-hub-debian-attestation-ed25519.pem" \
            "PROVENANCE_PUBLIC_KEY=$mount_path/public/TeslatlasHubProvenancePublicKey.pem" \
            "DEBIAN_ATTESTATION_PUBLIC_KEY=$mount_path/public/TeslatlasHubDebianAttestationPublicKey.pem"
        ;;
    unmount)
        if ! is_mounted; then
            echo 'Teslatlas release vault: already locked'
            exit 0
        fi
        /usr/sbin/diskutil eject "$mount_path" >/dev/null
        if is_mounted; then
            echo 'release vault remained mounted' >&2
            exit 1
        fi
        echo 'Teslatlas release vault: locked'
        ;;
    *) usage ;;
esac
