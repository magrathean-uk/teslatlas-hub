# Viewer architecture

## Responsibility

Demonstrate the public Teslatlas Hub client shape with a deliberately modest,
independently buildable application.

The viewer is not a protocol authority. `teslatlas-protocol` owns public
contracts and the packaged `@teslatlas/sdk` owns browser transport. The viewer
maps its public `hub-http-v1@1.0.0` results and defines no route, transport
error shape, or credential format.

## Application boundary

```text
React views
    ↓ viewer-owned display model
ViewerDataSource
    ├── FixtureDataSource (local deterministic examples)
    └── SdkDataSource (@teslatlas/sdk/browser)
```

`ViewerDataSource` is intentionally narrow:

- discover candidate Hubs;
- pair a device while retaining its credential in memory;
- read one immutable viewer snapshot;
- remove one fixture paired device.

It is dependency inversion for this app, not a replacement SDK. Transport
details such as cursors, conditional requests, typed public errors, and
caller-owned credential storage remain the SDK's job. The SDK adapter is the
only network boundary. Views must not
construct URLs, call transport primitives, retain raw pairing material, merge
records across sources, or calculate private analytics.

## Product surface

| View | Public protocol proof |
| --- | --- |
| Hub health | Discovery identity, health, versions, and capabilities |
| Vehicles | Vehicle collection and visibility scopes |
| Current state | Current projection, freshness, absent values, and inference |
| Recent sessions | Bounded live drive pages; richer fixture charge summaries |
| Data quality | Fixture evidence; unsupported in the current live profile |
| Collector freshness | Fixture evidence; unsupported in the current live profile |
| Paired devices | Fixture removal and local live-session clearing |

## State semantics

Every active view owns one text-labelled state marker:

- **Loading:** the data source has not returned.
- **Empty:** the request completed with no applicable resources.
- **Absent:** the resource exists but one field was not reported.
- **Stale:** last-known values exist but exceed the fixture freshness boundary.
- **Inferred:** a named value was derived rather than observed.
- **Degraded:** data arrived with a visible quality or collection problem.
- **Offline:** the Hub or collection path is unreachable; last-known data is not
  presented as live.
- **Error:** the request failed and a retry is available.
- **Complete:** the fixture reports no unresolved problem for that view.

Colour supports these labels but never carries the meaning alone.
Session records may also carry **Partial** quality. They keep that exact label
and use a non-complete status cue.

Fixture states use explicit viewer-owned markers. The live adapter marks values
stale only when it retains a previous successful value after a failed route;
request timing alone is never evidence. A missing timestamp means unknown age.

## Data flow

1. `main.tsx` reads deterministic mode, scenario, and paired-state query inputs.
2. `createDataSource` selects the fixture source or SDK adapter.
3. `useViewer` owns abort-safe loading, ready, and error transitions.
4. `App` derives a state per active view from the immutable snapshot.
5. Each view renders only its slice and keeps quality evidence next to values.

Fixture reads return structured clones. A view cannot mutate future reads.
Paired-device removal updates only the current in-memory snapshot.

## Interaction safety

- Removing another fixture device requires a confirmation naming that device.
- A failed removal keeps the device visible, explains the failure, and permits
  retry; cancel restores focus to the initiating control.
- Clearing the local session removes viewer-held in-memory pairing state and
  returns to discovery. It does not claim to revoke a Hub device.
- Live drive cursors and conditional requests pass through the public SDK.
  Unsupported resources stay explicit and transient failures retain only the
  affected last-known values.

## Accessibility and responsive layout

- semantic header, main, complementary navigation, regions, lists, terms, and
  form controls;
- skip link, visible focus, keyboard-operable section buttons, and live status
  announcements;
- state text plus colour, never colour alone;
- reduced-motion styles;
- mobile-first reflow with horizontal navigation contained inside its own
  scroll area;
- automated axe checks and 320px viewport proof.

## Boundaries

The viewer uses public protocol concepts only. It cannot call an undocumented
Hub route. It contains no Hub implementation, proprietary Teslatlas source,
command dashboard, cloud deployment, or persisted credential policy.

See the [public SDK integration roadmap](public-sdk-integration-roadmap.md) for
the release-backed catalogue, fixture, and evidence gates.
