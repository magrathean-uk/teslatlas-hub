# Fleet boundary

Status: design only. No Fleet OAuth, credential rotation, callback, regional
registration, or Fleet Telemetry implementation exists yet.

Fleet is deliberately not enabled by the token-first installer. A Hub owner
can run the native collector with an existing owner token without registering
an application, publishing a callback, or granting a new OAuth scope.

## Before Fleet can be connected

The Hub needs a Tesla Developer application owned by the person operating the
Hub. Tesla requires a registered application, a client ID, a client secret,
and one exact HTTPS callback URL. Fleet Telemetry and signed commands also
need a public key hosted at the Tesla-defined application-domain path and
Fleet registration in every operating region. Those are external application
owner actions; a package install must never invent or register them.

Use the least data scopes for this product: `openid`, `offline_access`,
`vehicle_device_data`, and `vehicle_location` only when the owner enables
location-derived history. Do not request command or charging-command scopes
for telemetry transfer.

## Authorization contract

When Fleet is enabled, Hub will use Tesla's documented third-party
`authorization_code` flow:

1. Generate a fresh high-entropy `state`; create the Tesla authorization URL
   with the registered client ID, exact registered HTTPS callback, and chosen
   scopes.
2. Accept one matching callback only. Reject missing, expired, replayed, or
   state-mismatched callbacks before an OAuth request is made.
3. Exchange the returned code server-side with
   `https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token`. The client
   secret is a host-encrypted systemd credential; it never enters a config
   file, command argument, environment value, log, or Hub database.
4. Treat both access and refresh tokens as credentials. Persist a replacement
   refresh token before using it again: Tesla refresh tokens are single-use,
   with only a short recovery window. Revoke and local credential removal are
   explicit owner actions.
5. Use Fleet Telemetry for ongoing data. Do not turn the `vehicle_data`
   endpoint into a regular poller. Commands, virtual-key pairing, and charging
   controls remain separate, opt-in work.

An owner can revoke third-party consent in Tesla's consent-management page.
Hub must then stop Fleet collection, clear its host-encrypted Fleet
credentials, and require a new authorization before resuming.

## Sources

- [Tesla third-party token flow](https://developer.tesla.com/docs/fleet-api/authentication/third-party-tokens)
- [Tesla Fleet authentication and scope reference](https://developer.tesla.com/docs/fleet-api/authentication/overview)
- [Tesla Fleet onboarding and public-key requirements](https://developer.tesla.com/docs/fleet-api/getting-started/what-is-fleet-api)
- [Tesla Fleet collection guidance](https://developer.tesla.com/docs/fleet-api/getting-started/best-practices)
