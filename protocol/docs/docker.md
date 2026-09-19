# Docker checker

This repository has no server daemon. The image is a small, outbound-only
checker for the existing `hub-http-v1@1.0.0` profile. Its pinned Python 3.13.13
and uv 0.12.9 image references are selected by digest in the
[`Dockerfile`](../Dockerfile). The image installs the locked development group,
then runs with `UV_OFFLINE=1`; the local gate cannot fetch packages at runtime.

Build and run the local, offline Protocol gate from the repository root:

```sh
docker build -t teslatlas-protocol-check .
docker run --rm --network none teslatlas-protocol-check
```

The local gate has passed once on explicit Docker context
`colima-interop-20260905`: a frozen 172-file snapshot built as Linux/arm64 and
completed `./tools/check` with `--network none` (133 tests, exit 0, no OOM).
That is native ARM64 package proof only. AMD64 coverage, a container-to-Hub
smoke, a running Hub, and installed acceptance cells remain unverified.

## Read-only remote wire smoke

`./tools/check-current-hub` uses the image's installed `curl`, not a second
HTTP client. It performs only these GET requests: discovery, vehicles, current,
the first drive page, the same drive page with its ETag, and at most the
configured number of drive pages in total. It supplies the bearer only after
discovery through curl's `--header @file` form. It disables curl's implicit
configuration and proxy use, does not follow redirects or retry, requires CA
and hostname verification, and returns a redacted JSON receipt.

Place these three owner-only files directly in one private directory (mode
`0700`): `probe.json`, `ca.pem`, and `authorization.header`. Every file must
be owned by the invoking user with mode `0600`; `authorization.header` contains
exactly one line such as `Authorization: Bearer …`. The endpoint and both file
paths in `probe.json` must be absolute. The file contains no bearer value.

```json
{
  "endpoint": "https://host.docker.internal:8443",
  "expected_hub_id": "11111111-1111-4111-8111-111111111111",
  "ca_path": "/run/teslatlas/ca.pem",
  "authorization_header_path": "/run/teslatlas/authorization.header",
  "vehicle_id": "22222222-2222-4222-8222-222222222222",
  "from_ms": 0,
  "to_ms": 9223372036854775807,
  "limit": 2,
  "max_pages": 3,
  "timeout_seconds": 30
}
```

`max_pages` is 1 through 10 and `timeout_seconds` is a total 1 through 120
seconds. The probe rejects unknown fields, endpoints with credentials, a path,
query, or fragment, invalid UUIDs and ranges, non-private inputs, redirects,
unexpected HTTP statuses, malformed or oversized responses, repeated cursors,
and failed profile validation. A non-null cursor at the configured page limit
returns `partial` with a nonzero exit; it never claims complete history.

Run the smoke from a host with a separately managed synthetic Hub and a
certificate whose hostname matches the selected URL:

```sh
docker run --rm \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$TESLATLAS_PRIVATE_DIR,dst=/run/teslatlas,readonly" \
  teslatlas-protocol-check ./tools/check-current-hub \
  --config /run/teslatlas/probe.json
```

The checker only needs writable temporary space inside its disposable
container. It does not write the private mount, need a persistent volume, or
publish a port. On Docker Desktop, `host.docker.internal` reaches the host. On
native Linux, add `--add-host host.docker.internal=host-gateway` only when the
Hub listener and certificate are prepared for that route; it cannot expose a
loopback-only Hub. Bind-mount ownership differs by platform, so verify the
observed ownership before using the command.

The smoke assumes Hub has already provisioned its bearer and trusted endpoint.
It does not perform invitation trust or validate `tlsPin`, and it cannot replace
the native adapter's same-host process/controller proof or any installed-Hub
acceptance. No Docker image has been published.
