# Versioning, discovery, and capabilities

This document defines how the retained contracts identify and version
themselves: the current-Hub HTTP profile `hub-http-v1@1.2.0` and the Hub sync
profiles `hub-sync-v1@1.3.0` through `1.5.0`. The earlier `hub-http-v1` `1.0.0`
and `1.1.0` profiles, the earlier sync profiles and the root `teslatlas-v1`
contract with its version-negotiation headers were retired and are no longer in
this package. See [`current-hub.md`](current-hub.md) and
[`hub-sync.md`](hub-sync.md).

## Discovery

A Hub publishes `GET /.well-known/teslatlas-hub` over HTTPS and returns the
document defined by `profiles/hub-http-v1/1.2.0/discovery.schema.json` as
`application/json`. The document is public metadata: it contains a stable Hub
identity, protocol support, the `profile` identity the Hub is held to,
capabilities, and attributions. It must not contain credentials, provider
tokens, vehicle identifiers, VINs, or user data.

The advertised `profile` member is the profile identity, for example
`hub-http-v1@1.2.0`. A client binds its expectations to that identity and not
to the Hub's product version: the `version` member is the product version and
is not a wire revision.

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

The current-Hub HTTP profile is `hub-http-v1@1.2.0`. Its additive policy is
part of the profile: clients ignore unknown object fields and unknown
capability tokens, and servers may add optional fields and tokens within 1.x.
Adding an optional field, token or route is a new minor profile. Removing or
retyping a field, changing an enum value, or changing a route's status set is a
new major profile.

A profile is a directory, and its identity is bound to the bytes of that
directory by its `SHA256SUMS` file. The checksum file lists exactly the files of
the profile, and the profile's generator and the conformance runner both refuse
a directory whose files differ from it. A changed byte is therefore a new
profile, never a silent edit of an existing one. Distribute and bind the
complete directory.

The Hub sync profiles follow the same rule and are each bound by their own
`SHA256SUMS`: `hub-sync-v1@1.3.0` defines changes-since, no-op, key discovery
and signed prepared map months, `hub-sync-v1@1.4.0` adds schema 2.2 PhysicalV3
changed sets and typed delta packs, and the candidate `hub-sync-v1@1.5.0` adds
a live overlay. These are protocol profiles, not claims that a particular Hub
or SDK release has shipped.

## Capabilities

Clients use only capabilities advertised by discovery. A capability token
matches `^[a-z0-9.]{1,64}$`, and a Hub advertises a token only when it serves
the routes behind it. Capability tokens are additive within a major profile
version.

Unknown fields and unknown capabilities are ignored unless a schema explicitly
forbids them.

## Deprecation and removal

A stable capability, route or field is removed only by a new major profile;
until then it keeps its meaning. A profile retired from this package is deleted
from it. Text that still names a retired identity either records the
predecessor of a retained, digest-bound profile or states that the identity was
retired.

## Normative references

- RFC 8615, Well-Known Uniform Resource Identifiers
- Semantic Versioning 2.0.0
