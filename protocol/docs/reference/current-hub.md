# Current-Hub HTTP profile

This guide covers the public HTTP surface implemented by a current Teslatlas
Hub. Run the shell blocks in one session, in order, with the private directory
and validated endpoint produced by the earlier blocks. The wire contract is
`hub-http-v1@1.1.0` in
[`profiles/hub-http-v1/1.1.0/`](../../profiles/hub-http-v1/1.1.0/). Copy the whole
directory when distributing the contract: its OpenAPI 3.1.0 document resolves
schemas from the adjacent profile files.

Run the command examples from the Protocol repository root. They use the
checked-in `conformance.hub_http` validator and write all response material to
the private directory created for the session.

The profile is source-neutral and does not require Hub source or an SDK. It is
separate from the richer semantic profiles (`1.0.0`, `1.1.0`, and
`1.2.0`) and from the Edge delivery profile. It has no protocol-version
request header, SSE stream, vehicle command route, or metadata CRUD. It has no
public charge-query route in `hub-http-v1@1.1.0`; `hub-http-v1@1.2.0` adds
charge, charge-sample and position pages (see below).

`hub-http-v1@1.1.0` is a candidate successor to the frozen
`hub-http-v1@1.0.0` profile. It defines the paired-bearer grace and schema 2.2
no-op-unavailable behavior in this guide. G3 r2 admits only the exact frozen
`1.0.0` manifest and its G4, G5, and G6 synthetic runtime receipts; it does not
admit this distinct `1.1.0` manifest. Fresh Hub evidence is required before
claiming runtime acceptance for `1.1.0`. See
[`docs/compatibility.md`](compatibility.md) and
[`docs/verification.md`](verification.md) for the accepted `1.0.0` boundary.

## hub-http-v1@1.2.0

`hub-http-v1@1.2.0` is the successor of `1.1.0` (`previous_profile`; the `1.1.0`
manifest is not pinned) and has profile status `served`. Use it when discovery returns
`"profile": "hub-http-v1@1.2.0"`; a Hub whose discovery has no `profile` field serves
`1.1.0`. Its contract is the directory
[`profiles/hub-http-v1/1.2.0/`](../../profiles/hub-http-v1/1.2.0/): `profile.json`
(routes, limits, rules), `openapi.json`, the schemas, `cases.json`, `examples/` and
`SHA256SUMS`. Copy the whole directory. Everything below this section describes
`1.1.0` and stays true for the routes `1.2.0` keeps; this section lists what
`1.2.0` changes. Generate clients with [`generators/README.md`](../../generators/README.md).

The additive policy: clients ignore unknown object fields and unknown capability
tokens; servers may add optional fields and tokens within 1.x; removing or retyping
a field, changing an enum value, or changing a route's status set is a new major
profile. Response objects in the schemas are therefore open. Conformance holds a
Hub to a stricter form that forbids undocumented fields.

**Discovery.** `api_versions` stays `["1.0"]`, `protocol` stays `teslatlas-sync` and
`protocol_major` stays `1`; `query.vehicles` and `query.current` stay advertised, so
Home Assistant keeps working unchanged. Discovery gains `profile`
(`hub-http-v1@1.2.0`) and, when enrichment is enabled, `attributions` (at most 8 of
`{id, text, licence, url}`). Capability tokens match `^[a-z0-9.]{1,64}$`, at most 16,
unique. A token is advertised only when its routes are served:

| Token | Advertised when |
| --- | --- |
| `query.vehicles`, `query.current` | always |
| `query.drives`, `query.charges`, `query.positions`, `query.charge.samples`, `sync.packs` | the Hub has a cursor key |
| `auth.revoke` | paired-device authentication is on |
| `query.enrichment` | address or terrain enrichment is enabled |

**New routes.**

| Method and path | Responses | Limit default / max |
| --- | --- | --- |
| `GET /v1/vehicles/{vehicle_id}/drives/{drive_id}` | `200`, `401`, `404`, `503` | |
| `GET /v1/vehicles/{vehicle_id}/drives/{drive_id}/positions` | `200`, `304`, `400`, `401`, `404`, `503` | 1000 / 2048 |
| `GET /v1/vehicles/{vehicle_id}/charges` | `200`, `304`, `400`, `401`, `404`, `503` | 100 / 500 |
| `GET /v1/vehicles/{vehicle_id}/charges/{charge_id}` | `200`, `401`, `404`, `503` | |
| `GET /v1/vehicles/{vehicle_id}/charges/{charge_id}/samples` | `200`, `304`, `400`, `401`, `404`, `503` | 1000 / 2048 |
| `POST /v1/device/revoke` | `204`, `400`, `401`, `404`, `503` | |

The drives route keeps its `1.1.0` limits (100 / 500) and its two `1.1.0` `503` forms (empty,
and JSON `service_unavailable`); like every route it can also answer the empty handler-timeout `503`
(see Handler time budget below). Order is
`start_date_ms DESC, id DESC` for drives and charges, `date_ms ASC, id ASC` for
positions and `timestamp_ms ASC, id ASC` for samples. A `404` on drive or charge is
JSON `drive_not_found` or `charge_not_found` for an unknown id of a granted vehicle and
JSON `vehicle_not_found` for an unknown or ungranted vehicle. `drive_id` and `charge_id`
are signed 64-bit integers of at least 1; any other value (non-numeric, zero, negative,
out of range) is the same `404` JSON, never `400`. Only closed sessions are served: a drive or
charge that has not ended (no `end_date_ms`) is left out of the lists, and its id is `404 drive_not_found`
or `404 charge_not_found` on the item route and on the positions and samples routes.

**Pages.** All four page routes share one rule set: query `from_ms` (inclusive, default
0, at least 0), `to_ms` (exclusive, default 9223372036854775807: omit it, and never send
that value from JavaScript, which cannot represent it), `limit` and `cursor`;
an unknown query parameter is `400 invalid_query`; the other `400` codes are
`invalid_time_range`, `invalid_limit` and `invalid_cursor`. A query value that does not
parse as a decimal integer in the parameter's range (`limit` 0 to 4294967295, `from_ms` and
`to_ms` signed 64-bit; for example `limit=abc`, `limit=-1`, `from_ms=1.5` or
`to_ms=9223372036854775808`) is also `400 invalid_query`: `invalid_limit` and
`invalid_time_range` apply only to well-formed values (`limit=0` or above the route maximum,
`from_ms=-1`, `from_ms` not below `to_ms`). The response is
`{"items": [...], "next_cursor": string|null}`. A serialised page is at most 1048576
bytes and is never split inside an item; an item that alone exceeds the cap is
`503 service_unavailable`. Every page has a strong `ETag` (the quoted lowercase hex
SHA-256 of the body), `Cache-Control: no-store` and `Content-Type: application/json`;
`If-None-Match` with the current `ETag` gives `304` with an empty body.

**Cursors.** `next_cursor` is opaque: keep it byte for byte, percent-encode it once as a
query value, and never parse or log it. Hubs issue `tqp1.<116 hex>.<64 hex>`: a 58-byte
payload (version 1, kind 1 drives / 2 charges / 3 positions / 4 charge samples, vehicle
UUID, scope id, `from_ms`, `to_ms`, last key A, last key B) and an HMAC-SHA256 tag under
the Hub's cursor key. A cursor is bound to that key, its kind, the vehicle, the scope
(the drive or charge id for nested pages, otherwise 0) and the exact window, so reusing it
anywhere else is `400 invalid_cursor`. The drives route still accepts the legacy `tqd1`
cursor for one release and no longer issues it. [`cursor.schema.json`](../../profiles/hub-http-v1/1.2.0/cursor.schema.json)
holds both grammars for Hubs and conformance; clients do not need them.

**Fields.** `drive` is the `1.1.0` drive plus optional `start_address_provenance` and
`end_address_provenance`. `position`, `charge` and `charge_sample` list their fields in
`resources.schema.json`; ids and `*_ms` values are signed 64-bit integers and never
null, `vehicle_id` is a UUID, every integer schema that spans signed 64 bits (ids, `*_ms`, and the
`from_ms`, `to_ms` and id parameters of the OpenAPI document) carries `format: int64` so a generator
picks a 64-bit type (an annotation: no validator of this package assigns it a check and it changes no
verdict), `end_date_ms` of a drive and of a charge is never null
(closed sessions only), and every other field may be null. `cost`, `cost_per_unit` and
`session_fee` are JSON numbers. `position.power` and the charge-sample charger fields are
whole numbers in TeslaMate but are typed `number`, because the page routes serve the
materialised projections of the Hub, which hold them as floating point: expect `12` or `12.0`. `address_provenance` is
`{source: import|geocoder|manual, provider, endpoint_host, language, licence, looked_up_at_ms}`
and `elevation_provenance` is `{source: vehicle|import|terrain, dataset, dataset_version, tile}`;
the provenance fields may be absent or null when no record exists.

**Access.** Every vehicle-scoped route checks the paired device's vehicle grant before
it reads the query, and answers an ungranted vehicle exactly as it answers an unknown one:
`404 vehicle_not_found` on JSON routes and an empty `404` on `/current`. The current drives route
does not check the vehicle before it reads the query yet: it parses the query first, so an unknown or
ungranted vehicle with an invalid query is `400` today, and the later Hub batch must move the check first,
as the case `ungranted-vehicle-404` holds a Hub to.
Each sync route keeps the `404` the Hub gives an unknown vehicle today, and an ungranted vehicle gets that same `404`.
`hub-sync-v1@1.3.0` lists the JSON `vehicle_not_found` `404` on changes-since and an empty `404` on
signing-keys and prepared-artefacts (its `status-tables.json`; `hub-sync-v1@1.4.0` publishes no status table and its OpenAPI
lists no `404` on changes-since or the manifest). It lists no `404` on manifest and no-op, where the Hub answers
the JSON `vehicle_not_found` envelope for a well-formed unknown vehicle id and an empty `404` for a
malformed one (the manifest also answers an empty `404` for a known vehicle that has no published full snapshot yet, so on the
manifest an empty `404` means a malformed id or that no snapshot is published yet): `1.2.0` asserts those two statuses (it does not inherit them). Conformance checks the
manifest's (JSON `vehicle_not_found` for a well-formed unknown, ungranted or nil vehicle id, an empty `404` for a
malformed one) and does not check the no-op's or the no-snapshot answer. For a well-formed unknown vehicle the Hub also answers JSON on prepared-artefacts,
where `hub-sync-v1@1.3.0` lists an empty `404`; `1.2.0` changes neither. The manifest route is served
whether or not `sync.packs` is advertised, so its `401` and `404` answers do not depend on that token.
`GET /v1/vehicles` lists only granted vehicles. A pack is requested by object name (the
lowercase hex SHA-256 digest plus `.sqlite.zst`, `GET /v1/packs/sha256/{object_name}`); it
resolves its owning vehicle before streaming, and an ungranted digest is an empty `404`
indistinguishable from an unknown one.

**Malformed identifiers.** A `vehicle_id` that is not a UUID, a path segment (`vehicle_id`, `drive_id`,
`charge_id` or a pack name) that percent-decodes to bytes that are not UTF-8 (for example `%FF`), and the nil
UUID `00000000-0000-0000-0000-000000000000` (well formed, but no vehicle can have it) are answered as not found
(each route's malformed-identifier answer, or for the nil UUID its unknown-vehicle answer), after the bearer check: an empty `404` on `/current`; JSON `vehicle_not_found` on the JSON routes
(`drive_not_found` or `charge_not_found` for such a `drive_id` or `charge_id`); on the sync manifest and no-op an
empty `404` for a malformed id and JSON `vehicle_not_found` for the nil UUID (the manifest's empty `404` is also what a known vehicle with no
published full snapshot answers; see Access); an empty `404` on the pack route for a
name that is not a digest plus `.sqlite.zst`; the other sync routes answer as `access.sync_routes` of `profile.json` says. A request without
a valid bearer is `401` whatever its path, never `400`. Only the lowercase hyphenated form of a `vehicle_id` and the plain decimal
form of a `drive_id` or `charge_id` are specified: a Hub's parser may also accept another spelling of the same value (a UUID in upper
case, without hyphens, in braces or as `urn:uuid:`; an integer with a plus sign or leading zeros) and serve the same resource, which a
client must not rely on being refused. The current Hub does not do this yet, and the later Hub batch must: its handlers take the path as
`Path<String>`, which axum answers `400` with a `text/plain` body for an undecodable segment before the handler
runs, so before authentication, and the nil UUID is `503` on `/current` and the sync routes (the drives route already
answers it as unknown). Take the path through `Result<Path<_>, PathRejection>` (or a raw segment), authenticate
first, then answer any path rejection with the route's not-found form, and treat the nil UUID as an unknown vehicle
on every vehicle-scoped route, the hub-sync-v1 routes included.

**Rotation, lifetime and revocation.** The prior bearer stays valid for normal requests
through `rotation_at_ms + 86400000` (exclusive). A retry of rotate with the prior bearer
during the grace window returns the same successor; it never issues a different token.
After the window the prior bearer is `401`. Only the bearer replaced by the latest rotation has
a grace window: a further rotation ends the earlier bearer's grace, and it is then `401`. A bearer's `expires_at_ms` slides to at most
now plus 30 days on use and never beyond the device's absolute maximum (owner-configurable,
default 180 days from pairing). When rotation cannot move the expiry later, rotate returns
`200` with the current bearer and expiry unchanged and `"reason": "max_lifetime"`, and
issues nothing: ask the owner to re-pair before `expires_at_ms`. `POST /v1/device/revoke`
takes the current or grace bearer and no request body (a non-empty body, including one over 4096
bytes, is `400` and revokes nothing, never `413`), revokes the calling device and returns `204`. Afterwards the bearer is
`401` on every route, so a repeated revoke is `401`; treat `401` on revoke as already
revoked. Call it only when discovery advertises `auth.revoke`: when paired-device
authentication is off the token is absent and the route answers `404` with an empty body,
as rotate does.

**Claim size.** `POST /v1/pairings/{pairing_id}/claim` takes at most 4096 bytes of request body.
A larger body with a JSON `Content-Type` (`application/json` or any `application/*+json`) is `413` with a bounded `text/plain` body (axum's "Failed to buffer the
request body: length limit exceeded"), and the claim is not attempted; a missing `Content-Type`, or one that is not a JSON media type
(neither `application/json` nor `application/*+json`), is `415` whatever the size, because the extractor checks it before it reads the body. `413` is therefore part of the claim route's status
set in `1.2.0` (`200`, `400`, `401`, `404`, `413`, `415`, `422`, `503`), and the "unobserved `413`"
advice in the `1.1.0` text below does not apply to `1.2.0`. The revoke route has no such status:
no layer of the Hub rejects an over-limit body it does not read, and a revoke handler must not read
it through an extractor that would answer `413`.

**Device name.** A claim's `device_name` is 1 to 128 UTF-8 bytes with no control character (U+0000 to U+001F and U+007F to U+009F).
A name that breaks the rule is `422` with a bounded `text/plain` body, answered before the invitation is consulted: the refusal consumes
nothing, and the same invitation still claims with a valid name. The schema says `minLength` 1, `maxLength` 128 and the
control-character rule, but `maxLength` counts characters and cannot state the byte limit, so a client checks the byte length itself.
The current Hub does not do this yet, and the later Hub batch must: it validates the name inside the store call, where an empty name, one
over 128 bytes or one with a control character is an error that the claim handler answers with an empty `503`, which no client can correct
by retrying. Conformance sends six such names before the real claim and expects `422` for each, then a `200` for the claim itself.
A client that takes the name from free text, such as the Home Assistant integration, can reach this case.

**Handler time budget.** The Hub answers any route, including `/healthz`, `/readyz`, the
discovery route and the `hub-sync-v1` routes, with `503` and an empty body (no content type) once
its 15 second handler budget is spent; the budget includes the wait for one of the 32 in-flight
request slots. Treat an empty `503` on any route except the claim as a transient outage and retry later. A claim is never retried
automatically (see the `1.1.0` text on the claim below): the invitation is single use, and a claim that ended in an empty `503` may
already have consumed it. A route that lists a JSON `503` (`service_unavailable` on discovery, the page and the item routes,
`not_ready` on `/readyz`) can therefore also answer an empty one, and `/healthz` lists `503` for
this reason alone.

**Browser origins.** A request that carries an `Origin` header is answered `403` with an empty body and
`Cache-Control: no-store`, before the route runs, on any route (the `hub-sync-v1` routes included), when the Hub does
not admit that origin (neither its own origin nor one of the owner's allowed origins) or the route has no browser
access. It is in no route's status set, like the handler-timeout `503`. Which routes admit browser origins, and how the Hub
answers a CORS preflight (an `OPTIONS` request), are set by the Hub and lie outside this profile. A native client sends no `Origin`
header and never sees any of them.

**Errors.** `{"error": {"code", "message"}}` with `code` matching `^[a-z_]{1,64}$`; handle
an unknown code as a generic failure of its status. Known codes: `invalid_query`,
`invalid_time_range`, `invalid_limit`, `invalid_cursor`, `vehicle_not_found`,
`service_unavailable`, `drive_not_found`, `charge_not_found`. Unsupported: vehicle
commands, SSE, metadata CRUD, snapshot revisions and the protocol-version header; the root
`teslatlas-v1` family is not served.

**Conformance.** The offline checks need no Hub and never prove anything about one:

```sh
./conformance/run --profile hub-http-v1@1.2.0
```

They verify the profile directory (exact `SHA256SUMS`, strict JSON, Draft 2020-12
schemas, OpenAPI, every example against the open and the server-strict schema, `maxItems`
on every array, capability grammar, cases, cursors, rotation and error text, field
semantics). The 50 native cases run only with `--config`, never because of an environment
variable, and only against a Hub you own: they revoke and rotate the disposable bearers the
descriptor names. A case that declares several routes runs on all of them or is `not_run`
(naming each capability the Hub did not advertise) before anything is sent: a case never passes on
part of its routes. The claim case first sends the six `device_name` values a Hub must refuse (see Device name) and then the
claim itself, which must still succeed because a refusal consumed nothing. `--config` is a private owner-only descriptor
(`schema_version` 1, `kind` `hub-http-v1-actual-hub`, `profile_id`, a loopback HTTPS
`endpoint`, `certificate_path`, optional `tls_pin`, and `inputs` named in
`cases.json`). A case whose inputs, advertised capability or data are missing is reported
`not_run` with the missing item named; it is never counted as passed. The grant cases first
read their target with `bearer` (`ungranted_vehicle_id` must be a vehicle `bearer` can read,
`ungranted_pack_digest` a pack `bearer` can download), so a vehicle or digest that does not exist
is `not_run`, not a pass; `expired_grace_bearer` is attested by the descriptor, because a
bearer the Hub never issued is also `401`. The runner gives the
native checks 80 percent of `--timeout-seconds` (default 10); raise it, for example to
`--timeout-seconds 300`, or most native cases report `not_run` for the time budget. An
exchange never outlasts that budget and does not start with less than a quarter of a second of it
left, so the report of the cases already run is kept. An exchange that the budget ends while it is in flight
makes its case `not_run` for the time budget, never a failure of the Hub; an exchange that reaches its own
five second limit with budget to spare is still a failure. The
exit status is `0` only when everything requested passed (the offline checks, and with
`--config` every one of the 50 cases), `1` when a check failed and `3` when nothing failed
but a requested check did not run, so a run that left cases `not_run` never reads as a pass.
The `later-current` case asks the harness to append one observation and waits up to 30
seconds for the receipt file (a run budget that ends first makes the case `not_run` for the time
budget, not a failure); a harness should write it to a private temporary file and
rename it into place, though a receipt that exists before it has content is read again
until it parses. Output carries case ids and fixed labels only, never a bearer.

**Deviations from the design note.** Page capabilities follow cursor-key availability,
not unconditional advertisement. Rotation near the absolute maximum returns the current
bearer with `reason: "max_lifetime"` instead of minting a capped token. A repeated revoke
is `401`, not `204`. Invalid `drive_id` and `charge_id` are `404`, not `400`.

**Beyond the specification.** Eighteen choices go past the G2-0a specification and are
recorded here until the specification lists them. (1) The revoke route lists `400` for a
non-empty body, and `404` for a Hub with paired-device authentication off, so its status set
is `204`, `400`, `401`, `404`, `503`; a non-empty body of any size, including one over 4096 bytes, is
`400` and never `413`. That is a requirement on the Hub, because no layer of it rejects a body the
handler does not read. (2) `position.power` and the four charge-sample charger
fields are typed `number`, not integer: the page readers return `ProjectionPosition` and
`ProjectionChargeSample`, whose values are floating point, not the integer
`Projection*V2_2` pack mirrors. (3) Each sync route keeps the `404` the Hub gives an unknown
vehicle today, for an ungranted vehicle too, instead of becoming an empty `404`. The manifest and
no-op `404`s (JSON `vehicle_not_found` for a well-formed unknown vehicle, empty for a malformed id, and on the manifest also empty
for a known vehicle with no published full snapshot yet) are asserted by this profile, not inherited: `hub-sync-v1@1.3.0` lists no `404` on those two routes. (4) The pack
route is `/v1/packs/sha256/{object_name}`, the object name being the digest plus `.sqlite.zst`, as in
`hub-sync-v1` and `1.1.0`. (5) The OpenAPI `to_ms` parameter has no schema `default`: the
default, 9223372036854775807, is in its description and in `profile.json`, because generated
JavaScript clients would otherwise send a value they cannot represent. (6) The handler time
budget makes an empty `503` possible on every route, so `/healthz` lists `503` (the specification
leaves its status set unchanged), discovery lists an empty `503` besides its JSON one and `/readyz`
an empty one besides `not_ready`. (7) Only closed sessions are served: a drive or charge in
progress is not listed and its id is `404 drive_not_found` or `404 charge_not_found` (the specification
only makes `end_date_ms` non-null). (8) The claim route lists `413`, with a `text/plain` body, for a
request body over 4096 bytes. (9) The manifest route is served whether or not `sync.packs` is
advertised, so its `401` and `404` answers do not depend on that token. (10) Every integer schema that
spans signed 64 bits carries `format: int64`, an annotation that changes no validation verdict.
(11) A query value that is not an integer of the parameter's type is `400 invalid_query`. (12) A
further rotation ends the earlier bearer's grace. (13) A path segment that is not valid UTF-8 and the nil UUID are
malformed or unknown identifiers answered with a `404` after the bearer check, never `400`, and never `503` because of the value
(a requirement on the later Hub batch; see Malformed identifiers). (14) A request with an `Origin` header the Hub does not admit is
`403` on any route. (15) Conformance has one case beyond the specification's list, `malformed-identifier-404`. (16) A claim whose
`device_name` is empty, over 128 UTF-8 bytes or has a control character is `422` with a `text/plain` body and consumes nothing (a
requirement on the later Hub batch; see Device name), and a claim is never retried automatically, so the empty `503` rule
excludes it. (17) Every vehicle-scoped route checks the grant before it reads the query (a requirement on the later Hub batch for
the drives route; see Access). (18) Only the lowercase hyphenated UUID and plain decimal ids are specified: other spellings a Hub's
parser accepts may be served, and a client must not rely on their refusal (see Malformed identifiers).
Changing any of these after the profile is frozen
is a new major profile.

## Choose the profile

Use this profile when discovery returns `protocol: "teslatlas-sync"`,
`protocol_major: 1`, and `api_versions: ["1.0"]`. A Hub advertises either
the reduced capability set
`["query.vehicles", "query.current"]` or the full set
`["query.vehicles", "query.current", "query.drives", "sync.packs"]`.
Require `query.drives` before attempting the drives route. `sync.packs` is a
delivery capability; it does not add a query endpoint.

The `version` field is the Hub product version (for example `2026.36.2`),
not a wire revision. The profile identity remains `hub-http-v1@1.1.0`.

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
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.1.0" \
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
    raise SystemExit("discovery does not match hub-http-v1@1.1.0")
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
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.1.0" \
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
schema = {**schema, "$ref": "#/$defs/claim"}
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
invitation. (`hub-http-v1@1.2.0` specifies the answer: `413` with a bounded
`text/plain` body, see Claim size in the `1.2.0` section.)

## Read resources

Create an authorization header file without putting the bearer in shell
arguments or history:

~~~sh
set -eu
umask 077
TESLATLAS_CLAIM_RESPONSE="$private_dir/claim.json" \
TESLATLAS_AUTH_HEADER="$private_dir/authorization.header" \
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.1.0" \
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
  TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.1.0" python3 - <<'PY'
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
    raise SystemExit("resource response does not match hub-http-v1@1.1.0")
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
  TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.1.0" python3 - <<'PY'
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
    raise SystemExit("drive response does not match hub-http-v1@1.1.0")
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
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.1.0" python3 - <<'PY'
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
new response private before replacing the old header file. A successful
rotation leaves the prior bearer valid for 24 hours, ending exactly at
`rotation_at_ms + 86,400,000` milliseconds. A client that loses the response
MAY retry `POST /v1/device/rotate` with that prior bearer during the grace
window and use the replacement claim it receives. The claim response fields
remain `device_id`, `access_token`, and `expires_at_ms`.

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
TESLATLAS_PROFILE="$PWD/profiles/hub-http-v1/1.1.0" python3 - <<'PY'
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
[ "$old_status" = 200 ] || { echo 'old bearer was not accepted during rotation grace' >&2; exit 1; }
rm -f "$private_dir/authorization.old"
~~~

Validate the replacement response before replacing the header. A non-200
response is an error and must leave the old header untouched; on `200`, run the
same strict `auth.schema.json` validation used for claim responses, write the
new header to a mode-600 temporary path, then atomically rename it over the old
header. The old bearer must be treated as expired after the 24-hour grace
window. Owner revocation and re-pairing are lifecycle operations outside this
HTTP profile; there is no public revoke route.

## Errors and unsupported surfaces

Branch on the documented HTTP status and the profile's stable JSON error code,
never on implementation-specific text. Claim extractor failures (`400`,
`415`, and `422`) are bounded `text/plain`; they are not the JSON error
envelope. A `401` claim means the secret is wrong, expired, or already used.
Do not retry it. A `503` may indicate an unavailable service, but a claim that
timed out still requires a fresh owner invitation before another attempt.

The current-Hub profile intentionally has no public SSE/events endpoint,
command submission, metadata CRUD, snapshot revision, or (in `1.1.0`) charge-query route.
Do not send rich-profile headers such as `Teslatlas-Protocol-Version` and do
not infer unsupported routes from discovery. The sync appendix documents pack
delivery checks separately from these HTTP query semantics. A schema 2.2
no-op that is unavailable returns an unsigned, empty `406` with
`Cache-Control: no-store`; it is never a signed `503` or an invented snapshot.

For the full route schemas, examples, and conformance vectors, use the profile
directory and [`docs/conformance.md`](../guides/conformance.md). The native acceptance
adapter requires a private fixture descriptor and a same-host owned Hub; a
reference adapter or a successful schema check is not installed-product
acceptance.

Installed-v2 evidence publication currently fails closed. Its actor cannot
truthfully assert its own completed exit while still awaiting a close
acknowledgement, and successful HTTP/schema tests do not satisfy that lifecycle
gate. The approved architecture for a future explicitly versioned revision
separates immutable actor-owned pre-exit observations from exact runner-owned
post-exit, process-group and fixture-settlement witnesses. Final admission must
atomically require both bound records. This architecture does not supply a new
schema or handshake implementation; common Hub integration and disposable
installed evidence remain required before the refusal can be removed.

Restart evidence also remains a layered enforcement question. An asserted
restart and extant controller anchors alone do not establish a new process
generation. The common Hub controller/runner must identify the enforcing
case-specific observations before combined installed admission can be claimed.
