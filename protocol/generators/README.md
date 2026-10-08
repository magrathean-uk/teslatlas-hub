# Generated clients for `hub-http-v1@1.2.0`

`profiles/hub-http-v1/1.2.0/openapi.json` is an OpenAPI 3.1.0 document. It
refers to the schemas next to it (`resources.schema.json`, `discovery.schema.json`,
`auth.schema.json` and `errors.schema.json`), so run a generator against the
profile directory itself and never against a copy of `openapi.json` alone.

No generated code is committed. Generate a client when you build your own
project, and keep it out of this repository.

## Invocations

[`openapi-generator`](https://openapi-generator.tech/) needs a release with OpenAPI
3.1 support (7.x or later). Run each command from the Protocol root and write the output outside this tree.

```sh
openapi-generator-cli generate \
  -i profiles/hub-http-v1/1.2.0/openapi.json \
  -g swift5 \
  -o "$OUT/hub-http-swift5"

openapi-generator-cli generate \
  -i profiles/hub-http-v1/1.2.0/openapi.json \
  -g typescript-fetch \
  -o "$OUT/hub-http-typescript-fetch"

openapi-generator-cli generate \
  -i profiles/hub-http-v1/1.2.0/openapi.json \
  -g python \
  --additional-properties=disallowAdditionalPropertiesIfNotPresent=false \
  -o "$OUT/hub-http-python"
```

The extra option on the Python recipe is required, not optional; see
[Unknown fields](#unknown-fields).

Generated code is a transport and model layer only. Pairing, bearer storage,
rotation, cursor handling and error handling remain the client's job; follow
[`docs/reference/current-hub.md`](../docs/reference/current-hub.md).

## Integers

Every identifier and every `*_ms` timestamp is a signed 64-bit JSON integer
(`-9223372036854775808` to `9223372036854775807`), and the schemas say so: each such integer
schema, and the id and window parameters of the OpenAPI document, carries `format: int64`, which
`openapi-generator` reads to choose a 64-bit type where the target language has one (`swift5` `Int64`, `python`
`int`; `typescript-fetch` still maps it to `number`, a double, so follow the JavaScript rule below). The format is an annotation:
no validator of this repository assigns it a check, and it changes no verdict.
JavaScript numbers are exact only for values within +-(2^53 - 1), that is
`-9007199254740991` to `9007199254740991`. A JavaScript or TypeScript consumer
must decode these fields losslessly (for example as `BigInt`) or reject any
value outside that range before it converts the value. It must never round an
identifier silently. Ids and millisecond timestamps a Hub produces today are far
below the limit, so rejection is a safety net, not an expected path.

`position.power` and the four `charge_sample` charger fields
(`charger_power_kw`, `charger_voltage`, `charger_actual_current`,
`charger_pilot_current`) are JSON numbers, not integers: a whole value may be
written `12` or `12.0`. Decode them as floating point.

The window parameters of the page routes are `from_ms` (default 0) and `to_ms`. The
default of `to_ms` is the signed 64-bit maximum, `9223372036854775807`, which is
outside the JavaScript range above: a JavaScript client rounds it to
`9223372036854775808`, which no longer fits a signed 64-bit integer, so a Hub rejects
the request.
The schema therefore carries no `default` for `to_ms`. Omit `to_ms` to read to the end
of the data, and never send that value.

## Unknown fields

The profile is additive within 1.x. A Hub may add optional fields to any response
object and new capability tokens, and the schemas do not close response objects.
Configure every generated client to **ignore unknown fields** and unknown
capability tokens:

- Swift: model structs decode with `Codable`, which ignores unknown keys. Do not
  add a strict key check.
- TypeScript: do not enable a runtime validator that rejects extra properties.
- Python: pass `--additional-properties=disallowAdditionalPropertiesIfNotPresent=false`
  to `openapi-generator` (it is in the recipe above). Its default in 7.x is `true`,
  and the profile's response objects deliberately omit `additionalProperties`, so
  without the option every generated model's `from_dict` raises `ValueError` on any
  key the schema does not declare, the first time a Hub adds an optional field. Do not
  set `extra="forbid"` or the equivalent on models either.

Do not turn the schemas into strict validators on the client. Servers are held to
the strict form by conformance (`./conformance/run --profile hub-http-v1@1.2.0`
checks every example against a variant of each response schema that forbids
undocumented fields); clients are not.

## Cursors, ETags and errors

- Treat `next_cursor` as an opaque string. Send it back unchanged, percent-encoded
  once as a query value. Never parse, log or store it beyond the paging loop.
- Page routes return a strong `ETag`. Send it back as `If-None-Match` to get `304`.
- `401` has an empty body on every bearer route, including `POST /v1/device/revoke`:
  after a revoke, a `401` means the device is already revoked.
- Error codes are open strings (`^[a-z_]{1,64}$`); handle an unknown code as a
  generic failure of its HTTP status.
- A pairing claim body over 4096 bytes is `413` with a bounded `text/plain` body, not the JSON
  error envelope (a claim whose `Content-Type` is missing or is not a JSON media type, `application/json` or
  `application/*+json`, is `415` whatever its size); `POST /v1/device/revoke` takes no body at all (any body is `400`).
- A claim's `device_name` is 1 to 128 UTF-8 bytes with no control character; a name that breaks the rule is `422` with a
  bounded `text/plain` body and the invitation is not consumed. The schema's `maxLength` counts characters, so check the byte length yourself.
- On the bearer routes, a `vehicle_id`, `drive_id`, `charge_id` or pack name that is malformed, including one that is not valid UTF-8
  (`%FF`), and the nil UUID are a `404` (an unknown vehicle, drive or charge), never a `400`. The `pairing_id` of the claim route is
  not covered by this rule.

## Empty 503

Any route can answer `503` with an empty body and no content type when the Hub's handler
time budget is spent (the OpenAPI `info.description` and `limits.handler_timeout_response` in
`profile.json` say so), including routes whose documented `503` is a JSON envelope. A generated
client must not try to decode a body on a `503`: treat an empty `503` as a transient,
retryable failure, except on the claim route, which is never retried automatically (the invitation is single use and the
claim may already have consumed it).
