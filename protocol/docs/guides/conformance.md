# Conformance

The conformance suite is a public behaviour contract for the Hub profiles this
package keeps: the `hub-sync-v1@1.3.0` fixture corpus and the
`hub-http-v1@1.2.0` and `hub-http-v1@1.3.0` offline and native checks (the
`1.3.0` runner, `conformance/hub_http_13.py`, adds nine alias and full-page
cases to the 50 native cases of `1.2.0`, for 59, and the descriptor inputs
`full_page_vehicle_id`, `full_page_drive_id` and `full_page_charge_id`; the
current-Hub section below describes the `1.3.0` run, and the retained `1.2.0`
run is the same with its 50 native cases and without those three inputs). No
Hub Rust or proprietary
application source is required.

Conformance is black-box. Passing proves the documented public behaviour for
the exercised cases; it does not prove code similarity, security, product
readiness, or correctness outside the tested contract.

## Run

Run the default gate:

```sh
./conformance/run
```

The default gate runs the `hub-sync-v1@1.3.0` fixture corpus only: 65 cases.
It prints one line per case and a summary line, or one machine-readable
summary with `--json`:

```sh
./conformance/run --json
```

The summary has the members `profiles`, `runs`, `passed`, `failed` and
`failures`. The result identifies every failing case and its exact fixture IDs.

`hub-sync-v1@1.3.0` can also be selected by name. It runs deterministic fixture
cases, including signed changed-set and no-change paths, multi-chunk
compaction, small and final chunks, schema 2.1 and 2.2 manifests, explicit
bootstrap-profile selection, legacy schema-list-only routing, and empty
no-store rejection of invalid, incomplete, or duplicate explicit selection,
I-JSON unsafe signed-integer rejection, vehicle-bound key discovery,
retained-checkpoint rebases, unknown-receipt errors, prepared artefacts, stable
HTTP errors, exact raw body limits for every JSON control response type, and
signature and ordering negatives. Hub and App can invoke the shared fixture
gate with:

```sh
./conformance/run --profile hub-sync-v1@1.3.0 --json
```

This gate validates the published fixture corpus; product adapters and a
running Hub are separate runtime evidence.

The runner takes no adapter executable, and the named file is never run. For a
default or `hub-sync-v1@1.3.0` invocation, `--adapter` is refused with exit
status 2 and the error `conformance runs use no adapter executable`. For
`hub-http-v1@1.2.0` and `hub-http-v1@1.3.0`, `--adapter` is refused with exit
status 2 and the profile-specific error `<profile id> uses no adapter
executable; supply a private --config descriptor for native cases`. Any
`--profile` other than `hub-sync-v1@1.3.0`, `hub-http-v1@1.2.0` and
`hub-http-v1@1.3.0` is refused with exit status 2 and
`unknown profile: <id>`; the retired profiles `hub-http-v1@1.0.0` and
`hub-http-v1@1.1.0` are among them.

## Current-Hub acceptance

`hub-http-v1@1.3.0` is the current-Hub profile; the retained `hub-http-v1@1.2.0`
is run the same way with its own harness (`conformance/hub_http_12.py`). That
harness holds every served body to the server-strict `1.2.0` schemas, so it
checks a Hub that serves exactly `1.2.0` (a build from before the `1.3.0`
change): against a Hub that serves `1.3.0`, whatever its discovery names, the
body cases fail on the added members and `discovery-profile` fails by design
when discovery names `1.3.0`, and the harness has no tolerant client mode for
a `1.2.0` client reading the `1.3.0` superset. Select one profile by itself; it
cannot be mixed with another profile:

```sh
./conformance/run --profile hub-http-v1@1.3.0 --json
```

Without a descriptor the run executes the offline profile checks only: the
checks of the checked-in profile directory (12 of the 71 runs). The 59 native
cases are reported as `not_run`, so the summary is incomplete (`complete:
false`), and its `provenance` is `offline-profile`. Offline checks are local
contract evidence and say nothing about a running Hub.

To run the native cases as well, pass a private descriptor supplied by the Hub
owner:

```sh
TESLATLAS_HUB_HTTP_CONFIG=/absolute/private/ready.json
./conformance/run \
  --profile hub-http-v1@1.3.0 \
  --config "$TESLATLAS_HUB_HTTP_CONFIG" \
  --timeout-seconds 120 \
  --json
```

The descriptor is a mode-600 JSON file in a private directory, owned by the
user who runs the checks. It is never printed or logged. Its members are
exactly these (`hub_http_13.validate_descriptor`):

- `schema_version`: `1`;
- `kind`: `hub-http-v1-actual-hub`;
- `profile_id`: `hub-http-v1@1.3.0`;
- `endpoint`: the owned loopback HTTPS origin of the Hub under test;
- `certificate_path`: an absolute path to the PEM file used for normal CA and
  hostname validation;
- `tls_pin`: optional, the lowercase SHA-256 of the DER leaf certificate,
  checked on the connected socket before any request is sent;
- `inputs`: the named inputs of the profile's `cases.json` (bearers, vehicle,
  drive and charge identifiers, the invitation path and the fixture update
  paths). A case whose input is missing is `not_run` and names what it needed;
  it never counts as passed.

The descriptor and its inputs are single use. A run consumes the invitation by
claiming it, rotates and revokes the bearers it is given for that purpose, and
asks the owner's harness to append one observation. Repeating a run needs a
fresh invitation, fresh disposable bearers and a fresh fixture; never reuse a
descriptor after its run. This package does not write descriptors: the Hub
owner's harness creates the private descriptor and its inputs outside this
package. The checks only read it.

Exit status of the `hub-http-v1@1.3.0` run:

- 0: every requested check passed (the offline checks, and with `--config`
  every native case);
- 1: a check failed;
- 2: the runner refused the invocation (an adapter, a mixed profile list or an
  unknown profile) or exceeded its total process deadline, and printed an
  error object;
- 3: nothing failed, but a requested check did not run: with `--config`, any
  native case that is `not_run`, or any offline check that is `not_run`.
  A run without `--config` requests no native case, so it ends with 0 when the
  offline checks pass, and its summary still reports `complete: false`.

`--timeout-seconds` bounds the whole run, which executes as one owned process
group; native cases still pending when the budget ends are `not_run`. The
current-Hub run is live or synthetic network evidence only when the descriptor
names an owned running Hub. Generated-artifact checks and unit-test stubs are
local contract evidence and cannot establish installed-product acceptance.
Installed support requires the Hub-owned runner receipts and cleanup for each
declared target.

## Release gate

A release gate passes only when every retained generator reproduces its profile,
every unit test passes, and the selected fixture cases pass. One failed step
fails its case and yields a nonzero exit. The `--json` summary is stable
machine input for external build systems; this repository does not require
hosted CI.
