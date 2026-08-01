# TeslaMate migration mapping v1

This is the field-level source contract for the pinned TeslaMate 4.1 adapter.
Every listed source field is captured as typed source evidence before any
projection. `NULL` remains unknown. Invalid IDs, timestamps, enum values,
non-finite numbers, broken references, or unit/type mismatch reject the
capture; none becomes zero, empty text, a guessed enum, or a rewritten source
row.

| Source table and fields | Hub destination | Rule |
| --- | --- | --- |
| `cars`: `id`, `eid`, `vid`, `vin`, `name`, `model`, `efficiency`, `trim_badging`, `marketing_name`, `exterior_color`, `wheel_type`, `spoiler_type`, `display_priority`, `inserted_at`, `updated_at` | source vehicle identity/configuration evidence; mirror `cars` name, model, VIN, efficiency | UUID derives from source namespace plus VIN, then EID only if VIN is absent. Name/model fallback is explicit; all other fields remain provenance/configuration extensions. |
| `drives`: `id`, `car_id`, start/end dates, start/end position/address/geofence IDs, average temperatures, speed/power extrema, ideal/rated ranges, odometer endpoints, distance, duration, ascent, descent | source drive; mirror `drives` and linked endpoint fields | Only completed selected-car drives publish. Distance/ranges are km, temperatures C, odometer km, duration minutes; source aggregates win. Unrepresented extrema/ascent/descent remain extension evidence. |
| `positions`: `id`, `car_id`, `drive_id`, date, latitude/longitude, elevation, speed, power, odometer, ideal/estimated/rated ranges, battery/SOC, heater flags, temperatures, fan/target temperatures, climate/defroster flags, four tyre pressures | source position; mirror `positions` | Only positions attached to a completed selected-car drive publish. Coordinates require finite WGS84; direct fields preserve units. Estimated range, heater, fan, targets, defrosters, tyre pressure remain versioned telemetry extensions. |
| `charging_processes`: `id`, `car_id`, position/address/geofence IDs, start/end dates, added/used energy, ideal/rated ranges, start/end SOC, duration, average temperature, cost | source charge process; mirror `charges` | Only completed selected-car processes publish. Energy is kWh, ranges km, temperature C, SOC percent; cost stays source cost evidence until a currency/scale contract exists. |
| `charges`: `id`, `charging_process_id`, date, heater flags, SOC, energy, current/phases/pilot/power/voltage, cable, fast-charger fields, ranges, heat-power flag, outside temperature | source charge sample; mirror `charge_samples` | Ordered by `(date,id)`. Electrical values retain source units; derived process energy is used only when source aggregate is absent and passes validation. |
| `addresses`: `id`, `display_name`, `name` | source address; flattened drive/charge address | Only selected-car references copy. `raw` geocoder payload is deliberately excluded and recorded as an intentional privacy/scope loss. |
| `geofences`: `id`, `name` | source geofence; flattened drive/charge geofence | Only selected-car references copy; no Hub spatial reinterpretation. |
| `states`: `id`, `car_id`, state, start/end dates | availability interval extension | `online`, `offline`, and `asleep` preserve source meaning; unknown enum is raw evidence plus explicit unknown, never a guessed state. |
| `updates`: `id`, `car_id`, start/end dates, version | software-update extension; latest version may populate mirror car | Empty version is unknown. Latest completed update is ordered by end/start/ID. |
| `settings`: length/temperature/pressure units, preferred range, base/grafana URL, language, theme mode, timestamps | global settings provenance | Unit preferences annotate source presentation only; URLs and web/UI settings do not become Hub endpoints or controls. |
| `car_settings`: suspend thresholds, unlock requirement, supercharging, streaming API, enabled, LFP battery | per-vehicle settings provenance | Settings are outcome evidence, not copied control configuration; Hub does not enact them. |

Timestamps decode in the source UTC session and normalize to non-negative Unix
milliseconds; sub-millisecond precision is retained in source evidence only.
PostgreSQL `numeric` decodes losslessly until the named destination conversion;
the current mirror uses finite binary64 only where its contract declares it.
Every capture records adapter revision, source schema fingerprint, input table
counts, field-loss reasons, and canonical row hashes. Bulk staged inserts use
bounded prepared batches, enforce relationships before publication, then build
nonessential destination indexes after bulk load but before integrity/seal.
