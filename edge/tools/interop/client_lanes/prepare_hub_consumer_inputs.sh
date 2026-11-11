#!/bin/sh
set -eu

usage() {
    echo "usage: $0 --server-ca-source PATH --server-certificate PATH --server-ip ADDRESS --client-ca PATH --client-certificate PATH --client-key PATH --bearer PATH --output-dir PATH" >&2
    exit 64
}

fail() {
    echo "Hub consumer input handoff rejected: $1" >&2
    exit 65
}

server_ca_source=
server_certificate=
server_ip=
client_ca=
client_certificate=
client_key=
bearer=
output_dir=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --server-ca-source) [ "$#" -ge 2 ] || usage; server_ca_source=$2; shift 2 ;;
        --server-certificate) [ "$#" -ge 2 ] || usage; server_certificate=$2; shift 2 ;;
        --server-ip) [ "$#" -ge 2 ] || usage; server_ip=$2; shift 2 ;;
        --client-ca) [ "$#" -ge 2 ] || usage; client_ca=$2; shift 2 ;;
        --client-certificate) [ "$#" -ge 2 ] || usage; client_certificate=$2; shift 2 ;;
        --client-key) [ "$#" -ge 2 ] || usage; client_key=$2; shift 2 ;;
        --bearer) [ "$#" -ge 2 ] || usage; bearer=$2; shift 2 ;;
        --output-dir) [ "$#" -ge 2 ] || usage; output_dir=$2; shift 2 ;;
        *) usage ;;
    esac
done

[ -n "$server_ca_source" ] && [ -n "$server_certificate" ] \
    && [ -n "$server_ip" ] \
    && [ -n "$client_ca" ] && [ -n "$client_certificate" ] \
    && [ -n "$client_key" ] && [ -n "$bearer" ] && [ -n "$output_dir" ] || usage

absolute_path() {
    case "$1" in
        /*) ;;
        *) fail "$2 path must be absolute" ;;
    esac
    case "$1" in
        *[![:print:]]*) fail "$2 path contains non-printable characters" ;;
    esac
}

regular_file() {
    path=$1
    label=$2
    absolute_path "$path" "$label"
    [ -f "$path" ] && [ ! -L "$path" ] || fail "$label must be a non-symlink regular file"
}

for input in \
    "$server_ca_source" \
    "$server_certificate" \
    "$client_ca" \
    "$client_certificate" \
    "$client_key" \
    "$bearer"; do
    regular_file "$input" "input"
done

[ "$server_ca_source" != "$client_ca" ] || fail "server and client CA inputs must be distinct"
[ "$server_certificate" != "$client_certificate" ] || fail "server and client certificates must be distinct"
command -v openssl >/dev/null 2>&1 || fail "openssl is required"
command -v install >/dev/null 2>&1 || fail "install is required"
command -v cmp >/dev/null 2>&1 || fail "cmp is required"

validate_ca() {
    path=$1
    label=$2
    text_file=$3
    openssl x509 -in "$path" -noout -text > "$text_file" 2>/dev/null \
        || fail "$label is not a readable X.509 certificate"
    openssl x509 -in "$path" -noout -text 2>/dev/null \
        | grep -A1 'Basic Constraints' \
        | grep -q 'CA:TRUE' \
        || fail "$label must be CA:TRUE"
}

validate_server_ip() {
    ip=$1
    case "$ip" in
        ''|*[!0-9.]*) fail "server IP must be a canonical IPv4 address" ;;
    esac
    saved_ifs=$IFS
    IFS=.
    set -- $ip
    IFS=$saved_ifs
    [ "$#" -eq 4 ] || fail "server IP must be a canonical IPv4 address"
    for octet in "$@"; do
        case "$octet" in
            0|[1-9]|[1-9][0-9]|[1-9][0-9][0-9]) ;;
            *) fail "server IP must be a canonical IPv4 address" ;;
        esac
        [ "$octet" -le 255 ] || fail "server IP must be a canonical IPv4 address"
    done
    [ "$ip" != "0.0.0.0" ] || fail "server IP must not be a wildcard address"
}

stage_parent=$(dirname -- "$output_dir")
[ -d "$stage_parent" ] && [ ! -L "$stage_parent" ] || fail "output parent is not a directory"
[ ! -e "$output_dir" ] && [ ! -L "$output_dir" ] || fail "output directory already exists"

stage=
created_output_dir=0
cleanup() {
    if [ -n "$stage" ] && [ -d "$stage" ]; then
        rm -rf "$stage"
    fi
    if [ "$created_output_dir" -eq 1 ] && [ -d "$output_dir" ]; then
        rmdir "$output_dir" 2>/dev/null || true
    fi
}
trap cleanup EXIT HUP INT TERM

umask 077
validate_server_ip "$server_ip"
mkdir -m 700 -- "$output_dir"
created_output_dir=1
stage=$(mktemp -d "$output_dir/.staging.XXXXXX")

validate_ca "$server_ca_source" "server CA source" "$stage/server-ca.txt"
validate_ca "$client_ca" "client CA" "$stage/client-ca.txt"
openssl verify -CAfile "$server_ca_source" -purpose sslserver \
    "$server_certificate" >/dev/null 2>&1 \
    || fail "server certificate does not verify for sslserver"
openssl x509 -in "$server_certificate" -noout -text > "$stage/server-certificate.txt" 2>/dev/null \
    || fail "server certificate is not a readable X.509 certificate"
awk -v expected_ip="$server_ip" '
    /X509v3 Subject Alternative Name:/ { in_san=1; next }
    in_san && /^[[:space:]]*X509v3 / { exit }
    in_san {
        line = $0
        while ((position = index(line, "IP Address:")) > 0) {
            value = substr(line, position + length("IP Address:"))
            sub(/[,[:space:]].*$/, "", value)
            ip_count++
            if (value == expected_ip) {
                expected_count++
            } else {
                invalid_ip=1
            }
            line = substr(line, position + length("IP Address:"))
        }
    }
    END { exit(ip_count == 1 && expected_count == 1 && invalid_ip != 1 ? 0 : 1) }
' "$stage/server-certificate.txt" \
    || fail "server certificate must contain IP SAN $server_ip"
openssl verify -CAfile "$client_ca" -purpose sslclient "$client_certificate" \
    >/dev/null 2>&1 \
    || fail "client certificate does not verify for sslclient"
openssl x509 -in "$client_certificate" -pubkey -noout > "$stage/client-cert.pub" 2>/dev/null \
    || fail "client certificate public key is unreadable"
openssl pkey -in "$client_key" -pubout > "$stage/client-key.pub" 2>/dev/null \
    || fail "client private key is unreadable"
cmp -s "$stage/client-cert.pub" "$stage/client-key.pub" \
    || fail "client certificate and private key do not match"

bearer_bytes=$(wc -c < "$bearer" | tr -d '[:space:]')
[ "$bearer_bytes" -ge 16 ] && [ "$bearer_bytes" -le 4096 ] \
    || fail "bearer must be between 16 and 4096 bytes"
invalid_bearer_bytes=$(LC_ALL=C tr -d 'A-Za-z0-9._-' < "$bearer" | wc -c | tr -d '[:space:]')
[ "$invalid_bearer_bytes" -eq 0 ] \
    || fail "bearer must be a single visible owner-only token"
LC_ALL=C grep -Eq \
    '^tte1\.[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\.[A-Za-z0-9_-]{43}$' \
    "$bearer" \
    || fail "bearer must use the Edge-issued tte1 credential format"

install -m 0600 "$server_ca_source" "$stage/server-ca.pem"
install -m 0600 "$client_certificate" "$stage/hub-client.crt"
install -m 0600 "$client_key" "$stage/hub-client.key"
install -m 0600 "$bearer" "$stage/delivery-bearer"
for name in server-ca.pem hub-client.crt hub-client.key delivery-bearer; do
    [ ! -e "$output_dir/$name" ] && [ ! -L "$output_dir/$name" ] || fail "output path already exists"
done
mv -- "$stage/server-ca.pem" "$output_dir/server-ca.pem"
mv -- "$stage/hub-client.crt" "$output_dir/hub-client.crt"
mv -- "$stage/hub-client.key" "$output_dir/hub-client.key"
mv -- "$stage/delivery-bearer" "$output_dir/delivery-bearer"
rm -rf -- "$stage"
stage=
created_output_dir=0
printf '%s\n' 'Hub consumer inputs prepared from an issuing CA and separate client-auth inputs'
