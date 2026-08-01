# Teslatlas Hub data contract

This is the public, versioned boundary between Hub and Teslatlas. It is not a
copy of the Hub catalogue, a TeslaMate database, or raw vehicle data.

## Wire contract v1

The paired HTTPS origin exposes only capabilities, paired vehicle list, signed
full-snapshot manifest, and immutable same-origin zstd SQLite packs. A
capability document declares `teslatlas-sync` protocol major 1, build version,
`sqlite-zstd` pack format, and the Ed25519 manifest-verifying key. Pairing is
one-use; all vehicle, manifest, and pack reads require the paired bearer.

Each manifest binds protocol and projection schema versions, installation,
account, vehicle, generation, snapshot, full-snapshot sequence interval,
ordered packs, exact totals, and opaque terminal cursor. The manifest signature
is over its exact bytes. Every pack is content-addressed by canonical lowercase
SHA-256 path and ETag, has bounded compressed/uncompressed size and expansion,
and carries application ID, SQLite user version, table list, row count, and the
same snapshot/sequence binding. The phone verifies signature, binding, digest,
pack limits, and SQLite identity before an atomic mirror swap.

The current typed projection schema is 2.0 and has only these tables:

| Table | Contract fields |
| --- | --- |
| `cars` | local car ID, name, model, VIN, firmware version, efficiency Wh/km |
| `drives` | identity, car, complete start/end, distance, duration, efficiency, temperature, speed, endpoint address/geofence/location/SOC/rated range |
| `positions` | identity, drive/car, UTC milliseconds, WGS84 location, speed/power, battery, elevation, odometer, ideal/rated range, climate and temperatures |
| `charges` | identity, car, start/end, energy added, SOC, duration, address/location/geofence, DC, rate/power, temperature, rated range |
| `charge_samples` | identity, charge, UTC milliseconds, battery, energy, electrical values, ranges, temperature, heater, fast-charger, and cable fields |

IDs are positive signed SQLite integers in the projection. Times are UTC
integer milliseconds. Locations are finite WGS84 coordinates. Numeric null is
unknown, not zero. The mirror contains one selected vehicle; raw observations,
credentials, pairing material, and unbounded third-party metadata never cross
this boundary.

## Parity extension contract

Complete parity data has four separately versioned families: immutable source
provenance and anomaly/gap records; availability and activity intervals plus
software updates; configuration/firmware, climate, door, tyre and optional
telemetry history; and derived terrain, geofence, address, energy, tariff, and
cost facts. Every record names source kind and stable source identity, source
record/observation IDs where present, observed/received time, field or formula
version, confidence/classification, and links to its input facts. Imported
TeslaMate source IDs and aggregates remain source facts; Hub derivations never
replace them.

New parity fields first enter a named optional extension schema. A phone reader
that does not understand it keeps the last compatible projection and reports
an upgrade requirement; it must never partially apply unknown data. A reader
release ships before an extension is published, and Hub serves the immediately
previous compatible schema for one complete app release transition. Removal,
type narrowing, semantic reuse, and changing a field's units require a new
major schema. Additive optional fields and tables use a new minor schema only
after old readers are proven to ignore them safely.

Every parity extension includes an explicit source/projection version and
recovery signal: complete, open, gap, delayed, conflict, unavailable, or
quarantined. Published projections are immutable. Rebuild, reprice, terrain,
geofence, or metadata refresh creates a later manifest and extension revision;
it never edits a historical pack. A cursor below a retained recovery floor gets
a new full snapshot, not a best-effort delta.

## Compatibility rules

Protocol major, schema major, pack format, signature algorithm, identity
binding, and cursor envelope are strict. Unknown major versions fail before
pack work. Unknown minor versions are accepted only when the reader declares
that optional extensions are safe to ignore. Manifest JSON rejects unknown
fields in v1; a wire-envelope change therefore requires a new protocol major
or a separately named capability.

The contract test corpus contains a valid manifest/pack, each current table,
null versus zero, every extension state, old-reader/new-Hub transition,
new-reader/old-Hub transition, bad signature/hash/ETag/range, missing pack,
wrong vehicle/account/generation, interrupted activation, cursor recovery,
and quarantine/gap cases. Differential TeslaMate fixtures remain the proof of
field and aggregate parity; transport tests alone are not parity evidence.
