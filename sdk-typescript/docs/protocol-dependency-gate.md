# Protocol dependency gate

## Historical gate

The original transport-foundation work did not consume resource contracts. Its
dependency gate recorded the public artifacts required before a typed client
could be built.

## Current local gate

The SDK now vendors the approved protocol inputs from base commit
`05225bd5b2f56885025180d68fedf3a42baaa90b`, with richer profile `1.2.0` and
supported revisions `1.0.0`, `1.1.0`, and `1.2.0`. `protocol/lock.json` is the
single manifest for that immutable source identity, the richer profile, the
separate `hub-http-v1@1.0.0` binding, every input hash, and the aggregate
generated-output hash. Local checks are:

```bash
npm run protocol:check
npm run protocol:generate
npm run test:protocol
```

The snapshot is a reproducible client input, not a second authority. Generated
types and validators are private package implementation details behind the
closed client API. The current-Hub profile is fully hash-bound in the
lock: input `68dca68355cd1011c31004f7d39eaabfef068706fb89ef0a5823496f26d3727e`,
generated output `3769abc73e927d20cd5842eed3a076b05bf436d1eba3026e324b5d5dfbe7c6b9`,
and bundle `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`.

A committed lock can be refreshed with `protocol:sync`, which reads the complete
selected input set from the manifest-pinned Git commit object. The current lock
records the base commit plus a content digest; its reviewed working-tree bytes
were imported through the tool's explicit working-tree option, `--candidate`.
That option name describes the import source; the resulting compatibility
record is now accepted. Both forms must pass the same offline verification
gate.

## Remaining external evidence

The local suite proves the SDK's handling of vendored cases. G4 separately
accepted the exact packed `@teslatlas/sdk@2026.36.2` archive on Node 26.7.0 and
Chrome 153 against a source-built synthetic Hub on macOS 27 Apple silicon,
including normal TLS, CORS, claim/replay rejection, two current results,
`2/2/1` drive pagination, `304`/cursor behavior, rotation, and scoped trust
removal. [G3 r2](../../teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json)
then admitted the exact compatibility record and verified the vendored
Protocol copy byte-for-byte (receipt SHA-256
`5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`).

These gates do not accept registry publication, an installed Hub service, App
integration, real Tesla data, the declared macOS 13 Hub floor, or the full
platform matrix. x86/amd64/Intel rows remain outside the active scope. No source
publication or tag is implied.
