# Normative references

Teslatlas requirements use the following primary specifications. If this
repository narrows a permitted standards behaviour, the narrower public
contract applies to Teslatlas implementations.

| Area | Specification | Teslatlas use |
| --- | --- | --- |
| Well-known discovery | [RFC 8615](https://www.rfc-editor.org/rfc/rfc8615.html) | `/.well-known/teslatlas-hub` shape and deployment considerations |
| HTTP semantics, validators | [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html) | ETag, `If-None-Match`, `If-Match`, and `304` |
| Server-Sent Events | [WHATWG HTML](https://html.spec.whatwg.org/multipage/server-sent-events.html) | framing, event IDs, reconnect, and terminal `204` |
| Problem details | [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457.html) | `application/problem+json` error base |
| OpenAPI | [OpenAPI 3.1.1](https://spec.openapis.org/oas/v3.1.1.html) | operation and HTTP representation contract |
| JSON Schema | [Core](https://json-schema.org/draft/2020-12/json-schema-core.html) and [Validation](https://json-schema.org/draft/2020-12/json-schema-validation.html) | all public data schemas |
| Cursor pagination | [RFC 8977](https://www.rfc-editor.org/rfc/rfc8977.html) and [RFC 9865](https://www.rfc-editor.org/rfc/rfc9865.html) | opaque cursor model and profile vocabulary |
| Deprecation | [RFC 9745](https://www.rfc-editor.org/rfc/rfc9745.html) | `Deprecation` response header |
| Sunset | [RFC 8594](https://www.rfc-editor.org/rfc/rfc8594.html) | scheduled removal response header |
| Canonical JSON | [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785.html) | projection and metadata hash inputs |
| Version syntax | [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html) | protocol and capability version numbers |

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
