# Normative references

Teslatlas requirements use the following primary specifications. If this
repository narrows a permitted standards behaviour, the narrower public
contract applies to Teslatlas implementations. Explicit compatibility departures
below are separate policies and do not establish standards conformance.

The table describes the rich semantic profiles unless it says otherwise. The
candidate current-Hub HTTP profile is a separate OpenAPI 3.1.0 bundle with
adjacent JSON Schema files; use its own profile documents and
[`current-hub.md`](current-hub.md) for its available routes and errors.

| Area | Specification | Teslatlas use |
| --- | --- | --- |
| Requirement words | [RFC 2119](https://www.rfc-editor.org/rfc/rfc2119.html) and [RFC 8174](https://www.rfc-editor.org/rfc/rfc8174.html) | uppercase normative terms in prose contracts |
| Well-known discovery | [RFC 8615](https://www.rfc-editor.org/rfc/rfc8615.html) | `/.well-known/teslatlas-hub` shape and deployment considerations |
| HTTP semantics, validators | [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html) | ETag, `If-None-Match`, `If-Match`, and `304`; selected rich compatibility departure below |
| Server-Sent Events | [WHATWG HTML](https://html.spec.whatwg.org/multipage/server-sent-events.html) | framing, event IDs, reconnect, and terminal `204` |
| Problem details | [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457.html) | `application/problem+json` error base |
| OpenAPI | [OpenAPI 3.1.1](https://spec.openapis.org/oas/v3.1.1.html) | operation and HTTP representation contract |
| JSON Schema | [Core](https://json-schema.org/draft/2020-12/json-schema-core.html) and [Validation](https://json-schema.org/draft/2020-12/json-schema-validation.html) | all public data schemas |
| Deprecation | [RFC 9745](https://www.rfc-editor.org/rfc/rfc9745.html) | `Deprecation` response header |
| Sunset | [RFC 8594](https://www.rfc-editor.org/rfc/rfc8594.html) | scheduled removal response header |
| Canonical JSON | [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785.html) | projection representation hashing; candidate metadata hashing only, with current metadata byte domain unestablished |
| Version syntax | [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html) | protocol and capability version numbers |

### Selected rich ETag compatibility and successor candidate

The selected rich profiles `1.0.0`, `1.1.0` and `1.2.0` retain the explicitly
approved inner-space and literal-backslash witnesses, including `"with space"`
and `"with\backslash"`. Inner ASCII space departs from RFC 9110 entity-tag
syntax; backslash is a valid opaque entity-tag octet and is not an escape
introducer. This compatibility choice preserves selected frozen consumers. It
does not authorize all other invalid byte values or claim complete header
grammar coverage. Existing control/header-injection checks, strong metadata
versus weak general GET distinctions and binding-specific budgets remain.
CurrentHub and deployed-v1 validator bindings remain separate.

A standards-correct successor candidate defines a single entity tag as an
optional literal `W/`, a double quote, one or more opaque `etagc` octets, and a
closing double quote. `etagc` is exactly `0x21`, `0x23–0x7E`, or `0x80–0xFF`.
This nonempty policy intentionally narrows RFC 9110's permitted empty tag.
Ordinary GET validators may be weak; metadata entity tags and its mutation
precondition must be strong. Space, controls, DEL, an inner double quote and
unquoted tags fail this candidate grammar. Backslash `0x5C` remains opaque;
neither escaping nor unescaping changes the validator. This is the entity-tag
grammar, not an additional header-list or wildcard policy.

The candidate observation boundary is the original HTTP field-value octets,
or an explicitly documented lossless mapping of one code point to each octet
`0x00–0xFF`. An arbitrary Unicode header string with unspecified encoding does
not establish its wire octets and cannot support a standards-parity claim.
This candidate does not select a new profile, narrow current rich admission,
change request/response budgets, or regenerate any frozen SDK input.

### Metadata hash authority

The exact persisted metadata emitter, preimage and canonical-byte compatibility
are unestablished. Current public digest examples are illustrative; their
spelling and audit linkage do not prove an independently reproducible value
digest or authenticity. The selected wire shape, live audit chain and separate
terminal deletion event remain unchanged. No historical digest is recalculated,
and there is no deletion sentinel or new tombstone hash field.

The unselected `metadata-value-sha256-jcs-v1` candidate defines lowercase
SHA-256 over the UTF-8 bytes of RFC 8785 JCS applied to `metadata_record.value`
only, with no BOM, trailing newline, identity, revision, timestamp or audit
members added. Its prerequisites are finite IEEE 754 binary64 values with the
candidate's declared numeric interpretation and valid Unicode without lone
surrogates; it uses RFC 8785 UTF-16 property ordering. Candidate byte/digest
vectors establish that candidate only. They do not narrow the current nested
metadata JSON number domain or infer compatibility with a supported emitter,
excluded consumer or persisted history. See
[`commands-and-metadata.md`](commands-and-metadata.md) for the audit and candidate
verification boundaries.

### Edge delivery key-order erratum

The `edge-delivery-v2@2.0.0` machine profile explicitly names
`edge-json-utf8-key-order-v1` as an in-place authority erratum for the already
prescribed identity behavior. Every object, including objects within arrays,
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
its selected bytes and identifiers, not broad numeric behavior or actual Edge
runtime, atomic commit, ACK, replay or crash acceptance.

## Project policies

These rules are Teslatlas contract choices, not requirements imposed by the
standards above:

- exact-millisecond UTC timestamps;
- cursor binding, errors, and expiry meanings;
- at least 86,400 seconds of event replay and idempotency retention;
- current plus previous two minor conformance profiles;
- two later minor versions and 180 days before deprecated removal;
- the command catalogue, retry classes, and metadata audit model;
- the fixed v1 request, range, page, and concurrency limits.

The `teslatlas-hub` well-known suffix is defined here for private and
development use. This repository does not claim an IANA registration.

## Pagination background

[RFC 8977](https://www.rfc-editor.org/rfc/rfc8977.html) defines RDAP sorting and
paging; [RFC 9865](https://www.rfc-editor.org/rfc/rfc9865.html) defines SCIM
cursor pagination. They provide background for cursor-based designs, not
Teslatlas wire requirements. Teslatlas cursor fields, binding, and errors are
defined by its own profiles and [HTTP contract](http.md#opaque-cursors).
