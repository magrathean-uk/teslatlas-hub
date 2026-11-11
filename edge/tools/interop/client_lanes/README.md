# Edge installed matrix coordinator

`edge.py` is the Edge-owned coordinator for the reviewed `edge_v2` installed
matrix lane. The shared runner invokes one pinned Python interpreter with one
argument:

```text
/absolute/pinned/python3 tools/interop/client_lanes/edge.py /private/session-input.json
```

The session input is the common closed `matrix-adapter-session` object. The
coordinator accepts only `adapter_id == client_id == edge_v2`, the three
registered Edge cells, the six fixed actors and the exact contract files under
`edge_contract/`. Product identity is bound to the staged `edge_executable`
and its installed member manifest; the executable path is observed from that
root-owned product input and is never selected from an argument, environment
variable or worker message.

The coordinator owns one Unix broker attachment. It executes the phase recipes
from `phases.json` in ordinal order using only the existing `verify`, `stop`,
`start`, `pair` and `revoke` operations. Root-launched workers publish raw
evidence below the deterministic paths
`<coordination_dir>/actors/<actor_id>/phase-<ordinal as six digits>-<schema_id>.json`.
Each file has the closed common envelope and one of the six raw schemas. No
worker may provide a command, target, path, fault point or alternate phase.

After every phase and raw schema has been bound, the coordinator writes the
normalized v1 receipt, actor evidence and `adapter-completion.json` with
exclusive owner-only files. It then writes `ready-000001.json` and waits for
the runner's matching `ack-000001.json`. It exits successfully only after an
`accepted`/`close_completed` acknowledgement whose close evidence says the
runner stopped and cleaned the owned resources. A malformed, stale, changed or
premature acknowledgement fails closed.

This publication supplies the Edge contract and launcher. It does not claim an
installed host, Docker runtime, service-manager lifecycle, vehicle receiver
wire or target matrix result; those observations remain root/Hub-owned gates.

## Synthetic receiver identity

`synthetic_vehicle_identity.py` creates a fresh, isolated client CA and leaf
for a synthetic VIN. The CA common name is the pinned receiver's recognized
`Tesla Motors Products CA`; the leaf common name is the VIN and carries
`CA:FALSE`, digital-signature/key-encipherment and TLS client-auth extensions.
Only the CA certificate, client certificate and owner-only client key leave
the private staging directory; the CA private key is not returned.

`test_pinned_receiver_identity.py` copies only the exact pinned
`messages/identity.go` verifier into a temporary Go module and tests the
generated certificate plus wrong-issuer and missing-identity-OID rejection.
It is source/application evidence only and does not start a receiver or open
a network connection. The focused generator test is run from the repository
root with its sibling module path supplied explicitly:

```text
PYTHONPATH=tools/interop/client_lanes /absolute/pinned/python3 \
  -m unittest tools/interop/client_lanes/test_synthetic_vehicle_identity.py
```

The pinned-verifier harness is invoked as:

```text
/absolute/pinned/python3 tools/interop/client_lanes/test_pinned_receiver_identity.py \
  --pinned-source /absolute/path/to/fleet-telemetry
```

## Preparing Hub consumer inputs

`prepare_hub_consumer_inputs.sh` is the Edge-owned, source-only handoff helper
for a fresh Hub consumer bundle. It requires explicit paths for the issuing
Edge server CA, the Edge server certificate, the exact expected Edge server
IPv4 address, the separate Hub client CA and client certificate/key, the
bearer, and a new output directory. It validates the CA constraints, server
`sslserver` chain and exactly one matching IP SAN, client `sslclient` chain,
certificate/key match, and bearer byte bounds before writing `server-ca.pem`,
`hub-client.crt`, `hub-client.key`, and `delivery-bearer` as 0600 files below a
0700 directory. A wildcard, loopback, or other server IP SAN is rejected when
the expected address is non-loopback; the expected address is always explicit,
so the legacy loopback lane remains testable without being silently reused for
a non-loopback target. The bearer must be a complete Edge-issued
`tte1.<uuid>.<base64url-secret>` credential token, rather than an arbitrary
owner-only ASCII value. Existing output paths and leaf-as-CA input are rejected.
The helper does not start services, open listeners, send requests, or consume a
runtime fixture.
