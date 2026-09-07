# Packed Node and Chromium actual lanes

Run the fixed entrypoint with an explicit Node executable and a private descriptor:

```sh
/absolute/pinned/node /absolute/workspace/hub/tools/interop/client_lanes/run.mjs /private/lane.json
```

This implementation owns a **fresh native user-process Hub**. It does not install
packages or control an existing service. `installed_service_runtime` remains
pending. The final installed three-target matrix requires its separate observed
service supervisor integration; a caller `service_mode` or `verified` boolean is
not accepted by this descriptor. Exit 0 means the exercised interim cases passed;
the receipt still contains the pending installed case. Exit 1 means a failed
case, prerequisite or teardown. A cleanup failure leaves a private `.failure.json`
and `.cleanup.json` instead of a successful normalized receipt. The matrix's `--require-complete` must remain nonzero for
this interim receipt.

The descriptor and fixture config are owner-only regular JSON files outside all
workspace source. Output paths must be absent under an owner-only private parent.
No bearer or invitation belongs in argv, environment, descriptor literals or logs.
The descriptor has exactly these fields (`browser` only for browser mode):

```json
{
  "mode": "node",
  "fixture_config": "/private/fresh-fixture.json",
  "package_root": "/private/install/node_modules/@teslatlas/sdk",
  "tarball": "/private/accepted-sdk.tgz",
  "tarball_sha256": "expected SHA256",
  "node_sha256": "expected SHA256",
  "hub_sha256": "expected SHA256",
  "seed_sha256": "expected SHA256",
  "evidence_path": "/private/absent-receipt.json",
  "python": "/absolute/python3.11-or-later"
}
```

`fixture_config` uses the existing read-only `../fixture.py` config contract:
`binary`, `seed_binary`, `output_dir`, `port`, `lifetime_seconds`, `profile_id`,
`profile_path`, `profile_sha256`, and `allowed_origins`. Use current profile
`hub-http-v1@1.0.0`, a fresh output directory, port 18480, and browser origin
`http://localhost:18481`. Inputs are exact accepted artifacts; this command
never builds or repacks the SDK or Hub. It checks all 80 installed SDK members
against the accepted tar and imports its installed `dist/node.js` or serves its
installed `dist/browser.js` to Chromium. It uses the existing SDK artifact
verification helper as a read-only harness dependency, never the SDK source APIs.

The independent expected values are read from `hub/tests/interop/scenario.json`;
they are not derived from seed implementation or successful response bodies.
The private supervisor uses the supported stopped-Hub `pair` and
`control revoke-device` CLI. It retains the same config, TLS identity and data
store across restarts. Each start and each secret-bearing scenario admission
uses the existing strict OS executable/UID/parent/start witness. Cleanup owns
only child processes it created, including failures before readiness.

Node uses unmodified default fetch and independent Undici diagnostic channels.
Node 26 may publish parsed object headers or raw byte arrays; both are decoded.
Chromium uses unmodified browser fetch, with CDP Network request/response facts.
The observed transport counters surround local expired-invitation and cursor
binding rejection, and the absent current-Hub `charges` operation. That operation
is absent from the production API; its actual JavaScript invocation raises
TypeError and sends no request. No capability response or fetch implementation
is modified to force this result. HTTP transcripts preserve method, pathname,
status and the server's request ID; query cursor values and auth headers are not
retained. The `.transport.json` sidecar retains bounded lifecycle and browser
trust/process evidence. HTTP rejection assertions retain both the typed error and
independently required HTTP status: authentication 401, unknown vehicle 404.
An empty-body 500 cannot pass. Node/browser runtime normalization uses the lane
mode and observed architecture/version, including Linux Node. No historical receipt is imported.

For the supported isolated Linux Chromium host, add:

```json
{
  "browser": {
    "ssh_config": "/private/isolated-host/ssh.config",
    "ssh_alias": "isolated-host",
    "remote_root": "/tmp/teslatlas-client-lanes-unique-run",
    "playwright_entry": "/absolute/installed/playwright/index.mjs",
    "local_log": "/private/absent-browser-ssh.log"
  }
}
```

Only a new launcher script and fresh owned directory are created on that host.
It needs Python 3, `/usr/bin/chromium`, NSS `certutil`, and available loopback
ports 18480, 18481, 18483, 18484 and 18489. The SSH reverse forwards start only
after the native Hub listener exists. Trusted and untrusted Chromium use
separate new HOME/NSS/profile directories. Only the trusted NSS database receives
the fixture CA; export digest and live CDP launch arguments are verified, and
untrusted navigation must fail with `ERR_CERT_AUTHORITY_INVALID`. Certificate
bypass flags are forbidden. A closed loopback proxy blocks non-loopback browser
traffic while the actual Hub and local page bypass it; QUIC/non-proxied WebRTC
are disabled. The owner's browser and existing services/stores are never used.
The launcher attempts both Chromium process groups independently and writes its
private `cleanup.json`, including errors and rescue outcomes. The client retrieves
that record through a fresh SSH verification after the forward owner exits,
checks live group members and all four remote listener ports, and closes its
local page server and forwards. Browser-close rejection cannot skip later
resources. The Hub supervisor has a separate stopped record; its exit must be
zero without signal/escalation, with the bound final Hub PID stopped and listener
closed. All required cleanup completes and is admitted before receipt creation. The
candidate process witness binds the exact admitted cleanup document SHA-256. Keep these private evidence directories for review.

Focused harness checks:

```sh
/absolute/pinned/node --test hub/tools/interop/client_lanes/test_contract.mjs hub/tools/interop/client_lanes/test_cleanup.mjs
PYTHONDONTWRITEBYTECODE=1 /absolute/python3.14 -m unittest discover -s hub/tools/interop/client_lanes -p 'test_*.py' -v
```

These cover harness errors, not product acceptance. Actual acceptance requires
fresh real lane execution and independent expected/actual assertions. The current
source module can be run on another native supported Hub host after provisioning
its exact accepted executables and trusted browser forwarding there. Installed
service identity and complete final matrix integration remain explicitly pending.
