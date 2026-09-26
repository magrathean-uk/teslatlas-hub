# Support

For a reproducible SDK defect, use the repository's [issue tracker](https://github.com/magrathean-uk/teslatlas-sdk-typescript/issues) if it is available to you. This documentation does not promise a response time or a supported-release window. For private security concerns, use the organization reporting route described in [SECURITY.md](SECURITY.md), rather than the issue tracker.

Include the SDK version and source commit or package digest, Node/npm versions, browser and operating system, the factory used (`createClient` or `createHubClient`), the failing method, and a minimal reproduction with synthetic data. Include the typed error code and validator name when present.

Never attach authorization headers, invitations, credentials, private keys, raw Hub descriptors, vehicle identifiers, or location history. Reproduce with local fixtures where possible.

Before reporting, check [compatibility](docs/compatibility.md), [API behavior](docs/api.md), and [historical acceptance boundaries](docs/acceptance.md). A browser pairing error of `hub_tls_pin_unavailable` is expected without a caller-provided pin-capable transport. A local `logout()` does not revoke a device at the Hub.
