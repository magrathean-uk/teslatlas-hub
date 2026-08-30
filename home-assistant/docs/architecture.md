# Home Assistant integration architecture

## Responsibility

Map public Teslatlas Hub data and events to Home Assistant devices and entities while following the applicable Home Assistant integration-quality requirements.

## First entity set

State of charge, charging state/power/limit, estimated range, odometer, awake state, temperatures, access state, software status, telemetry age, data quality, Fleet cost, collector health, and backup age are the first candidate entities.

## Integration behaviour

- UI config flow and public pairing-secret or QR claim flow.
- Zeroconf discovery where a Hub advertises a safe local endpoint.
- Public event-stream push; no periodic provider polling by Home Assistant.
- Reauthentication, reconfiguration, offline/reconnect, diagnostics, translations, and config-flow tests.
- Command services are absent by default and require separately approved scopes and confirmations.

## Boundaries

The integration does not read Hub databases, embed Hub collector logic, require Tesla credentials, or call private APIs.
