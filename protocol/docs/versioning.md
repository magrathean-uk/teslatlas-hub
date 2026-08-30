# Versioning, discovery, and capabilities

## Discovery

A Hub publishes `GET /.well-known/teslatlas-hub` over HTTPS and returns the
document defined by `schemas/discovery.schema.json` as `application/json`.
The document is public metadata: it contains a stable Hub identity, protocol
support, capabilities, endpoints, and limits. It must not contain credentials,
provider tokens, vehicle identifiers, VINs, or user data.

`teslatlas-hub` is the protocol's well-known suffix. It is not represented as
an IANA registration by this repository. General Internet deployment must
complete the RFC 8615 registration process; private and development deployments
use this specification as the suffix definition.

## Protocol versions

The protocol uses semantic versions `MAJOR.MINOR.PATCH`.

- A major version may remove or incompatibly change wire behaviour.
- A minor version only adds optional capabilities, fields, resources, or
  stricter server guarantees. Existing fields retain their meaning.
- A patch version clarifies text or fixes a contract defect without changing a
  valid request or response.

The current foundation profile is `1.2.0`. Its compatibility window is
`1.0.0`, `1.1.0`, and `1.2.0`. These are protocol profiles, not claims that a
particular Hub or SDK release has shipped.

Clients normally send `Teslatlas-Protocol-Version` on every versioned request,
using the highest version they understand. The server selects the highest
supported version with the same major number that is not newer than the client
version, and returns that selection in the same response header. A server
returns a `426` `unsupported_protocol_version` problem when no compatible
version exists. A client may omit the request header; omission selects exactly
the `minimum_client_version` published by discovery.

## Capabilities

Clients use only capabilities advertised by discovery. A capability has an
independent semantic version, a protocol version in which it was introduced,
a status, and a path. `experimental` capabilities carry no compatibility
promise. Stable capabilities obey the protocol compatibility window.

Unknown fields and unknown capabilities are ignored unless a schema explicitly
forbids them. Unknown problem-detail extension members are always ignored.

## Deprecation and removal

A stable capability can be deprecated only after a replacement or migration
document exists. Discovery then marks it `deprecated` and supplies its
deprecation instant, nullable sunset instant, successor, and documentation
URI. A null `sunset_at` means removal is not scheduled. Affected HTTP responses
emit `Deprecation` and a `Link` with `rel="deprecation"`; they emit `Sunset`
only when `sunset_at` is non-null.

Removal occurs no earlier than both:

1. two later minor protocol versions have been published; and
2. 180 days have elapsed since the deprecation instant.

The compatibility runner evaluates the current profile and the previous two
minor profiles on every local release gate.

## Normative references

- RFC 8615, Well-Known Uniform Resource Identifiers
- Semantic Versioning 2.0.0
- RFC 9745, The Deprecation HTTP Response Header Field
- RFC 8594, The Sunset HTTP Header Field
