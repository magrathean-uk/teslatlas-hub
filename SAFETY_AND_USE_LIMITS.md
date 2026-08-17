# Safety and use limits

Teslatlas Hub is a telemetry and data-analysis system. It is not a vehicle control or safety system.

Do not rely on it for:

- emergency response;
- determining whether a vehicle is safe to drive;
- autonomous or remote vehicle control;
- battery, charging or electrical safety decisions;
- warranty, insurance, resale or legal evidence without independent verification;
- billing or taxation where certified accuracy is required;
- preservation of forensic evidence;
- medical, life-support or other high-risk use.

Telemetry can be delayed, incomplete, duplicated, corrupted, mapped to the wrong time zone or interpreted incorrectly.

Battery health, efficiency, charging, cost, route and parked-drain outputs are informational estimates. Obtain a qualified inspection before making a safety, repair, warranty or financial decision.

Never expose fake-source, test or debug endpoints in production. Do not run Hub and another collector against the same credential where concurrent refresh can invalidate access.

Maintain independent backups and test restoration before migration or upgrade.
