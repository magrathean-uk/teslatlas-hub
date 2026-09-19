# Current-Hub HTTP profile

This guide covers the public HTTP surface implemented by a current Teslatlas
Hub. Run the shell blocks in one session, in order, with the private directory
and validated endpoint produced by the earlier blocks. The wire contract is
`hub-http-v1@1.0.0` in
[`profiles/hub-http-v1/1.0.0/`](../profiles/hub-http-v1/1.0.0/). Copy the whole
directory when distributing the contract: its OpenAPI 3.1.0 document resolves
schemas from the adjacent profile files.

Run the command examples from the Protocol repository root. They use the
checked-in `conformance.hub_http` validator and write all response material to
the private directory created for the session.

The profile is source-neutral and does not require Hub source or an SDK. It is
separate from the richer semantic profiles (`1.0.0`, `1.1.0`, and
`1.2.0`) and from the Edge delivery profile. It has no protocol-version
request header, SSE stream, vehicle command route, metadata CRUD, or public
charge-query route.

The profile is admitted for the bounded product `2026.36.2` compatibility
record. G3 r2 binds the exact profile manifest digest to the accepted G4, G5,
and G6 source-built synthetic runtime receipts. This admission does not claim
an installed service, release artifact, real Tesla data, or a complete platform
matrix. See [`docs/compatibility.md`](compatibility.md) and
[`docs/verification.md`](verification.md) for the exact evidence boundary.

## Choose the profile

Use this profile when discovery returns `protocol: "teslatlas-sync"`,
`protocol_major: 1`, and `api_versions: ["1.0"]`. A Hub advertises either
the reduced capability set
`["query.vehicles", "query.current"]` or the full set
`["query.vehicles", "query.current", "query.drives", "sync.packs"]`.
Require `query.drives` before attempting the drives route. `sync.packs` is a
delivery capability; it does not add a query endpoint.

The `version` field is the Hub product version (for example `2026.36.2`),
not a wire revision. The profile identity remains `hub-http-v1@1.0.0`.

## Routes

| Method and path | Authentication | Successful response | Other documented responses |
| --- | --- | --- | --- |
| `GET /.well-known/teslatlas-hub` | none | discovery JSON (`200`) | `503` JSON error |
| `GET /healthz` | none | health JSON (`200`) | — |
| `GET /readyz` | none | `{"status":"ready"}` (`200`) | `503` readiness JSON |
| `POST /v1/pairings/{pairing_id}/claim` | none | claim JSON (`200`) | `400`, `401`, `404`, `415`, `422`, `503` |
| `GET /v1/vehicles` | bearer | vehicle list (`200`) | `401`, `503` |
| `GET /v1/vehicles/{vehicle_id}/current` | bearer | current observation (`200`) | `401`, `404`, `503` |
| `GET /v1/vehicles/{vehicle_id}/drives` | bearer | drive page (`200`) | `304`, `400`, `401`, `404`, `503` |
| `POST /v1/device/rotate` | bearer | replacement claim (`200`) | `401`, `404`, `503` |

The machine-readable schemas and `openapi.json` define the exact fields and
status handling. The examples in the profile are deliberately redacted and
must not be used as credentials.

## Discover and pair a client safely

Discovery is unauthenticated and has no rich-profile version header. Obtain the
Hub origin from the owner or deployment configuration; do not copy an endpoint
from another Hub. Save the response privately and validate it against the
profile before choosing capabilities:

~~~sh
set -eu
umask 077
private_dir="$(mktemp -d /tmp/teslatlas-current-hub.XXXXXX)"
chmod 700 "$private_dir"
export private_dir
hub_endpoint="$HUB_ENDPOINT"
test -n "$TESLATLAS_HUB_CA"
HUB_ENDPOINT="$hub_endpoint" python3 - <<'PY'
import os
from urllib.parse import urlsplit

url = urlsplit(os.environ["HUB_ENDPOINT"])
if (url.scheme != "https" or url.username or url.password or url.query
        or url.fragment or url.path not in ("", "/")):
    raise SystemExit("Hub endpoint must be a bare HTTPS origin")
PY
curl --silent --show-error --config /dev/null \
  --noproxy '*' --cacert "$TESLATLAS_HUB_CA" --connect-timeout 5 --max-time 30 \
  --max-filesize 1048576 \
  --dump-header "$private_dir/discovery.headers" \
  --output "$private_dir/discovery.json" \
  "$hub_endpoint/.well-known/teslatlas-hub"
TESLATLAS_DISCOVERY="$private_dir/discovery.json" \
TESLATLAS_DISCOVERY_HEADERS="$private_dir/discovery.headers" \
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.0.0" \
python3 - <<'PY'
import json
import os
from pathlib import Path
from jsonschema import Draft202012Validator, FormatChecker

profile = Path(os.environ["TESLATLAS_PROFILE"])
header_lines = Path(os.environ["TESLATLAS_DISCOVERY_HEADERS"]).read_text(encoding="iso-8859-1").splitlines()
if not header_lines or len(header_lines[0].split()) < 2 or int(header_lines[0].split()[1]) != 200:
    raise SystemExit("discovery request did not return HTTP 200")
headers = {}
for line in header_lines[1:]:
    if ":" in line:
        key, item = line.split(":", 1)
        headers[key.strip().lower()] = item.strip()
if headers.get("content-type", "").split(";", 1)[0].strip() != "application/json":
    raise SystemExit("discovery response has an invalid content type")
raw = Path(os.environ["TESLATLAS_DISCOVERY"]).read_bytes()
if len(raw) > 1048576:
    raise SystemExit("discovery response exceeds the profile byte limit")
value = json.loads(raw)
schema = json.loads((profile / "discovery.schema.json").read_bytes())
errors = list(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(value))
if errors:
    raise SystemExit("discovery does not match hub-http-v1@1.0.0")
if "query.vehicles" not in value["capabilities"] or "query.current" not in value["capabilities"]:
    raise SystemExit("Hub does not advertise the required query capabilities")
PY
~~~

If the client does not use `jsonschema`, apply the same JSON Schema 2020-12
validation with its chosen library before trusting discovery. A Hub may
advertise the reduced capability set; do not request drives unless
`query.drives` is present.

Pairing is an owner operation. Ask the Hub owner to run the configured CLI and
write its JSON result to a private file:

~~~sh
set -eu
umask 077
if [ -z "${private_dir:-}" ]; then
  private_dir="$(mktemp -d /tmp/teslatlas-current-hub.XXXXXX)"
  chmod 700 "$private_dir"
fi
teslatlas-hub --config "$PRIVATE_CONFIG" pair --json >"$private_dir/invitation.json"
chmod 600 "$private_dir/invitation.json"
~~~

`PRIVATE_CONFIG` is a path to the owner's private Hub configuration. Keep
the invitation file private: it contains `pairingId`, `secret`, `expiresAtMs`,
`endpoint`, `tlsPin`, and a `pairingUri` that repeats the secret. Confirm
that the invitation has not expired, that `pairingUri` matches all of its
fields, and that its endpoint is the endpoint you intend to trust. Do not paste
the JSON into a shell command, log it, or put any of its values in process
arguments.

`tlsPin` is the lowercase SHA-256 digest of the connected leaf certificate's
DER bytes. It is not a public-key pin. `curl --pinnedpubkey` therefore cannot
implement this rule. A client must establish normal CA and hostname validation,
inspect the leaf certificate on the connection that will carry the claim, and
compare that DER digest before sending the secret. Never use `-k`.

Here is a complete HTTPS example using Python's standard-library transport and
the profile's JSON Schema dependency. It takes only private file paths and a
nonsecret device-name environment variable as inputs. The claim response is
written to a mode-600 file and is not printed. `TESLATLAS_HUB_CA` must name a
CA file supplied by the owner; the endpoint hostname must be covered by that
certificate.

~~~sh
TESLATLAS_HUB_CA="$private_dir/ca.pem" \
TESLATLAS_INVITATION="$private_dir/invitation.json" \
TESLATLAS_CLAIM_RESPONSE="$private_dir/claim.json" \
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.0.0" \
TESLATLAS_DEVICE_NAME="My current-Hub client" \
python3 - <<'PY'
import hashlib
import json
import os
import ssl
import time
from http.client import HTTPSConnection
from pathlib import Path
from urllib.parse import parse_qs, quote, urlsplit
from jsonschema import Draft202012Validator, FormatChecker
from conformance import hub_http


def fail(message):
    raise SystemExit(message)


invitation_bytes = Path(os.environ["TESLATLAS_INVITATION"]).read_bytes()
try:
    invitation = hub_http.strict_json(invitation_bytes)
except (UnicodeError, ValueError):
    fail("invitation is not strict JSON")
if hub_http.validate_raw(
    os.environ["TESLATLAS_PROFILE"], "invitation", 200,
    {"content-type": "application/json"}, invitation_bytes,
):
    fail("invitation does not match auth.schema.json")
required = {"pairingId", "secret", "expiresAtMs", "endpoint", "tlsPin", "pairingUri"}
if set(invitation) != required:
    fail("invitation fields do not match the current-Hub profile")
if invitation["expiresAtMs"] <= int(time.time() * 1000):
    fail("invitation has expired")
if len(invitation["secret"]) != 64 or len(invitation["tlsPin"]) != 64:
    fail("invitation secret or TLS pin has the wrong shape")

endpoint = urlsplit(invitation["endpoint"])
if (endpoint.scheme != "https" or endpoint.username or endpoint.password
        or endpoint.query or endpoint.fragment or endpoint.path not in ("", "/")):
    fail("invitation endpoint is not a bare HTTPS origin")
uri = urlsplit(invitation["pairingUri"])
expected = {
    "endpoint": [invitation["endpoint"]],
    "pairing_id": [invitation["pairingId"]],
    "secret": [invitation["secret"]],
    "tls_pin": [invitation["tlsPin"]],
}
if (uri.scheme != "teslatlas-hub" or uri.netloc != "pair" or uri.path
        or uri.fragment or parse_qs(uri.query, strict_parsing=True) != expected):
    fail("pairingUri does not match the invitation fields")

context = ssl.create_default_context(cafile=os.environ["TESLATLAS_HUB_CA"])
connection = HTTPSConnection(endpoint.hostname, endpoint.port or 443, context=context, timeout=15)
connection.connect()
leaf_der = connection.sock.getpeercert(binary_form=True)
if hashlib.sha256(leaf_der).hexdigest() != invitation["tlsPin"]:
    connection.close()
    fail("connected TLS leaf does not match tlsPin")

claim = json.dumps(
    {"secret": invitation["secret"], "device_name": os.environ["TESLATLAS_DEVICE_NAME"]},
    ensure_ascii=False,
    separators=(",", ":"),
).encode("utf-8")
if len(claim) > 4096:
    connection.close()
    fail("claim JSON exceeds the 4096-byte profile limit")
path = "/v1/pairings/" + quote(invitation["pairingId"], safe="") + "/claim"
connection.request("POST", path, body=claim, headers={
    "Accept": "application/json",
    "Content-Type": "application/json",
})
response = connection.getresponse()
body = response.read(1048577)
status = response.status
content_type = response.getheader("Content-Type", "")
connection.close()
if status != 200 or len(body) > 1048576:
    fail("claim failed; inspect the private response status without retrying")
if content_type.split(";", 1)[0].strip() != "application/json":
    fail("claim response has an invalid content type")
try:
    value = hub_http.strict_json(body)
except (UnicodeDecodeError, json.JSONDecodeError, ValueError):
    fail("claim response is not valid JSON")
schema = json.loads((Path(os.environ["TESLATLAS_PROFILE"]) / "auth.schema.json").read_bytes())
if list(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(value)):
    fail("claim response does not match auth.schema.json")
result_path = Path(os.environ["TESLATLAS_CLAIM_RESPONSE"])
result_path.write_bytes(body)
result_path.chmod(0o600)
PY
~~~

The invitation is single-use. Do not automatically retry a claim after a
timeout or lost response: the Hub may already have consumed it. Obtain a new
invitation through the owner flow. A successful claim returns
`device_id`, `access_token`, and `expires_at_ms`; write that object to a
mode-600 file and treat the bearer as a secret.

The serialized UTF-8 claim body MUST be at most 4,096 bytes. This is the
profile's client bound; the exact status and body a Hub emits for an
over-limit request still require runtime observation. Do not rely on an
unobserved `413` representation or retry an over-limit claim with the same
invitation.

## Read resources

Create an authorization header file without putting the bearer in shell
arguments or history:

~~~sh
set -eu
umask 077
TESLATLAS_CLAIM_RESPONSE="$private_dir/claim.json" \
TESLATLAS_AUTH_HEADER="$private_dir/authorization.header" \
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.0.0" \
python3 - <<'PY'
import os
from pathlib import Path
from conformance import hub_http

claim_bytes = Path(os.environ["TESLATLAS_CLAIM_RESPONSE"]).read_bytes()
claim = hub_http.strict_json(claim_bytes)
if hub_http.validate_raw(
    os.environ["TESLATLAS_PROFILE"], "claim", 200,
    {"content-type": "application/json"}, claim_bytes,
):
    raise SystemExit("claim response does not match auth.schema.json")
token = claim["access_token"]
if not isinstance(token, str) or len(token) != 64:
    raise SystemExit("claim response has an invalid bearer")
path = Path(os.environ["TESLATLAS_AUTH_HEADER"])
path.write_text("Authorization: Bearer " + token + "\n", encoding="ascii")
path.chmod(0o600)
PY

validate_resource() {
  RESOURCE_BODY="$1" RESOURCE_HEADERS="$2" RESOURCE_KIND="$3" \
  TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.0.0" python3 - <<'PY'
import os
from pathlib import Path
from conformance import hub_http

body_path = Path(os.environ["RESOURCE_BODY"])
header_lines = Path(os.environ["RESOURCE_HEADERS"]).read_text(encoding="iso-8859-1").splitlines()
if not header_lines or len(header_lines[0].split()) < 2:
    raise SystemExit("resource response headers are invalid")
status = int(header_lines[0].split()[1])
headers = {}
for line in header_lines[1:]:
    if ":" in line:
        key, value = line.split(":", 1)
        headers[key.strip()] = value.strip()
if status != 200 or hub_http.validate_raw(
    os.environ["TESLATLAS_PROFILE"], os.environ["RESOURCE_KIND"], status,
    headers, body_path.read_bytes()
):
    raise SystemExit("resource response does not match hub-http-v1@1.0.0")
PY
}

endpoint="$(TESLATLAS_INVITATION="$private_dir/invitation.json" python3 - <<'PY'
import os
from pathlib import Path
from conformance import hub_http
print(hub_http.strict_json(Path(os.environ["TESLATLAS_INVITATION"]).read_bytes())["endpoint"].rstrip("/"))
PY
)"
curl --silent --show-error --get \
  --config /dev/null --noproxy '*' --cacert "$TESLATLAS_HUB_CA" \
  --connect-timeout 5 --max-time 30 --max-filesize 1048576 \
  --header @"$private_dir/authorization.header" \
  --dump-header "$private_dir/vehicles.headers" \
  --output "$private_dir/vehicles.json" \
  "$endpoint/v1/vehicles"
validate_resource "$private_dir/vehicles.json" "$private_dir/vehicles.headers" vehicles
~~~

Use the `vehicle_id` from the validated vehicle list to request current state.
Quote the path, and keep the response private when it may contain identifying
data:

~~~sh
set -eu
umask 077
vehicle_id='11111111-1111-4111-8111-111111111111'
curl --silent --show-error \
  --config /dev/null --noproxy '*' --cacert "$TESLATLAS_HUB_CA" \
  --connect-timeout 5 --max-time 30 --max-filesize 1048576 \
  --header @"$private_dir/authorization.header" \
  --dump-header "$private_dir/current.headers" \
  --output "$private_dir/current.json" \
  "$endpoint/v1/vehicles/$vehicle_id/current"
validate_resource "$private_dir/current.json" "$private_dir/current.headers" current
~~~

`/healthz` reports process health and the product `version`. `/readyz` reports
whether the Hub can serve the published surface; a ready Hub does not promise
that a vehicle has a recent observation. A `current` response can contain
`null` for unavailable or unknown values. Preserve `null` and numeric zero as
different values, and use `observed_at_ms` when deciding whether an
observation is fresh.

## Iterate drives

Drive filters are `from_ms` inclusive and `to_ms` exclusive. The default range
is zero through signed-64-bit maximum; `limit` is 1–500 and defaults to 100.
Use `--get --data-urlencode` for every query value and return the opaque
`next_cursor` unchanged. Encode it once as a query value, keep the exact
vehicle and time filters, reject a repeated cursor, and stop only when
`next_cursor` is `null`:

~~~sh
set -eu
umask 077
from_ms=0
to_ms=9223372036854775807
max_pages=100
cursor_file="$private_dir/drives.cursor"
seen_cursors="$private_dir/drives.seen-cursors.json"
: >"$cursor_file"
chmod 600 "$cursor_file"
printf '[]' >"$seen_cursors"
chmod 600 "$seen_cursors"
page=0
while [ "$page" -lt "$max_pages" ]; do
  page=$((page + 1))
  output="$private_dir/drives-$page.json"
  headers="$private_dir/drives-$page.headers"
  if [ -s "$cursor_file" ]; then
    curl --silent --show-error --get \
      --config /dev/null --noproxy '*' --cacert "$TESLATLAS_HUB_CA" \
      --connect-timeout 5 --max-time 30 --max-filesize 1048576 \
      --header @"$private_dir/authorization.header" \
      --data-urlencode "from_ms=$from_ms" \
      --data-urlencode "to_ms=$to_ms" \
      --data-urlencode 'limit=100' \
      --data-urlencode "cursor@$cursor_file" \
      --dump-header "$headers" \
      --output "$output" \
      "$endpoint/v1/vehicles/$vehicle_id/drives"
  else
    curl --silent --show-error --get \
      --config /dev/null --noproxy '*' --cacert "$TESLATLAS_HUB_CA" \
      --connect-timeout 5 --max-time 30 --max-filesize 1048576 \
      --header @"$private_dir/authorization.header" \
      --data-urlencode "from_ms=$from_ms" \
      --data-urlencode "to_ms=$to_ms" \
      --data-urlencode 'limit=100' \
      --dump-header "$headers" \
      --output "$output" \
      "$endpoint/v1/vehicles/$vehicle_id/drives"
  fi
  DRIVE_PAGE="$output" DRIVE_HEADERS="$headers" CURSOR_FILE="$cursor_file" \
  SEEN_CURSORS="$seen_cursors" \
  NEXT_CURSOR_FILE="$private_dir/next.cursor" \
  TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.0.0" python3 - <<'PY'
import json
import os
from pathlib import Path
from conformance import hub_http

body_path = Path(os.environ["DRIVE_PAGE"])
header_lines = Path(os.environ["DRIVE_HEADERS"]).read_text(encoding="iso-8859-1").splitlines()
if not header_lines or len(header_lines[0].split()) < 2:
    raise SystemExit("drive response headers are invalid")
status = int(header_lines[0].split()[1])
headers = {}
for line in header_lines[1:]:
    if ":" in line:
        key, value = line.split(":", 1)
        headers[key.strip()] = value.strip()
problems = hub_http.validate_raw(
    os.environ["TESLATLAS_PROFILE"], "drives", status, headers, body_path.read_bytes()
)
if problems:
    raise SystemExit("drive response does not match hub-http-v1@1.0.0")
value = hub_http.strict_json(body_path.read_bytes())
next_cursor = value["next_cursor"]
next_path = Path(os.environ["NEXT_CURSOR_FILE"])
if next_cursor is None:
    next_path.write_text("", encoding="ascii")
else:
    seen_path = Path(os.environ["SEEN_CURSORS"])
    seen = json.loads(seen_path.read_bytes())
    if next_cursor in seen:
        raise SystemExit("repeated cursor")
    seen.append(next_cursor)
    seen_path.write_text(json.dumps(seen, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
    next_path.write_text(next_cursor, encoding="utf-8")
PY
  mv "$private_dir/next.cursor" "$cursor_file"
  [ ! -s "$cursor_file" ] && break
done
[ ! -s "$cursor_file" ] || { echo 'drive page limit reached before next_cursor became null' >&2; exit 1; }
ETAG_SOURCE="$private_dir/drives-1.headers" \
ETAG_HEADER="$private_dir/drives.if-none-match" python3 - <<'PY'
import os
from pathlib import Path

source = Path(os.environ["ETAG_SOURCE"])
destination = Path(os.environ["ETAG_HEADER"])
for line in source.read_text(encoding="iso-8859-1").splitlines():
    if line.lower().startswith("etag:"):
        value = line.split(":", 1)[1].strip()
        if not value or any(char in value for char in "\r\n"):
            raise SystemExit("first drive page has an invalid ETag")
        try:
            value.encode("ascii")
        except UnicodeEncodeError:
            raise SystemExit("first drive page has a non-ASCII ETag")
        destination.write_text("If-None-Match: " + value + "\n", encoding="ascii")
        destination.chmod(0o600)
        break
else:
    raise SystemExit("first drive page has no ETag")
PY
curl --silent --show-error --get \
  --config /dev/null --noproxy '*' --cacert "$TESLATLAS_HUB_CA" \
  --connect-timeout 5 --max-time 30 --max-filesize 1048576 \
  --header @"$private_dir/authorization.header" \
  --header @"$private_dir/drives.if-none-match" \
  --data-urlencode "from_ms=$from_ms" \
  --data-urlencode "to_ms=$to_ms" \
  --data-urlencode 'limit=100' \
  --dump-header "$private_dir/drives-conditional.headers" \
  --output "$private_dir/drives-conditional.json" \
  "$endpoint/v1/vehicles/$vehicle_id/drives"
DRIVE_PAGE="$private_dir/drives-conditional.json" \
DRIVE_HEADERS="$private_dir/drives-conditional.headers" \
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.0.0" python3 - <<'PY'
import os
from pathlib import Path
from conformance import hub_http

body = Path(os.environ["DRIVE_PAGE"]).read_bytes()
lines = Path(os.environ["DRIVE_HEADERS"]).read_text(encoding="iso-8859-1").splitlines()
if not lines or len(lines[0].split()) < 2:
    raise SystemExit("conditional drive response headers are invalid")
status = int(lines[0].split()[1])
headers = {}
for line in lines[1:]:
    if ":" in line:
        key, value = line.split(":", 1)
        headers[key.strip()] = value.strip()
if status != 304 or hub_http.validate_raw(
    os.environ["TESLATLAS_PROFILE"], "drives", status, headers, body
):
    raise SystemExit("conditional drive response does not match the 304 profile")
PY
~~~

Drive `200` and `304` responses carry an ETag and
`Cache-Control: no-store`. You may compare a retained ETag for a conditional
request, but `no-store` means the response is not a durable HTTP cache entry.
A matching `304` has no body. Invalid limits, ranges, filters, or cursors are
`400` errors; an unknown vehicle is `404`; an invalid or expired bearer is
`401`. Retry only a read that is safe to retry and only with a bounded policy
after a `503`. The files in this example are private, bounded, temporary
captures for validation. `--max-filesize` is an early client transfer bound;
the profile validator remains the authoritative 1 MiB ceiling, including for
responses without a declared length. Delete captures after comparison and do
not place them in a reusable HTTP cache.

Signed-64-bit JSON integers include drive IDs and timestamps. A JavaScript
client must decode them losslessly or reject values outside
`[-9007199254740991, 9007199254740991]`; silently rounding an ID can make
pagination or reconciliation incorrect.

## Rotate credentials

Rotation is authenticated and returns a replacement claim object. Keep the
new response private before replacing the old header file. The old bearer is
invalid immediately after a successful rotation; a lost rotation response may
therefore have already invalidated it. Do not retry blindly or promise a
rollback. Re-enter the owner pairing flow if recovery is needed:

~~~sh
set -eu
umask 077
curl --silent --show-error --request POST \
  --config /dev/null --noproxy '*' --cacert "$TESLATLAS_HUB_CA" \
  --connect-timeout 5 --max-time 30 --max-filesize 1048576 \
  --header @"$private_dir/authorization.header" \
  --dump-header "$private_dir/rotated-claim.headers" \
  --output "$private_dir/rotated-claim.json" \
  "$endpoint/v1/device/rotate"

cp "$private_dir/authorization.header" "$private_dir/authorization.old"
chmod 600 "$private_dir/authorization.old"
ROTATED_CLAIM="$private_dir/rotated-claim.json" \
ROTATED_HEADERS="$private_dir/rotated-claim.headers" \
AUTH_HEADER="$private_dir/authorization.header" \
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.0.0" python3 - <<'PY'
import os
from pathlib import Path
from conformance import hub_http

body_path = Path(os.environ["ROTATED_CLAIM"])
header_lines = Path(os.environ["ROTATED_HEADERS"]).read_text(encoding="iso-8859-1").splitlines()
if not header_lines or len(header_lines[0].split()) < 2:
    raise SystemExit("rotation response headers are invalid")
status = int(header_lines[0].split()[1])
headers = {}
for line in header_lines[1:]:
    if ":" in line:
        key, value = line.split(":", 1)
        headers[key.strip()] = value.strip()
body = body_path.read_bytes()
if status != 200 or hub_http.validate_raw(
    os.environ["TESLATLAS_PROFILE"], "rotate", status, headers, body
):
    raise SystemExit("rotation response does not match auth.schema.json")
value = hub_http.strict_json(body)
token = value["access_token"]
if not isinstance(token, str) or len(token) != 64:
    raise SystemExit("rotation response has an invalid bearer")
destination = Path(os.environ["AUTH_HEADER"])
temporary = destination.with_name(destination.name + ".new")
if temporary.exists():
    raise SystemExit("temporary authorization file already exists")
fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
try:
    with os.fdopen(fd, "w", encoding="ascii") as stream:
        stream.write("Authorization: Bearer " + token + "\n")
        stream.flush()
        os.fsync(stream.fileno())
finally:
    if temporary.exists() and temporary.stat().st_size == 0:
        temporary.unlink()
os.replace(temporary, destination)
PY

curl --silent --show-error --get \
  --config /dev/null --noproxy '*' --cacert "$TESLATLAS_HUB_CA" \
  --connect-timeout 5 --max-time 30 --max-filesize 1048576 \
  --header @"$private_dir/authorization.old" \
  --dump-header "$private_dir/old-bearer.headers" \
  --output "$private_dir/old-bearer.json" \
  "$endpoint/v1/vehicles"
old_status="$(awk 'NR == 1 { print $2; exit }' "$private_dir/old-bearer.headers")"
[ "$old_status" = 401 ] || { echo 'old bearer was not rejected after rotation' >&2; exit 1; }
rm -f "$private_dir/authorization.old"
~~~

Validate the replacement response before replacing the header. A non-200
response is an error and must leave the old header untouched; on `200`, run the
same strict `auth.schema.json` validation used for claim responses, write the
new header to a mode-600 temporary path, then atomically rename it over the old
header. Verify that the old bearer no longer works. Owner revocation and
re-pairing are lifecycle operations outside this HTTP profile; there is no
public revoke route.

## Errors and unsupported surfaces

Branch on the documented HTTP status and the profile's stable JSON error code,
never on implementation-specific text. Claim extractor failures (`400`,
`415`, and `422`) are bounded `text/plain`; they are not the JSON error
envelope. A `401` claim means the secret is wrong, expired, or already used.
Do not retry it. A `503` may indicate an unavailable service, but a claim that
timed out still requires a fresh owner invitation before another attempt.

The current-Hub profile intentionally has no public SSE/events endpoint,
command submission, metadata CRUD, snapshot revision, or charge-query route.
Do not send rich-profile headers such as `Teslatlas-Protocol-Version` and do
not infer unsupported routes from discovery. The sync appendix documents pack
delivery checks separately from these HTTP query semantics.

For the full route schemas, examples, and conformance vectors, use the profile
directory and [`docs/conformance.md`](conformance.md). The native acceptance
adapter requires a private fixture descriptor and a same-host owned Hub; a
reference adapter or a successful schema check is not installed-product
acceptance.
