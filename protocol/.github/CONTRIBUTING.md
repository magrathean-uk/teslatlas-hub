# Contributing

Teslatlas Protocol owns public contracts and their local validation tools. Keep
changes implementable by an independent client using only this repository.
Start with the [architecture](../docs/architecture/overview.md) and the profile
you intend to change.

## Make a contract change

1. Identify the profile family and revision. Rich semantic HTTP, current-Hub
   HTTP, Hub sync, and Edge delivery have separate identities.
2. Update the canonical definition and its generator where applicable, then
   update affected schemas, OpenAPI, examples, fixtures, compatibility
   profiles, conformance cases, and tests together.
3. Preserve published wire spellings, capability negotiation, opaque cursors,
   UTC timestamps, validators, and stable errors. Follow the
   [versioning policy](../docs/reference/versioning.md) before changing
   compatibility.
4. Use synthetic, deterministic examples. Exclude credentials, real vehicle
   identifiers, precise location data, private descriptors, and response
   captures.
5. Run focused checks for the changed behavior, then the local gate. Describe
   the exact result and any check you could not run.

## Validate locally

Python 3.11 or later and uv are required. From the repository root:

```sh
./tools/check
```

The script synchronizes the locked dependencies, checks generated artifacts,
runs the unit tests, and invokes the reference conformance adapter. For a
focused check, use the relevant test module, for example:

```sh
uv run python -m unittest tests.test_hub_sync_profile -v
uv run python tools/build_hub_sync_profile.py --check
```

Generator `--check` modes compare committed output without replacing it. A
contract change may require running the matching generator without `--check`
before reviewing the resulting diff. See [conformance](../docs/guides/conformance.md)
for implementation adapters and [verification](../docs/reference/verification.md)
for the limits of each kind of evidence.

GitHub is source storage for this project. Do not add hosted CI, build or test
workflows, release automation, or binary publication as part of a contribution.

## Submit a useful change

Keep the change focused. Explain the affected profile, before and after
behaviour, compatibility impact, and validation performed. Documentation-only
changes should check paths, links, examples, and consistency with the
machine-readable contract. Avoid claims of deployed support based on a local
check.

Preserve the [Apache 2.0 licence](../LICENSE) and
[third-party notices](../docs/legal/third-party-notices.md). Do not copy Hub
implementation code or proprietary application code into this repository.
Record the origin and licence of any third-party material you propose to add.

Use [support guidance](SUPPORT.md) for questions and
[security guidance](SECURITY.md) for sensitive reports.
