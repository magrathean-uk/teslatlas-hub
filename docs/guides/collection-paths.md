# How Hub gets data from your car

This guide explains the four ways Teslatlas Hub can receive data from a Tesla, what each one needs from you, what it costs, and how to set it up step by step. Hub runs on your own computer; it never sends your vehicle data through Magrathean. Provider modes and the setup screens described here are the v7 design (plan group G2-6); until they ship, the Hub supports the Legacy path and self-hosted Fleet Telemetry.

## The short version

| Path | What you need | Monthly cost | Data quality | Who sees your data |
| --- | --- | --- | --- | --- |
| 1. Legacy (default) | Your Tesla account token pair (from TeslaMate or a token app) | Free | Best resolution (streaming every second while driving) | Tesla and your Hub only |
| 2. Provider | An account with Teslemetry, MyTeslaMate or Tessie | Provider subscription (see below) | Fleet Telemetry, usually 1 to 60 s intervals; provider polling sometimes | Tesla, the provider and your Hub |
| 3. Direct Fleet polling | Your own Tesla developer application, a card on file, a web page you control for the key file | Tesla usage charges, about $0.12 per awake hour at a 60 s interval; a $10 monthly allowance covers light use | Snapshots at the interval you choose | Tesla and your Hub only |
| 4. Self-hosted Fleet Telemetry | Everything in 3 plus a public hostname with a certificate and inbound TCP 443 to your Hub | Tesla streaming charges, roughly $0.007 per awake hour | Fleet Telemetry at the intervals you configure | Tesla and your Hub only |

Legacy is the default because it is free, has the highest resolution, and needs nothing but a token. Tesla has said it will phase out this unofficial API; when that happens Hub tells you and offers paths 2 to 4. Path 2 is the one most people without a server will use. Paths 3 and 4 are for people who are comfortable running internet-facing services.

## How the companies do it without you owning a domain

Tesla's official data feed, Fleet Telemetry, is pushed by the car to a server. Tesla only sends it to an application that has registered a domain, hosts its public key on that domain, has a payment method on file, and whose "virtual key" the owner has installed in the car through the Tesla app. That is a lot of infrastructure for one car.

Tessie, Teslemetry and MyTeslaMate are such registered applications. When you sign in to one of them with your Tesla account, you authorise *their* application, install *their* virtual key (`https://tesla.com/_ak/teslemetry.com` and so on), and your car streams to *their* servers. They pay Tesla per signal and charge you a subscription. They then hand the data on to you: Teslemetry as Server-Sent Events, webhooks and a Fleet API proxy; Tessie as a WebSocket at `streaming.tessie.com/<VIN>` plus its REST API; MyTeslaMate as a proxy that speaks the old Owner API and a websocket that reproduces the old streaming feed (which is how TeslaMate itself uses it).

So "no domain needed" means "the provider owns the domain". Hub's provider mode connects outward from your computer to that provider; nothing on your network has to accept connections. Tesla allows a car to stream to at most five third-party applications at once (Teslemetry documents three), so a car already used with other services may need one removed.

Teslatlas itself is not a Tesla partner application and does not operate such a relay. That is a deliberate privacy choice: Magrathean never sees your car's data. You bring your own provider account, exactly as TeslaMate users do.

The Mac app shows this guide, with screenshots, as the Collection step of its setup, and again from the Collection screen later.

## Path 1: Legacy (Tesla Owner API)

**How it works.** Hub uses the same unofficial Owner API and streaming connection that TeslaMate has used for years, with a token pair from your Tesla account. The car streams position, speed and power every second while driving; Hub polls state at a slow cadence otherwise, and never wakes a sleeping car.

**What you need.** A Tesla access and refresh token. Hub does not take your Tesla password.

**Steps.**
1. In Hub, choose *Legacy* (the default) on the Collection screen.
2. Provide the token pair in one of two ways:
   - *Migrating from TeslaMate*: Hub reads TeslaMate's stored token during the migration (optional; Hub can also sign in with its own token so TeslaMate keeps working if you roll back).
   - *Fresh*: generate a token pair with a token tool such as "Auth app for Tesla" and paste it into Hub. Hub stores it encrypted and refreshes it automatically.
3. Hub shows the vehicles on the account. Pick the ones to record.
4. Wait for the first observation. Status shows "recording" and the last data time.

**What can go wrong.** Tesla may refuse the Owner API in future. Hub reports that as "Owner API refused" (not as a wrong password) and offers the other paths; your history stays. A refresh token is single-use: if another logger (TeslaMate) refreshes with the same chain, one of them is logged out. Run only one.

## Path 2: Provider (Teslemetry, MyTeslaMate, Tessie)

**How it works.** You authorise the provider's application with Tesla, install its virtual key, and Hub connects to the provider's API from your computer to receive the data. Hub treats the provider like Tesla: everything is stored locally and the provider token stays on your computer.

**What you need.** A provider account and its API token. Prices on 2026-10-08 (check the provider): Teslemetry $32.70 per vehicle per month including streaming and 500 command credits, extra on-demand polling by credit; MyTeslaMate pay-as-you-go API plan (its hosted TeslaMate plan is 4.99 € per vehicle per month); Tessie $6.99 to $19.99 per vehicle per month with Fleet Telemetry access on every plan.

**Steps (Teslemetry).**
1. Sign in at `teslemetry.com/login` with your Tesla account and grant *Vehicle Information* and *Vehicle Location* (commands are not needed for Hub).
2. In the Teslemetry console, select your vehicle and follow its instructions to install the virtual key (open `https://tesla.com/_ak/teslemetry.com` on the phone with the Tesla app). Pre-2021 Model S/X with MCU2 need *Settings > Safety > Allow Third-Party App Data Streaming* instead; MCU1 cars are not supported.
3. Open *Telemetry Configuration* and select the fields Hub needs (Hub shows the list on its Collection screen). Changes save automatically.
4. Create an access token in the console.
5. In Hub, choose *Provider > Teslemetry*, paste the token. Hub connects to `api.teslemetry.com` for vehicle data and to the SSE stream for telemetry. Status shows "receiving" once the first signal arrives.

**Steps (MyTeslaMate).**
1. Sign in at `app.myteslamate.com` with your Tesla account and open the *Fleet* page.
2. Pair your vehicle(s) there (it installs MyTeslaMate's virtual key through the Tesla app).
3. Copy the token from the Fleet page.
4. In Hub, choose *Provider > MyTeslaMate*, paste the token. Hub uses `https://api.myteslamate.com` for the Owner-API-compatible proxy and `wss://streaming.myteslamate.com` for the reproduced streaming feed, keyed by VIN.

**Steps (Tessie).**
1. Sign in at `dash.tessie.com` with your Tesla account and subscribe.
2. Create an access token under *Settings > Developer*.
3. In Hub, choose *Provider > Tessie*, paste the token. Hub connects to `streaming.tessie.com/<VIN>` for telemetry and to the Tessie REST API for state.

**What to expect.** Fleet Telemetry sends a field only when it changes, at the interval the provider configured, typically 1 to 60 seconds; Hub reconstructs drives and charges from that. Resolution is lower than Legacy streaming for fast-changing values. If the provider's subscription lapses or the virtual key is removed from the car, Hub shows "provider stopped" and keeps your history.

## Path 3: Direct Fleet polling (your own Tesla developer application)

**How it works.** You register your own application with Tesla, and Hub polls Tesla's Fleet API for vehicle state at a slow, consented cadence. No virtual key and no inbound connection are needed for polling, but registration still needs a web page you control.

**What you need.**
- A Tesla developer account at `developer.tesla.com` with a payment method and a billing limit set. Tesla charges per request (about $1 per 500 data requests) and gives a $10 monthly allowance; applications over their limit are disabled.
- A domain or site you control where a static file can be served over HTTPS at `/.well-known/appspecific/com.tesla.3p.public-key.pem` (a GitHub Pages site is commonly used for this).
- About an hour.

**Steps.**
1. Create the application at `developer.tesla.com`: name, the domain above as the allowed origin, redirect URI as Hub shows it.
2. Generate the key pair (`openssl ecparam -name prime256v1 -genkey -noout -out private-key.pem`; `openssl ec -in private-key.pem -pubout -out public-key.pem`) and publish the public key at the well-known path on your domain.
3. Register the application with Tesla's partner `register` endpoint for your region (Hub's *Direct Fleet* setup runs this call for you once you enter the client id, client secret and domain).
4. Add a payment method and a billing limit in the Tesla developer dashboard.
5. In Hub, choose *Direct Fleet polling*. Hub shows the consent screen: the cadence it will use (defaults: 30 s while driving, 120 s while charging, nothing while the car sleeps), the estimated monthly cost at Tesla's published rate, and a local monthly cap you set. Nothing is polled until you confirm.
6. Sign in with Tesla when Hub opens the authorisation page; grant *Vehicle Information* and *Vehicle Location*.
7. Hub shows the request counter and estimate on its status screen. If Tesla suspends the application for exceeding the billing limit, Hub says so; it never switches to another path on its own.

## Path 4: Self-hosted Fleet Telemetry

**How it works.** Your car streams directly to a Fleet Telemetry server you run next to Hub. This is the highest-quality official path and the hardest to set up.

**What you need.** Everything from path 3, plus: a public hostname inside your registered domain, a publicly trusted certificate for it, inbound TCP 443 forwarded to the Hub computer (no HTTP-terminating tunnel or CDN in front: the car does mutual TLS with the server itself), and the virtual key installed in the car.

**Steps.**
1. Complete path 3 steps 1 to 4.
2. Install the virtual key in the car by opening `https://tesla.com/_ak/<your-domain>` on the phone with the Tesla app.
3. In Hub, choose *Self-hosted Fleet Telemetry* and enter the hostname, certificate and key paths. Hub runs the Tesla `fleet-telemetry` server and the `tesla-http-proxy` as local helper processes and shows a reachability check for port 443.
4. Hub sends the telemetry configuration to the car through the proxy (signed with your private key) and shows when the car has accepted it.
5. Status shows "streaming" when the first frame arrives.

**What can go wrong.** If the port forward or certificate breaks, the car buffers about 40 minutes of data and then drops it; Hub shows the gap. If Tesla disables the application for exceeding the billing limit, the telemetry configuration is removed from the car and must be re-sent; Hub offers that after you raise the limit.

## Switching paths

Hub never switches collection path by itself. You change it on the Collection screen; Hub stops the old collector, confirms the new one is receiving, and records the switch in its status history. Only one path runs at a time per car. Your history is the same regardless of path; Hub records which path produced each period.

## Privacy summary

- Legacy, direct polling and self-hosted: only Tesla and your Hub handle your data.
- Provider: the provider's servers receive the car's stream and store at least a cache; read the provider's privacy policy. Hub keeps its own complete copy locally.
- Addresses: Hub looks up street addresses for drive and charge locations through a geocoding service you choose at setup (the public OpenStreetMap Nominatim service by default, with its usage policy shown). Elevation comes from downloaded terrain tiles; no coordinates are sent for it.
- Magrathean receives nothing.
