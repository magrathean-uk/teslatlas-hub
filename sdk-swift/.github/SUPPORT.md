# Support

Where to get help with tesatlas-sdk-swift, and what to include in a report.

Identify the product before investigating a problem:

| Product | Contract to check |
| --- | --- |
| `TesatlasHubSDK` and `TesatlasCommands` | Public protocol profiles through 1.2.0 |
| `TesatlasHubV1Compatibility` | Exact historical Hub v1.0.0 binding |
| `TesatlasCurrentHub` | Bundled `hub-http-v1@1.0.0` profile and admitted Hub product version |

A server may expose more routes than the selected client contract. Check [current Hub](../docs/reference/current-hub.md), [historical compatibility](../docs/reference/hub-v1-compatibility.md) and [versioning](../docs/reference/product-versioning.md) before changing validation to accept a response.

For non-sensitive bugs and questions, use the [repository issues](https://github.com/magrathean-uk/tesatlas-sdk-swift/issues).

## Prepare a useful report

Include the SDK revision, library, Swift version, operating system, Hub product/profile version, a small synthetic reproduction, expected behaviour and the typed error. Include the command and result if a local check fails. Redact tokens, invitations, private certificates, Hub and vehicle identifiers, location data and personal paths.

For a suspected vulnerability, follow [SECURITY.md](SECURITY.md) and keep exploit details out of public reports. No response-time or supported-release commitment is made here.
