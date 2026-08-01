# State History Projection Contract

This contract preserves TeslaMate `states` history in Teslatlas Hub without
changing the existing projection schema `2.0`.

## Source facts

Each state row contains:

- `id`: required source row identifier
- `car_id`: required source car identifier
- `state`: one of `online`, `offline`, or `asleep`
- `start_date`: required UTC timestamp
- `end_date`: optional UTC timestamp

Rows are ordered by ascending `id`. A car has at most one open row, where
`end_date` is absent. A closed row must not end before it starts.

## Projection rules

- Hub preserves every source field and source order.
- Timestamps use the Hub-wide millisecond UTC representation. Source
  microsecond precision is intentionally rounded down to milliseconds.
- Hub rejects a state value outside the known source enum.
- An empty state list is valid.
- An open state is valid and retains a missing `end_date`.

## Version rule

Schema `2.0` remains unchanged.

Schema `2.1` adds an ordered `states` section. Hub selects `2.1` only when the
client explicitly advertises it in `X-Teslatlas-Supported-Schemas`. A request
without that header receives `2.0`.

The first `2.1` consumer must prove that it preserves the ordered rows and open
state form before Hub advertises `2.1` in production.
