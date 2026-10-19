# Normative references

Teslatlas requirements use the following primary specifications. If this
repository narrows a permitted standards behaviour, the narrower public
contract applies to Teslatlas implementations. Explicit compatibility departures
below are separate policies and do not establish standards conformance.

The table describes the contracts this package keeps: the current-Hub HTTP
profile `hub-http-v1@1.2.0`, the Hub sync profiles `hub-sync-v1@1.3.0` through
`1.5.0`, and `receiver-v1@1.0.0`. Use each profile's own documents for its
routes, errors and limits: [`current-hub.md`](current-hub.md) for the HTTP
profile and [`hub-sync.md`](hub-sync.md) for the sync profiles.

| Area | Specification | Teslatlas use |
| --- | --- | --- |
| Requirement words | [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119.html) and [RFC 8174](https://www.rfc-editor.org/rfc/rfc8174.html) | uppercase normative terms in prose contracts |
| Well-known discovery | [RFC 8615](https://www.rfc-editor.org/rfc/rfc8615.html) | `/.well-known/teslatlas-hub` shape and deployment considerations |
| HTTP semantics, validators | [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html) | strong `ETag` and `If-None-Match` with `304` on the `hub-http-v1@1.2.0` reads; strong `ETag` and a single byte range (`206` with `Content-Range`) on the `hub-sync-v1@1.3.0` pack route |
| OpenAPI | [OpenAPI 3.1.1](https://spec.openapis.org/oas/v3.1.1.html) | operation and HTTP representation contract of the OpenAPI documents of every retained profile (`hub-http-v1@1.2.0`, `hub-sync-v1@1.3.0`, `1.4.0` and `1.5.0`), each of which declares OpenAPI 3.1.0 |
| JSON Schema | [Core](https://json-schema.org/draft/2020-12/json-schema-core.html) and [Validation](https://json-schema.org/draft/2020-12/json-schema-validation.html) | the data schemas of every retained profile |
| Canonical JSON | [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785.html) | signed payloads and digests of the sync profiles; the receiver profile's informative identity block, with the key-order erratum below |
| Version syntax | [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html) | protocol and capability version numbers |

### Entity tags in the current-Hub profile

`hub-http-v1@1.2.0` gives each page read of its drives, drive positions, charges
and charge samples a strong `ETag`: the quoted lowercase hex SHA-256 of the
response body. `If-None-Match` carrying the current tag returns `304` with an
empty body, the same `ETag` and `Cache-Control: no-store`. The profile defines
no `If-Match`, weak-validator or tag-list behaviour beyond that, and makes no
claim of complete header grammar coverage. The profile files are the authority;
this section only names the standard they rest on. The inner-space and
literal-backslash entity-tag witnesses of the retired root contract, its
metadata hash candidates and its other validator rules were retired with it.

### Receiver identity key-order erratum

The `receiver-v1@1.0.0` machine profile carries, as informative text, the
identity block of the retired `edge-delivery-v2@2.0.0` profile. That block
names `edge-json-utf8-key-order-v1` as an in-place authority erratum for the
already prescribed identity behavior. Every object, including objects within arrays,
sorts its raw property names by unsigned lexicographic UTF-8 byte order before
JSON escaping, without Unicode normalization. Shorter strings precede longer
strings with the same byte prefix. Array order is preserved.

This differs from RFC 8785's UTF-16 property order when two keys first differ
at U+E000 to U+FFFF on one side and a supplementary character on the other.
Existing `2.0.0` identities MUST retain the UTF-8 order. The erratum preserves
identity domains, field inclusion, array order and all frozen record IDs; it
does not migrate identities to ordinary RFC 8785 JCS. Other existing
serialization semantics are not changed by this key-order correction. Broad
numeric interpretation for signed64 timestamps and unrestricted payloads
remains unresolved. The bounded nested Unicode/integer/string corpus proves
its selected bytes and identifiers, not broad numeric behavior or any runtime,
atomic commit, replay or crash acceptance.

## Project policies

These rules are Teslatlas contract choices, not requirements imposed by the
standards above:

- epoch-millisecond timestamps where a profile names a millisecond member, for
  example `expiresAtMs`;
- cursor binding, errors, and expiry meanings, as each profile defines them;
- the request, range, page and body limits each profile states;
- profile versions follow [`versioning.md`](versioning.md): a profile changes
  only as a new profile, a stable route or field is removed only by a new major
  profile, and a profile retired from this package is deleted from it.

The `teslatlas-hub` well-known suffix is defined here for private and
development use. This repository does not claim an IANA registration.

## Pagination background

[RFC 8977](https://www.rfc-editor.org/rfc/rfc8977.html) defines RDAP sorting and
paging; [RFC 9865](https://www.rfc-editor.org/rfc/rfc9865.html) defines SCIM
cursor pagination. They provide background for cursor-based designs, not
Teslatlas wire requirements. Teslatlas cursor fields, binding, and errors are
defined by its own profiles.
