# MAC-4 runtime handoff

This is the prepared ordinary-user journey for the existing
`teslatlas-debian13-arm64` guest and a native Mac Hub. During the current
coordinated MAC-4 execution the runtime owner has started that existing guest and
established the private forward described below. This handoff does not authorize
another guest, public ingress, real Tesla access, Keychain trust changes, signing,
or changes outside the named synthetic runtimes.

## Fixed private route and TLS identity

Use this topology for the first MAC-4 run:

- Native Hub listens only on Mac loopback at `https://127.0.0.1:21444`.
- Its server leaf has `subjectAltName = IP:127.0.0.1`, server-auth EKU, and a
  fresh cohort identity. The invitation's lowercase SHA-256 leaf pin must match.
- `tools/mac-hub-forward` opens an SSH remote forward from guest
  `127.0.0.1:18443` to Mac `127.0.0.1:21444`. It does not start the guest or Hub.
- Home Assistant Container uses host networking, so guest
  `https://127.0.0.1:18443` is reachable from the Container. This explicit
  tunnel is what makes guest loopback reach the Mac service; without it,
  guest localhost is not the Mac.
- No wildcard listener, LAN bind, public ingress, disabled verification, or
  global trust change is permitted. The Mac login Keychain must not be read or
  modified for this journey.

The Hub owner must hand off: Hub source/profile identity; config, state and log
paths; start/status/stop commands; confirmed loopback port; server leaf and CA
fingerprints; an owner-readable CA PEM; the one-use invitation; and a redacted
request census. Private paths and values stay outside receipts.

After the runtime owner confirms the existing guest is owned, stopped/started as
expected, and its SSH engine and port are ready, start the route on the Mac:

```sh
TESLATLAS_MAC_HUB_PORT=21444 \
TESLATLAS_GUEST_HUB_PORT=18443 \
tools/mac-hub-forward
```

Before Home Assistant starts, the guest owner must require port 18443 to have
been free, then prove the forwarded certificate and identity:

```sh
openssl s_client \
  -connect 127.0.0.1:18443 \
  -verify_ip 127.0.0.1 \
  -verify_return_error \
  -CAfile /PRIVATE/PATH/hub-ca.pem </dev/null
```

The selected HA Container retains `network_mode: host`. Build a private CA bundle
inside its existing `/config` owner-controlled tree from the guest system CA
bundle plus the fresh Hub CA. Set both `SSL_CERT_FILE` and
`REQUESTS_CA_BUNDLE` to that same combined file: Home Assistant 2026.8 creates
its managed client context from `REQUESTS_CA_BUNDLE`, while command-line probes
may use `SSL_CERT_FILE`. Keep the component mount read-only. Do not replace the
guest trust store or set an insecure SSL option. The runtime handoff must record
the exact Container image ID, component tree hash, bundle hash, and permissions
without recording private paths.

The retained current execution uses Home Assistant `2026.8.3` at exact arm64
digest `sha256:14931c6b13756317849f46da1d01b45937a1150db66c081cfe529d48215943fe`.
The current 31-file component is mounted read-only with manifest SHA-256
`e3ce825c87b4cab40372fe381aa3a39bbc7e7e0b86ede7dc168899b9b052d56f`.
Strict TLS health already passes from both guest and Container; this is runtime
preparation, not headed acceptance.

## Headed UI journey

Expose only the existing HA UI to the Mac with `tools/mac-consumer-forward`, then
open `http://127.0.0.1:18123/` in a visible browser. Use Settings -> Devices &
services -> Add integration -> Teslatlas Hub and enter:

- Host: `127.0.0.1`
- Port: `18443`
- Use TLS: enabled
- TLS certificate fingerprint: the invitation's verified lowercase leaf pin
- Pairing ID, pairing secret, and device name from the fresh one-use invitation

Acceptance is a visible successful entry plus the authenticated validation read;
the invitation must then fail replay. Record entry ID, registry IDs and aggregate
counts only. Do not record bearer, invitation, endpoint identity, vehicle identity,
names, locations, or raw state.

Run the following bounded sequence against the same entry and synthetic Hub data:

1. Observe two scheduled 30-second refreshes in the UI and Hub request census.
   Require at most one snapshot in flight and at most four current reads in flight.
2. Show numeric zero as `0`; missing/null fields as `unknown`; a missing current
   projection with entities present but values unknown; Hub outage as
   `unavailable`; and recovery after the same Hub store restarts.
3. Add one synthetic vehicle, then remove it. Require one stable device/entity set,
   no duplicates, removed entities unavailable, and the same registry IDs when it
   returns.
4. Revoke only this synthetic HA device credential. Complete the visible reauth
   prompt with a fresh invitation. Require the same config entry and registry IDs,
   and no stored update until the new bearer completes an authenticated read.
5. Reconfigure the same entry by running the forward with guest port `18444`
   (still targeting Mac port `21444`) and changing only the UI port to `18444`.
   Require the same Hub identity and an authenticated read before the endpoint
   update; then return to the fixed `18443` route through the same flow.
6. Exercise Reload, Disable, Enable, HA process restart, and Container stop/start.
   Require one entry, the same config/device/entity identities and no duplicate
   refresh authority after each transition.
7. Download diagnostics while healthy and during an outage. Search the serialized
   result for every known endpoint, invitation, bearer, device/Hub/vehicle ID,
   vehicle name, location/coordinate, request ID and raw observation sentinel;
   all must be absent while aggregate availability and counts remain useful.
8. Create one unrelated HA sentinel entry/device/entity before the run. Unload the
   Teslatlas entry, then remove it through the UI. Require zero post-unload Hub
   requests, removal of its owned registry surface, and the unrelated sentinel
   unchanged.

Stop the two SSH forwards after evidence is captured. Runtime cleanup and the
decision to retain or stop the guest belong to the Hub/runtime owner. A green local
suite or this prepared journey is not headed MAC-4 acceptance.
