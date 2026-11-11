# Edge macOS one-event actor r5 guard correction

This directory is an additive source-only correction to the rejected r4 guard
package. Hub found that r4 checked only the guard before the timestamp, then
created output and spawned the helper before discovering a pre-existing result
or an output/result alias. That could leave a sentinel untouched but still
produce one child invocation with no final result.

The r5 wrapper performs a complete output preflight before the one clock read:
guard, result, output and the derived atomic-result temporary path must be
pairwise distinct, disjoint from every input path, owner-only-parented, and
absent. Existing files and aliases fail closed without a clock read, guard
claim or spawn. The r4 producer source/test pair is unchanged; Hub must retain
the r4 review rejection and inspect the fresh r5 wrapper source/test hashes.

Run the focused local-only tests with:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover \
  -s guard -p 'test_*.py' -v
```

No guest, credentials, PKI, listener, producer, event, network request or
shared runtime was created for this correction.
