# Support

First checks for setup and connection problems, and where to report a bug.

This is a custom integration candidate. The recorded development baseline is Home Assistant `2026.8.3`; historical synthetic test results do not establish production support or compatibility with every Home Assistant release.

## First checks

| Symptom | Check |
| --- | --- |
| Integration does not appear | Confirm that the complete `teslatlas_hub` directory is under `/config/custom_components`, then restart Home Assistant. |
| Cannot connect | Check reachability from the Home Assistant runtime, the host and port, certificate trust and hostname. Container loopback refers to its host when using the supplied host-network example. |
| Certificate, pin or Hub identity mismatch | Stop and verify the intended Hub with its operator. Do not disable TLS validation to bypass the mismatch. |
| Invitation rejected | Request a fresh invitation. Expired or already-used invitation material cannot be reused. |
| Reauthentication requested | Use the entry's reauthentication flow with a fresh invitation. |
| Unknown value | The current profile may not supply that field or observation. Missing values are not converted to zero. |
| Unavailable entities | Check Hub reachability and whether the vehicle is still listed. Retries use bounded backoff. |

For certificate trust in a Container, see the [Container guide](../docs/guides/docker.md). Use Home Assistant's reconfigure flow for endpoint changes and keep the same Hub identity.

## Report a bug

Use the [repository issue tracker](https://github.com/magrathean-uk/teslatlas-home-assistant/issues). Include the component version or commit, Home Assistant version, installation type, architecture, reproduction steps and expected versus actual behavior. State whether the Hub data was synthetic or real without sharing it.

Review diagnostics before attaching them. Never attach `core.config_entries`, a configuration backup, tokens, invitations, vehicle identities, coordinates or raw Hub responses. Report security issues through [SECURITY.md](SECURITY.md).
