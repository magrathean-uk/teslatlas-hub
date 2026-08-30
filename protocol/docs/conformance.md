# Conformance

The conformance suite is a data-driven public behaviour contract. Any server,
SDK, proxy, or test double can participate by implementing one JSON Lines
adapter. No Hub Rust or proprietary application source is required.

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

The current gate executes 28 profile/case runs: eight for `1.0.0`, nine for
`1.1.0`, and eleven for `1.2.0`. A profile contains every case whose
`introduced_in` version is not newer than that profile.

## Adapter protocol

The runner starts one adapter process with the repository root as its working
directory. It writes one compact JSON object per line to stdin:

```json
{"protocol":"teslatlas-conformance/1","profile":"1.2.0","case_id":"etag-conditional-get","step_id":"initial","capability":"query.vehicles","request":{"method":"GET","path":"/v1/vehicles/vehicle_demo_alpha/current","headers":{},"query":{}}}
```

The adapter performs that request against its subject and writes exactly one
correlated JSON object to stdout:

```json
{"case_id":"etag-conditional-get","step_id":"initial","response":{"status":200,"headers":{"ETag":"\"opaque-r42\""},"body":{}}}
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
executable. The adapter language is otherwise unrestricted.

## Case data

`conformance/cases/*.json` contains requests, expectations, and deterministic
reference responses. The runner validates cases against
`schemas/conformance.schema.json`, resolves `${profile}` and prior-step
references, then checks status, headers, schemas, normalized events, and
assertions.

Headers beginning `Teslatlas-Conformance-` select deterministic test scenarios
inside an adapter. They exist only at the adapter boundary. They are not public
Teslatlas HTTP headers and a production server must not expose them.

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
