# Packed Node and Chromium lanes

`run.mjs` exercises the packed TypeScript SDK through a fresh native Hub fixture.
It does not install packages or control an existing service. Invoke it with a
pinned Node executable and a private descriptor:

```sh
/absolute/pinned/node tools/interop/client_lanes/run.mjs /private/lane.json
```

The interim descriptor owns a fresh native-process Hub. Exit 0 means the cases
that ran passed, but the receipt remains incomplete while
`installed_service_runtime` is pending. Keep `--require-complete` nonzero for
that interim receipt.

The final installed lane uses a runner-created v2 descriptor and bound,
owner-only `SessionInput`. It verifies the accepted SDK archive and all 81
installed package members, runs Node or Chromium through the private broker,
records hash-bound evidence, and proves final service and child cleanup. The
installed service identity and complete matrix remain pending until the lane is
run on every required macOS and Debian host with admitted runtime identities.

The descriptor must point to a fresh fixture config, accepted SDK tarball and
checksum, pinned Node checksum, Hub and seed checksums, an absent output path,
and Python 3.11 or later. The fixture uses `hub-http-v1@1.0.0`, fresh private
state, and loopback ports. No bearer or invitation may appear in argv,
environment, descriptor literals, or logs.

For the isolated Linux Chromium host, the optional `browser` object identifies
the SSH configuration, isolated remote root, installed Playwright entrypoint,
and local log path. The runner creates only a fresh private directory there,
uses separate trusted and untrusted Chromium profiles, and rejects certificate
bypass flags. It verifies browser cleanup, listener closure, process identity,
and the separate stopped Hub record before receipt creation.

Focused harness checks are:

```sh
/absolute/pinned/node --test \
  tools/interop/client_lanes/test_contract.mjs \
  tools/interop/client_lanes/test_cleanup.mjs
PYTHONDONTWRITEBYTECODE=1 /absolute/python3.14 -m unittest discover \
  -s tools/interop/client_lanes -p 'test_*.py' -v
```

These checks cover harness errors and cleanup. They do not prove product
acceptance. Keep all descriptors, fixtures, receipts, logs, and raw evidence
outside the repository and owner-only.
