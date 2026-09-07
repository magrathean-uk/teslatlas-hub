# Protocol dependency gate

## Historical gate

The original transport-foundation work did not consume resource contracts. Its
dependency gate recorded the public artifacts required before a typed client
could be built.

## Current local gate

The SDK now vendors the approved protocol inputs at commit
`79ced4c7fdc79520ad31d72a0280bf5f3f19f407`, with richer profile `1.2.0` and
supported revisions `1.0.0`, `1.1.0`, and `1.2.0`. `protocol/lock.json` is the
single manifest for that immutable source identity, the richer profile, the
separate `hub-http-v1@1.0.0` candidate, every input hash, and the aggregate
generated-output hash. The current-Hub profile hashes remain null until its
content-addressed export exists. Local checks are:

```bash
npm run protocol:check
npm run protocol:generate
npm run test:protocol
```

The snapshot is a reproducible client input, not a second authority. Generated
types and validators are private package implementation details behind the
closed client API.

Normal `protocol:sync` reads the complete selected input set and bytes from the
manifest-pinned Git commit object, so dirty tracked files and matching untracked
files in the checkout cannot be labelled as committed source. Reading working
tree bytes requires the explicit `--candidate` mode; that mode records the base
commit plus a content digest and must pass the same offline verification gate.

## Remaining external evidence

The local suite proves the SDK's handling of vendored cases. It does not replace
an independently authorized live integration exercise. A publication decision,
including any release-specific review and distribution steps, also remains
outside this private repository gate.
