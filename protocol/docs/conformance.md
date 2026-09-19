# Conformance

The conformance suite is a data-driven public behaviour contract. Any server,
SDK, proxy, or test double can participate by implementing one JSON Lines
adapter. No Hub Rust or proprietary application source is required.

Conformance is black-box: cases observe only the adapter's public requests,
responses, state transitions, and SSE sequence. Passing proves the documented
public behaviour for the exercised cases; it does not prove code similarity,
security, product readiness, or correctness outside the tested contract.

## Run

Run all supported profiles against the bundled harness self-test adapter:

```sh
./conformance/run
```

Run an implementation adapter:

```sh
./conformance/run --adapter /absolute/path/to/adapter
```

Select profiles or request one machine-readable result:

```sh
./conformance/run --profile 1.2.0 --adapter ./adapter --json
```

The current gate executes 31 profile/case runs: nine for `1.0.0`, ten for
`1.1.0`, and twelve for `1.2.0`. A profile contains every case whose
`introduced_in` version is not newer than that profile.

## Current-Hub acceptance

`hub-http-v1@1.0.0` is a separate current-Hub profile. Run it by itself with
the actual-Hub adapter and a private descriptor supplied by the Hub owner:

```sh
TESLATLAS_HUB_HTTP_CONFIG=/absolute/private/ready-or-matrix.json
./conformance/run \
  --profile hub-http-v1@1.0.0 \
  --adapter "$PWD/conformance/adapters/actual-hub" \
  --config "$TESLATLAS_HUB_HTTP_CONFIG" \
  --json
```

The descriptor is either a native fixture `ready.json` or a matrix descriptor
with `kind: "protocol-actual-hub-matrix"`. Both bind the profile manifest and
owned fixture artifacts. Matrix descriptors additionally bind selected Hub and
Protocol source identities, runtime details, and an owner-controlled
`host_session`; native descriptors bind the launcher's retained `ready.json`
and live process identity. The runner rejects arbitrary endpoints, missing
prerequisites, mixed rich/current profile runs, and a different adapter.

Native mode uses the existing bounded `urllib` transport. It loads the owner's
CA file, checks the invitation's configured certificate DER digest before the
first request, and lets the normal TLS context perform CA and hostname
validation. The fixture launcher owns the process and cleanup; native mode does
not provide matrix broker receipts or host-session admission.

Matrix mode uses the stricter raw `http.client` transport. It validates CA and
hostname, checks the connected socket's leaf DER digest before request bytes,
and applies one deadline across connect, TLS, sends, response parsing, and
validation. The installed controller supplies the host-session proof and owns
service lifecycle. These are distinct evidence paths; passing one does not
substitute for the other.

The installed Protocol branch accepts the reviewed v2 wrapper around the
existing matrix config. Hub supplies a closed `SessionInput` file with the
18-member profile staging, certificate DER binding, actor input manifest,
output reservations, bounds, and the unchanged Unix broker descriptor. The
adapter preserves the v1 normalized header and 21 case records, writes
hash-bound raw case files and `actor_evidence`, then writes
`adapter-completion.json` and `ready-000001.json`. It keeps the broker attached
until the Hub runner writes an identity-matched `accepted/close_completed`
acknowledgement; only then may it close and exit zero. Missing or stale files,
changed hashes, a wrong session/nonce, a rejected acknowledgement, or an
unavailable contract fails closed. The inert case manifest and pure semantic
predicate are [`tools/matrix-contract.json`](../tools/matrix-contract.json) and
[`tools/matrix_contract.py`](../tools/matrix_contract.py); they describe the
adapter-owned contract and do not themselves establish an installed row.

Native mode exercises discovery, readiness, claim/replay, vehicle/current
reads, bounded drive pages and conditional `304`, cursor binding, fixture
advancement, and credential rotation. Matrix mode adds the installed lifecycle
cases: wrong or expired invitations, revoke and re-pair, restart, outage
recovery, and unsupported-operation zero-request refusal. The matrix's expired
invitation case intentionally proves local refusal without sending an HTTP
request; a server-side expiry response requires a separate disposable
invitation and raw request.

The current-Hub run is live or synthetic network evidence only when the
descriptor names an owned running Hub. `./conformance/run` with the reference
adapter, generated-artifact checks, or a unit-test stub is local contract
evidence and cannot establish installed-product acceptance. Installed support
requires the Hub-owned runner receipts and cleanup for each declared target.

## Adapter protocol

For each profile/case pair, the runner starts a fresh adapter process with the
repository root as its working directory. It writes one compact JSON object per
line to stdin:

```json
{"protocol":"teslatlas-conformance/1","profile":"1.2.0","case_id":"etag-conditional-get","step_id":"initial","capability":"query.vehicles","request":{"method":"GET","path":"/v1/vehicles/vehicle_demo_alpha/current","headers":{},"query":{}}}
```

The adapter performs that request against its subject and writes exactly one
correlated JSON object to stdout:

```json
{"case_id":"etag-conditional-get","step_id":"initial","response":{"status":200,"headers":{"ETag":"\"bY8nJ4qW2sF7kL5p\""},"body":{}}}
```

Rules:

- stdout is reserved for one JSON response line per request line;
- diagnostics go to stderr;
- preserve response header values as strings;
- omit `body` for bodyless responses;
- return decoded JSON in `body`;
- normalize SSE messages into `events` entries containing `id`, `event`, and
  decoded `data`;
- keep process state between steps because later requests may reference earlier
  ETags, cursors, event IDs, and jobs;
- exit nonzero for adapter infrastructure failure.

Python files run with the runner's interpreter. Other adapter paths must be
executable. The adapter language is otherwise unrestricted. A process retains
state between steps in one case, then exits; state never leaks into another
case or profile. Each response and process shutdown has a bounded timeout,
overridable with `--timeout-seconds`.

Adapter JSON is strict: `NaN` and infinities are rejected. Every response
envelope is schema-validated before expectations run. The runner reads stdout
with a deadline while draining bounded stderr concurrently, so a stalled or
noisy adapter produces a structured failure instead of hanging the gate.

## Case data

`conformance/cases/*.json` contains requests, expectations, and deterministic
reference responses. The runner validates cases against
`schemas/conformance.schema.json`, resolves `${profile}` and prior-step
references, then checks status, headers, exact schema definitions, normalized
events, and assertions. JSON GET and `304` responses also receive cross-case
checks for ETag, cache metadata, representation type, and selected protocol
headers.

Headers beginning `Teslatlas-Conformance-` select deterministic test scenarios
inside an adapter. They exist only at the adapter boundary. They are not public
Teslatlas HTTP headers and a production server must not expose them.
`Teslatlas-Conformance-Principal` asks the adapter to use a separately
provisioned test principal; an adapter maps it to credentials locally and never
forwards the conformance header to its subject.

`conformance/adapters/reference_adapter.py` replays the embedded reference
responses to prove the runner and vectors are internally consistent. Passing
that adapter is not evidence that a Hub, SDK, or client conforms. Product proof
requires an independently implemented adapter targeting that product.

## Compatibility policy

`compatibility/manifest.json` names the current profile and the preceding two
minor profiles. Each profile lists capabilities and its complete case set.
Adding a case requires an `introduced_in` version; regenerating profiles adds it
to that version and later profiles without rewriting earlier semantics.

A release gate passes only when artifact validation, every unit test, and every
selected adapter case pass. One failed step fails its case and yields a nonzero
exit. The `--json` summary is stable machine input for external build systems;
this repository does not require hosted CI.
