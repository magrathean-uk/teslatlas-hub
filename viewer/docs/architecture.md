# Viewer architecture

## Responsibility

Demonstrate the public protocol with a deliberately modest, independently buildable client.

## Product surface

| View | Public protocol proof |
| --- | --- |
| Hub health | Discovery and health capabilities |
| Vehicles | Vehicle list and visibility scopes |
| Current state | Current-state query and cache validation |
| Recent drives and charges | Historical cursors and detail resources |
| Data quality | Gap and projection-quality fields |
| Collector freshness | Event/query consistency |
| Paired-device management | Public pairing and device lifecycle |

## Non-goals

No Teslatlas advanced maps, Efficiency Lab, Charge Doctor, proprietary local analytics, full operator console, or commercial product styling.

## Boundaries

The viewer uses `teslatlas-sdk-typescript` and public `teslatlas-protocol` artifacts only. It cannot call an undocumented Hub route.
