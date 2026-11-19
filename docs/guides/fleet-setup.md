# Fleet API setup

This guide covers direct Fleet polling with your own Tesla developer
application. The Hub app walks you through it: prerequisites, public key file,
registration, consent, sign-in and a final page that shows your car and starts
collection. Fleet is a second choice beside the Owner sign-in, which stays the
default. The [collection paths guide](collection-paths.md) compares all paths.

Fleet in this release is **polling only**. Hub asks Tesla for vehicle data at
an interval; it does not configure Fleet Telemetry push, runs no receiver and
needs no inbound connection. It sends no vehicle command (Hub never does, on
any provider). Hub runs no relay and Tesatlas supplies no shared application:
the application, its client secret and its bill are yours.

Hub sends nothing to Tesla outside an open
[collection window](../operations/collection-windows.md). Registration,
sign-in and car discovery each need the window to be Open.

## What you need

- A Tesla Developer application with the `authorization-code` grant, the
  `Vehicle Information` and `Vehicle Location` scopes, a client ID, a client
  secret and one registered redirect URI. Create it at
  <https://developer.tesla.com/dashboard>, or
  <https://developer.tesla.cn/dashboard> for China, with a Tesla account that
  has a verified email address and multi-factor authentication.
- A domain you control, able to serve one static file over HTTPS (step 2).
  Nothing connects to your Hub.
- A payment method and a billing limit on the developer account. Polling is
  billed by Tesla.
- The Tesla account region: North America and Asia-Pacific, Europe, Middle East
  and Africa, or China. Choose the account's region, not the car's present
  location. China uses a separate Tesla.cn account and application.

Hub stores the client ID, region and the tokens it obtains. It uses your client
secret for the registration and the sign-in exchange only, and never stores it.
Tokens are never typed in or pasted: the Hub app and the service exchange the
authorisation code themselves.

## 1. Set up in the Hub app

In the Hub app's onboarding, choose **New installation**, then **Fleet API
(your own Tesla developer app, polling)**. The page lists these prerequisites
as a checklist, then asks for the client ID, the client secret (a hidden
field), the region, the redirect URI and your domain.

The redirect URI must be exactly the address registered on your application.
The Hub app's sign-in window cancels that navigation before it loads, so no
page needs to exist at that address, and the authorisation code never reaches a
web server.

## 2. Save and publish the public key

Choose **Save public key** and pick a folder outside Hub's data folder and
configuration folder. Hub creates its Fleet key pair on first use, keeps the
private key in its own secrets folder (never in a backup) and writes only the
public key to the file you chose.

Publish that file on your domain at exactly:

```text
https://YOUR-DOMAIN/.well-known/appspecific/com.tesla.3p.public-key.pem
```

## 3. Register the application

With the collection window open, choose **Register**. Hub gets a Tesla partner
token with your client ID and secret, registers your domain, then reads back the
key Tesla holds for it. Registration succeeds only when Tesla returns your
domain and the key that Hub made. If Tesla holds a different key, or cannot
fetch one, Hub says so; fix the published file and register again.

## 4. Accept the cost

Before anything that Tesla bills is sent, the app shows a consent page with
this Hub's polling cadence, the price per request and the monthly cap. The
figures come from the running service, not from the page. Nothing is sent until
you accept, and your acceptance is kept as a record (time and figures, no
secret).

Hub enforces its own monthly cap on scheduled polling. The default is 1000
cents (10 US dollars), and polling slows or stops when the cap is reached.
Tesla's current rate is the source of truth for the bill: check the developer
dashboard and keep your own billing limit set there too.

## 5. Sign in with Tesla

With the window open, choose **Sign in with Tesla**. Hub starts one attempt,
valid for 10 minutes, and the app opens Tesla's authorisation page in its own
window. Approve both scopes: vehicle information and vehicle location. The app
checks the returned `state`, then hands the code to the service, which exchanges
it and stores the login. A grant that lacks either scope is refused and nothing
is stored; sign in again and allow both.

The sign-in lists your cars, and Tesla bills that request, which is why the
consent comes first. If the list fails after the login is stored, the app offers
to retry the discovery without signing in again.

## 6. Choose the car and start

The last page shows your car and starts collection. If your account has several
cars, choose one. Starting sets the polling interval (300 seconds if none was
set) and Hub then polls inside an open window only. Nothing is polled before
this step.

If the Hub app or service stops part way, reopen the app: it reads the service
state and continues from the step that is unfinished.

## Check it

```sh
tesatlas-hub --config /absolute/path/config.toml status
tesatlas-hub --config /absolute/path/config.toml doctor
```

`status` shows provider `fleet`, a healthy Fleet credential and the request
counter against the cap. A successful check proves the Hub's own path only; the
latest observation advances while a window is open and the car is awake.

## Not offered

- **Telemetry push.** A configuration with `[collector.fleet_telemetry]` does
  not load, and `configure-fleet-telemetry` is refused.
- **Vehicle commands and the command proxy.** Fleet configuration keys for a
  command proxy do not load, and every vehicle action is refused with
  `vehicle_commands_not_offered`.
- **Stored tokens from elsewhere.** Hub refuses a Fleet login it did not
  obtain itself, and any input with a token-named key. Fleet rows from an
  earlier version are never admitted; sign in again.

## Revoke access

Revoke the application from Tesla Account Security, then sign Hub out
(`tesatlas-hub control sign-out`) to remove its stored credentials locally.

Official references:

- <https://developer.tesla.com/docs/fleet-api/getting-started/what-is-fleet-api>
- <https://developer.tesla.com/docs/fleet-api/getting-started/regions-countries>
- <https://developer.tesla.com/docs/fleet-api/authentication/third-party-tokens>
- <https://developer.tesla.com/docs/fleet-api/authentication/overview>
