# Data and retention

Teslatlas Hub processes precise location, journeys, charging, vehicle state,
vehicle identifiers, provider credentials, client pairing authority, and
security diagnostics.

## Stored data

- SQLite catalogue and bounded current state;
- immutable compressed history packs;
- encrypted provider credentials;
- device pairing and bearer records;
- bounded provider-response observations after credential-field stripping;
- bounded service and diagnostic logs;
- optional bounded terrain cache.

Raw processing rows are pruned after lifecycle projection. Hub is not designed
as an unlimited raw provider archive.

## Not collected by default

Hub does not require a Magrathean account, advertising identifier, analytics
SDK, Grafana, or MQTT broker. Self-hosted vehicle data does not reach
MAGRATHEAN UK LTD unless the operator deliberately sends support material or
uses a separately described Magrathean service.

## Operator duties

The operator controls retention and access. Consider drivers, employees,
family, passengers, lawful basis, notices, rights requests, access control,
backups, incident response, processor contracts, and employee-monitoring or
DPIA obligations.

See [PRIVACY.md](../PRIVACY.md) for the full deployment-role statement.

## Deletion

Stopping or uninstalling Hub does not delete history. The macOS uninstaller and
Debian package intentionally preserve data by default. Permanent deletion must
be a separate deliberate action after backup-retention obligations are checked.

Never attach a catalogue, pack, production log, VIN, coordinate, token, or
pairing payload to a public issue.
