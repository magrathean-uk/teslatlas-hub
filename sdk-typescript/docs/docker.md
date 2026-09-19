# Docker setup

The repository contains one small Dockerfile with separate `test` and
`consumer` targets. It is a local build and test workflow for the private SDK;
it does not publish an image or start a Hub service.

The image identity is pinned to the official `node:26.7.0-bookworm-slim`
image. Each target installs and checks npm `11.19.0`. The `test` target also
installs the Playwright Chromium browser and its Debian dependencies because
the existing unit and conformance suite includes browser launches.

Build the targets from the SDK root:

```bash
docker build --target test -t teslatlas-sdk-test .
docker build --target consumer -t teslatlas-sdk-consumer .
```

The test target runs `npm run verify`, including the package, Node fixture and
Chromium checks. A green local image is still source and fixture evidence; it
does not prove an installed Hub service, trusted TLS, or browser CORS policy.

The consumer target contains only the five source files in `examples/hub/`, a
freshly packed SDK, and its production dependencies. It runs as the non-root
image user by default. Match the container UID and GID to a private input
directory when using mode `0700` directories and mode `0600` files:

```bash
docker run --rm --user "$(id -u):$(id -g)" \
  --mount type=bind,src="$PWD/private",dst=/run/teslatlas,readonly \
  -e NODE_EXTRA_CA_CERTS=/run/teslatlas/ca.pem \
  teslatlas-sdk-consumer \
  --endpoint "$HUB_ENDPOINT" --hub-id "$HUB_ID" \
  --credential-file /run/teslatlas/credential.json
```

Use `--invitation-file /run/teslatlas/invitation.json` for a fresh pair. The
credential and invitation files remain private inputs and are never copied into
an image layer. To persist a newly claimed credential, mount a separate private
writable directory and pass `--credential-out
/run/teslatlas-output/new-credential.json`.

Container `localhost` refers to the container. Use a routable Hub hostname,
Docker Desktop host access, or a documented Linux host-gateway mapping. The
server certificate must cover that hostname, and `NODE_EXTRA_CA_CERTS` is only
needed when the CA is absent from the image trust store. The Node consumer does
not need a published container port or a database volume.

The browser example remains a separately served origin. Hub must allow its
exact scheme, host, and port and the browser must trust the endpoint
certificate. Provision its endpoint- and Hub-bound credential envelope through
the pin-capable Node path or a trusted native bridge; ordinary browser Fetch
cannot enforce the invitation's raw leaf pin and therefore does not claim directly. Docker does
not remove those CORS and TLS requirements.
